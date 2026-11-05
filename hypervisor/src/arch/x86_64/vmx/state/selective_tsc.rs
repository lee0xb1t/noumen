//! 选择性 TSC 隐藏：只对指定进程/内核模块隐藏 VM-exit 开销。
//!
//! 与全系统 TSC offsetting 的区别：`TSC_OFFSET` 是 per-vCPU 的，多核各自扣减
//! host 时间会累积时钟差，Windows 的 HAL 定时器跨核比较时直接 `0x5C`。这里改成
//! **拦 RDTSC + 只在读的时候做减法**：
//!
//! - 每个 vCPU 记账 `host_time` = 累计的 exit→entry（root 执行）时间；
//! - `RDTSC` 退出时，若读的进程/模块是目标，返回 `物理 TSC - host_time`；
//!   否则返回物理 TSC（多核天然一致，因为是同一个物理计数器）。
//!
//! 目标配置放在全局原子变量里：`RDTSC` 退出发生在 root，这里不能拿锁（可能和
//! 别的核的配置路径死锁）。

use core::sync::atomic::{AtomicBool, AtomicU32, AtomicU64, Ordering};

use crate::klog;

// ---------------------------------------------------------------- 全局目标配置

static TARGET_PID: AtomicU32 = AtomicU32::new(0);
static MODULE_BASE: AtomicU64 = AtomicU64::new(0);
static MODULE_SIZE: AtomicU64 = AtomicU64::new(0);

/// 目标 PID 生效的模式；默认两个特权级都算。
static TARGET_MODE: AtomicU32 = AtomicU32::new(TARGET_MODE_BOTH);

/// 目标模式：只隐藏 CPL0（内核代码）的 `RDTSC`。
pub const TARGET_MODE_KERNEL: u32 = 0;
/// 目标模式：只隐藏 CPL3（用户代码）的 `RDTSC`。
pub const TARGET_MODE_USER: u32 = 1;
/// 目标模式：CPL0 和 CPL3 都隐藏。
pub const TARGET_MODE_BOTH: u32 = 2;

/// 每次配置变化 +1。每个 vCPU 发现自己落后时更新 VMCS 的 `rdtsc_exiting`。
static CONFIG_GENERATION: AtomicU32 = AtomicU32::new(0);

/// 每次**命中目标的退出**额外记到账上的 tick 数（VM-exit 转换本身，root 侧测不到）。
/// `0` = 不补偿。由调用方实测后经 `0x204` 下发，上限见 [`MAX_EXIT_OVERHEAD`]。
static EXIT_OVERHEAD: AtomicU64 = AtomicU64::new(0);

/// 补偿上限：单次转换不可能到这个量级，超了就是单位填错。
pub const MAX_EXIT_OVERHEAD: u64 = 1_000_000;

/// 退出时间戳与 handler 里 `rdtsc` 之间允许的最大差值（约 1 ms）；
/// 超过说明 MSR-store 里的值不是本次退出写的。
pub const MAX_EXIT_TSC_LAG: u64 = 3_000_000;

/// 设置每次命中退出的补偿值；`0` 关闭补偿。
pub fn set_exit_overhead(ticks: u64) -> Result<(), u64> {
    if ticks > MAX_EXIT_OVERHEAD {
        return Err(ticks);
    }

    EXIT_OVERHEAD.store(ticks, Ordering::Release);

    klog!(
        "TSC: exit overhead = {} ticks ({})",
        ticks,
        if ticks == 0 { "off" } else { "compensating" }
    );

    Ok(())
}

/// 当前补偿值（每次退出都会读，用 relaxed 就够）。
pub fn exit_overhead() -> u64 {
    EXIT_OVERHEAD.load(Ordering::Relaxed)
}

// ------------------------------------------------------- VM-exit MSR-store 区

const PAGE_SIZE: usize = 4096;
const ENTRY_BYTES: usize = 16;
/// MSR-store 记录条数：`IA32_TSC` + `IA32_KERNEL_GS_BASE`。
pub const MSR_STORE_ENTRIES: usize = 2;

/// `IA32_TSC`。
const IA32_TSC: u32 = 0x10;

/// `IA32_KERNEL_GS_BASE`：用户态时它保存 KPCR（内核 `swapgs` 的另一半）。
///
/// 用户态 `GS_BASE` 是 TEB，所以 CPL3 下走内核结构链必须先从这个 MSR 里取 KPCR。
pub const IA32_KERNEL_GS_BASE: u32 = 0xC000_0102;

/// VM-exit MSR-store 区域：一页两条记录（`IA32_TSC` + `IA32_KERNEL_GS_BASE`）。
/// 记录布局按 SDM：`u32 index | u32 reserved | u64 data`，16 字节一条，整页 16 字节对齐。
/// 两个值都由**硬件在退出转换时**写，比 handler 里再读更早、也不依赖 root 有没有碰过这些 MSR。
#[repr(C, align(16))]
pub struct MsrStoreArea {
    entries: [MsrStoreEntry; MSR_STORE_ENTRIES],
    pad: [u8; PAGE_SIZE - ENTRY_BYTES * MSR_STORE_ENTRIES],
}

#[repr(C)]
struct MsrStoreEntry {
    index: u32,
    reserved: u32,
    data: u64,
}

impl MsrStoreArea {
    pub fn init(&mut self) {
        self.entries[0].index = IA32_TSC;
        self.entries[0].reserved = 0;
        self.entries[0].data = 0;
        self.entries[1].index = IA32_KERNEL_GS_BASE;
        self.entries[1].reserved = 0;
        self.entries[1].data = 0;
    }

    /// 最近一次 VM-exit 的物理 TSC（`read_volatile`：内存是硬件写的）。
    pub fn exit_tsc(&self) -> u64 {
        unsafe { core::ptr::read_volatile(&self.entries[0].data) }
    }

    /// 最近一次 VM-exit 时 `IA32_KERNEL_GS_BASE` 的快照（guest 的值，CPL3 时是 KPCR）。
    pub fn exit_kernel_gs_base(&self) -> u64 {
        unsafe { core::ptr::read_volatile(&self.entries[1].data) }
    }
}

/// 一次配置快照（一致性由 generation 保证：读两次 generation 相同才算数）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct TargetSnapshot {
    pub pid: u32,
    /// [`TARGET_MODE_KERNEL`] / [`TARGET_MODE_USER`] / [`TARGET_MODE_BOTH`]。
    pub mode: u32,
    pub module_base: u64,
    pub module_size: u64,
    pub generation: u32,
}

/// 取一份一致的配置快照。
pub fn snapshot() -> TargetSnapshot {
    loop {
        let before = CONFIG_GENERATION.load(Ordering::Acquire);

        let snapshot = TargetSnapshot {
            pid: TARGET_PID.load(Ordering::Relaxed),
            mode: TARGET_MODE.load(Ordering::Relaxed),
            module_base: MODULE_BASE.load(Ordering::Relaxed),
            module_size: MODULE_SIZE.load(Ordering::Relaxed),
            generation: before,
        };

        if CONFIG_GENERATION.load(Ordering::Acquire) == before {
            return snapshot;
        }
    }
}

/// 是否有任何隐藏目标（决定是否打开 `rdtsc_exiting`）。
pub fn hiding_active() -> bool {
    TARGET_PID.load(Ordering::Acquire) != 0 || MODULE_SIZE.load(Ordering::Acquire) != 0
}

pub fn generation() -> u32 {
    CONFIG_GENERATION.load(Ordering::Acquire)
}

fn bump_generation() {
    CONFIG_GENERATION.fetch_add(1, Ordering::AcqRel);
}

/// 设置目标进程 PID 和生效模式；`pid == 0` 表示清除。
///
/// `mode` 必须是 [`TARGET_MODE_KERNEL`]/[`TARGET_MODE_USER`]/[`TARGET_MODE_BOTH`]，
/// 校验交给 hypercall 入口（`0x200`）。
pub fn set_target_pid(pid: u32, mode: u32) {
    TARGET_PID.store(pid, Ordering::Release);
    TARGET_MODE.store(mode, Ordering::Release);
    reset_ledgers();
    bump_generation();

    klog!(
        "TSC: target pid = {} mode = {} ({})",
        pid,
        mode_name(mode),
        if pid == 0 { "cleared" } else { "active" }
    );
}

/// 模式的可读名（日志用）。
pub fn mode_name(mode: u32) -> &'static str {
    match mode {
        TARGET_MODE_KERNEL => "kernel",
        TARGET_MODE_USER => "user",
        _ => "kernel+user",
    }
}

/// 设置目标内核模块范围；`size == 0` 表示清除。
pub fn set_target_module(base: u64, size: u64) {
    if size == 0 {
        MODULE_BASE.store(0, Ordering::Release);
        MODULE_SIZE.store(0, Ordering::Release);
    } else {
        MODULE_BASE.store(base, Ordering::Release);
        MODULE_SIZE.store(size, Ordering::Release);
    }

    reset_ledgers();
    bump_generation();

    klog!("TSC: target module base={:#x} size={:#x}", base, size);
}

impl TargetSnapshot {
    pub fn has_pid(&self) -> bool {
        self.pid != 0
    }

    /// 这个特权级是否在目标模式的范围内。
    ///
    /// 只隐藏用户态（`mode == 1`）时，内核态的退出连 PID 都不用读——那是每次退出
    /// 三跳 guest 页表翻译，白白拖慢系统。
    pub fn allows_cpl(&self, cpl: u8) -> bool {
        match self.mode {
            TARGET_MODE_KERNEL => cpl == 0,
            TARGET_MODE_USER => cpl == 3,
            _ => true,
        }
    }

    pub fn has_module(&self) -> bool {
        self.module_size != 0
    }

    pub fn contains_rip(&self, rip: u64) -> bool {
        self.has_module() && rip >= self.module_base && rip < self.module_base.wrapping_add(self.module_size)
    }
}

// ---------------------------------------------------------------- 每 vCPU 记账

/// 一次「是否命中隐藏目标」的判定结果。
///
/// 退出记账（`dispatch_vm_exit`）和 `RDTSC` 处理都要用同一个判定，所以由 dispatch
/// 算一次存进 [`SelectiveTsc`]，`RDTSC` 处理直接复用：既保证「记账按 A 判、返回值
/// 按 B 判」不会漂移，也避免一次退出把 guest 页表走两遍（那几跳解引用很贵）。
#[derive(Debug, Clone, Copy)]
pub struct TargetMatch {
    pub by_pid: bool,
    pub by_module: bool,
    pub cpl: u8,
    /// guest CS selector 原始值（诊断用：`& 3` 就是 CPL）。
    pub cs: u16,
    pub rip: u64,
    pub pid: Option<u32>,
    /// 命中时是哪个线程（KTHREAD 指针），账本按它分账；没命中是 `None`。
    pub thread: Option<u64>,
}

impl TargetMatch {
    pub fn matched(&self) -> bool {
        self.by_pid || self.by_module
    }
}

/// 虚拟时钟：全局唯一一份 `(上次物理 TSC, 上次可见值)`。
/// 不按线程分槽——键不稳定会让两个尺度互相跳（历史见 AGENTS.md §9.8）。
static CLOCK_PHYS: AtomicU64 = AtomicU64::new(0);
static CLOCK_VISIBLE: AtomicU64 = AtomicU64::new(0);
/// 保护上面两个变量的配对更新（root 里不能拿阻塞锁，用短自旋）。
static CLOCK_LOCK: AtomicBool = AtomicBool::new(false);
/// 累计记到账上的 host 时间，只用于日志诊断。
static HOST_DIAG: AtomicU64 = AtomicU64::new(0);

fn clock_acquire() -> bool {
    let mut spins = 0u32;

    while CLOCK_LOCK
        .compare_exchange_weak(false, true, Ordering::Acquire, Ordering::Relaxed)
        .is_err()
    {
        core::hint::spin_loop();
        spins = spins.wrapping_add(1);
    }

    spins != u32::MAX
}

fn clock_release() {
    CLOCK_LOCK.store(false, Ordering::Release);
}

/// 目标时钟是否已经建立过基准。
pub fn clock_started() -> bool {
    CLOCK_PHYS.load(Ordering::Acquire) != 0
}

/// 是否在每次命中时打日志（`0x206` 可改，默认开）。
/// 一条日志 12000~45000 tick，落在测量窗口里会直接抬高读数——测真实开销前关掉它。
static HIT_LOG: AtomicBool = AtomicBool::new(true);

/// 设置是否记录命中日志。
pub fn set_hit_log(enabled: bool) {
    HIT_LOG.store(enabled, Ordering::Release);

    klog!("TSC: hit log {}", if enabled { "on" } else { "off" });
}

/// 当前是否记录命中日志。
pub fn hit_log() -> bool {
    HIT_LOG.load(Ordering::Relaxed)
}

/// 目标看到的时钟相对物理 TSC 的默认除数。
///
/// 开 VT 后 `cpuid`/`rdtsc` 要多绕一次 VM-exit，目标量到的差值比裸机大一个数量级，
/// 所以直接把「流逝时间」除以这个数：300 tick 的差变成 30，和裸机同量级。
/// 运行时可以用 `0x205` 改成别的值。
pub const TSC_SLOWDOWN_DIVISOR: u32 = 10;

/// 除数的上限：再大就只是在伪造一个停摆的时钟了。
pub const MAX_DIVISOR: u64 = 1_000_000;

/// 当前除数（`0x205` 可改，默认 [`TSC_SLOWDOWN_DIVISOR`]）。
static TSC_DIVISOR: AtomicU32 = AtomicU32::new(TSC_SLOWDOWN_DIVISOR);

/// 设置可见时钟的除数；`1` 表示不减速。
pub fn set_divisor(divisor: u64) -> Result<(), u64> {
    if divisor == 0 || divisor > MAX_DIVISOR {
        return Err(divisor);
    }

    TSC_DIVISOR.store(divisor as u32, Ordering::Release);

    klog!("TSC: divisor = {} (visible clock advances 1/{} per tick)", divisor, divisor);

    Ok(())
}

/// 当前除数。
pub fn divisor() -> u64 {
    let divisor = TSC_DIVISOR.load(Ordering::Relaxed);

    if divisor == 0 {
        TSC_SLOWDOWN_DIVISOR as u64
    } else {
        divisor as u64
    }
}

/// 给账本加一段 host 时间（只用于日志诊断，任何核都可以加）。
fn ledger_add(key: u64, delta: u64) {
    if key == 0 || delta == 0 {
        return;
    }

    HOST_DIAG.fetch_add(delta, Ordering::AcqRel);
}

/// 清空虚拟时钟：换目标、关隐藏时调用，避免把旧目标的基准带进来。
pub fn reset_ledgers() {
    clock_acquire();

    CLOCK_PHYS.store(0, Ordering::Release);
    CLOCK_VISIBLE.store(0, Ordering::Release);
    HOST_DIAG.store(0, Ordering::Release);

    clock_release();
}

/// 每个 vCPU 一份：按线程累计 root 执行的时间，并在读 TSC 时减掉。
#[derive(Debug, Clone, Copy)]
pub struct SelectiveTsc {
    /// 本 vCPU 已经应用到 VMCS 的配置代数。
    pub applied_generation: u32,
    /// 硬件 VM-exit MSR-store 已经验证可用（读回计数 == 1）。
    ///
    /// 为假时退出时间戳退回 handler 里的 `rdtsc`（精度差几百 tick，但不会算错）。
    pub msr_store: bool,
    /// 上一次 VM-exit 的物理 TSC；`0` 表示还没记过。
    last_exit_tsc: u64,
    /// 上一次「命中」退出属于哪个线程；`0` = 上一次没命中。
    last_thread: u64,
    /// 本次退出已经算好的目标判定，由 dispatch 写入、`RDTSC` 处理读走。
    current: Option<TargetMatch>,
    /// 退出总数（诊断用：判断两次隐藏读之间发生了哪些退出）。
    exits: u64,
    /// 其中判定为目标的退出数。
    matched_exits: u64,
    /// 隐藏值出现回退的次数（补偿填过头时会这样：每次退出补的比实际转换还多）。
    rewind: u64,
    /// 退出时间戳被判为陈旧（MSR-store 没写/值不前进）的次数，诊断用。
    stale_exits: u64,
    /// 本段 guest 运行了多久（`exit - entry`），诊断用。
    ///
    /// 命中退出只扣 handler，这段**故意保留**给目标自己（那是它在跑）。所以一旦这段
    /// 里混进了「没有 VM exit 的 guest 时间」（guest 自己的 ISR/DPC、被 guest 调度器
    /// 切走），它就会整段漏进目标的读数——`guest=` 就是用来一眼看出这一点的。
    pub guest_run: u64,
    /// 本次退出实际扣掉的 root 窗口（`0` = 没扣），诊断用。
    pub last_window: u64,
}

impl SelectiveTsc {
    pub const fn new() -> Self {
        Self {
            applied_generation: 0,
            msr_store: false,
            last_exit_tsc: 0,
            last_thread: 0,
            current: None,
            exits: 0,
            matched_exits: 0,
            rewind: 0,
            stale_exits: 0,
            guest_run: 0,
            last_window: 0,
        }
    }

    /// VMCS 里的 MSR-store 记录已经写入并读回验证。
    pub fn enable_msr_store(&mut self) {
        self.msr_store = true;
    }

    pub fn uses_msr_store(&self) -> bool {
        self.msr_store
    }

    /// 每次 VM-exit 调一次：只把上一个 handler 的 root 段（`entry - last_exit`）
    /// 记到 `last_thread` 的账上；guest 段如实透出（TSC 是墙钟）。
    pub fn note_exit(
        &mut self,
        exit_tsc: u64,
        entry_tsc: u64,
        handler_tsc: u64,
        active: bool,
        thread: Option<u64>,
    ) {
        if !active {
            self.last_exit_tsc = 0;
            self.last_thread = 0;
            return;
        }

        // **窗口起点一律用 handler 自己的 `rdtsc`，不信硬件 MSR-store 的值。**
        //
        // 这是踩了很多次才定下来的：MSR-store 里的 `IA32_TSC` 在嵌套/被 host 抢占时
        // 可能比真实退出早很多（甚至就是一两个退出之前的值），拿它当「上一个 handler
        // 的起点」会把并不属于 handler 的时间一起扣掉。真机表现就是样本在「0」和
        // 「几万」之间乱跳：多扣一点就被单调钳制压成 0，少扣一点就冒出 ~25000。
        //
        // 用 `handler_tsc`（handler 进来时自己读的 rdtsc）当起点，窗口 = 上个 handler
        // 从「开始执行」到「即将 vmresume」这一段，**必然落在 handler 内部**，永远
        // 不会多扣；少算的只有 guest→root 那次转换（~150 tick），那个由 `0x204` 补。
        if exit_tsc > handler_tsc || handler_tsc.wrapping_sub(exit_tsc) > MAX_EXIT_TSC_LAG {
            self.stale_exits = self.stale_exits.wrapping_add(1);

            if self.stale_exits == 1 {
                klog!(
                    "TSC: exit tsc {:#x} vs handler {:#x} (lag {}), windowing on handler rdtsc",
                    exit_tsc,
                    handler_tsc,
                    handler_tsc.wrapping_sub(exit_tsc)
                );
            }
        }

        let exit_tsc = handler_tsc;

        self.exits = self.exits.wrapping_add(1);

        if thread.is_some() {
            self.matched_exits = self.matched_exits.wrapping_add(1);
        }

        let had_baseline = self.last_exit_tsc != 0;

        // 1) 上一个 handler 的 root 时间，按线程入账。
        //
        // 窗口 = `entry_tsc - last_exit_tsc`，其中两头都是本 vCPU 自己记的：
        // `entry_tsc` 是 stub 在 vmresume 之前写的，`last_exit_tsc` 是上一个 handler
        // 进来时读的，所以这段**完全落在上一个 handler 内部**。
        if had_baseline && self.last_thread != 0 {
            let window = entry_tsc.wrapping_sub(self.last_exit_tsc);

            // 最高位为 1 = 物理 TSC 倒退（迁移/异常），这种窗口没有意义。
            if window >> 63 == 0 {
                self.last_window = window;
                ledger_add(self.last_thread, window);
            } else {
                self.last_window = 0;
            }
        } else {
            self.last_window = 0;
        }

        // 只记 root 时间。**不需要**再单独处理 guest 段：
        //
        // 早先在「单特权级模式」下，会给「未命中的周期」把整段 guest 时间
        // （`exit - last_exit`）扣到**所有**已建账线程头上，本意是「时钟只在目标
        // 执行时前进」（CPU 时间语义）。但那个做法有两个致命问题：
        //
        // 1. TSC 本来是**墙钟**，不是 CPU 时间——线程被换出时真实 TSC 也在走，
        //    把它冻掉反而不像真机；
        // 2. 更糟的是**多核下会重复累加**：每个核上「非目标退出」的间隙都被加到所有
        //    目标线程的账上。12 个核各自跑满 ⇒ 每过 1 秒就给每个线程记 12 秒，
        //    账本永远追不上物理时间，可见值被永久钳死（真机现象：样本从几千变成
        //    全 0，时钟不动了）。
        //
        // 所以只扣「命中退出自己的 handler」这一段，其余时间一律如实透出。
        if let Some(kthread) = thread {
            ledger_add(kthread, EXIT_OVERHEAD.load(Ordering::Relaxed));
        }

        self.last_exit_tsc = exit_tsc;
        self.last_thread = thread.unwrap_or(0);
    }

    /// 退出时间戳被判陈旧的次数（诊断用）。
    pub fn stale_count(&self) -> u64 {
        self.stale_exits
    }

    /// 记录本次退出的目标判定（dispatch 每次退出调一次）。
    pub fn set_match(&mut self, matched: Option<TargetMatch>) {
        self.current = matched;
    }

    /// 取本次退出的目标判定。
    pub fn current_match(&self) -> Option<TargetMatch> {
        self.current
    }

    /// 可见值 = 上次可见值 + (物理差) / 除数；基准由第一次读建立，那次直接返回物理值。
    ///
    /// 用比例而不是「减掉 host 时间」：只做加法所以天然单调，残留误差同比缩小，
    /// 也不依赖任何时间戳（理由见 AGENTS.md §9.8）。代价是目标看到慢 N 倍的时钟。
    ///
    /// 返回 `(可见值, 本次推进量, 累计 host 时间[诊断])`；`_kthread` 只是日志标签。
    pub fn hide(&mut self, physical: u64, _kthread: u64) -> (u64, u64, u64) {
        clock_acquire();

        let last_physical = CLOCK_PHYS.load(Ordering::Acquire);
        let last_visible = CLOCK_VISIBLE.load(Ordering::Acquire);

        // 第一次读：建立基准，原样返回物理值。
        if last_physical == 0 || last_visible == 0 {
            CLOCK_PHYS.store(physical, Ordering::Release);
            CLOCK_VISIBLE.store(physical, Ordering::Release);

            let host = HOST_DIAG.load(Ordering::Acquire);

            clock_release();

            return (physical, 0, host);
        }

        // 物理值没有前进（跨核 TSC 的微小偏移、或者同一时刻的两次读）：**保留上一次的
        // 可见值**，不前进也不跳变。
        //
        // 早先这里 `CLOCK_VISIBLE.store(physical)` 并返回物理值——那会把可见时钟一次
        // 抬高整整一段 drift（实测 1.16e9 tick），下一次读又回到慢速曲线，两次读的
        // 差值就成了 −1.16e9（`rdtsc_probe` 里那个 18446744072541936881）。
        if physical <= last_physical {
            let host = HOST_DIAG.load(Ordering::Acquire);

            clock_release();

            return (last_visible, 0, host);
        }

        let elapsed = physical - last_physical;

        // 兜底：差值小于除数时按原值前进，不做整除。
        //
        // 否则 `Δ/N` 会把它抹成 0——两次紧挨着的读会看到完全相同的值（时钟"冻住"），
        // 这本身就是个异常特征；而且目标做小步长自增计时的时候会卡住。
        let divisor = divisor().max(1);

        let advance = if elapsed < divisor {
            elapsed
        } else {
            elapsed / divisor
        };

        let visible = last_visible.wrapping_add(advance);

        CLOCK_PHYS.store(physical, Ordering::Release);
        CLOCK_VISIBLE.store(visible, Ordering::Release);

        let host = HOST_DIAG.load(Ordering::Acquire);

        clock_release();

        (visible, advance, host)
    }

    /// 虚拟时钟是否已经建立（诊断用，1 = 已建立）。
    pub fn thread_count(&self) -> u32 {
        u32::from(clock_started())
    }

    /// (退出总数, 其中命中目标的退出数)，诊断用。
    pub fn exit_counters(&self) -> (u64, u64) {
        (self.exits, self.matched_exits)
    }

    /// 隐藏值回退次数，诊断用：不为 0 说明 `0x204` 的补偿值填大了。
    pub fn rewind_count(&self) -> u64 {
        self.rewind
    }

    /// 关闭隐藏时清掉本核的记账基准（全局账本由 `reset_ledgers` 统一清）。
    pub fn reset(&mut self) {
        self.last_exit_tsc = 0;
        self.last_thread = 0;
        self.current = None;
        self.exits = 0;
        self.matched_exits = 0;
        self.rewind = 0;
    }
}

impl Default for SelectiveTsc {
    fn default() -> Self {
        Self::new()
    }
}

// ---------------------------------------------------------------- 进程/模块判定

/// 读 guest 虚拟地址的 8 字节（内核上下文，走 guest CR3 翻译 + host 直接映射）。
pub fn read_guest_u64(cr3: u64, address: u64) -> Option<u64> {
    let phys = crate::arch::x86_64::guest_memory::translate_guest(cr3, address, false, false, false)
        .ok()?;
    let virt = crate::arch::x86_64::paging::phys_to_virt(crate::arch::x86_64::addr::PhysAddr::new(
        phys,
    ));

    Some(unsafe { core::ptr::read_unaligned(virt.as_u64() as *const u64) })
}

/// 读 guest 虚拟地址的 4 字节。
pub fn read_guest_u32(cr3: u64, address: u64) -> Option<u32> {
    let phys = crate::arch::x86_64::guest_memory::translate_guest(cr3, address, false, false, false)
        .ok()?;
    let virt = crate::arch::x86_64::paging::phys_to_virt(crate::arch::x86_64::addr::PhysAddr::new(
        phys,
    ));

    Some(unsafe { core::ptr::read_unaligned(virt.as_u64() as *const u32) })
}

/// `KPCR.Prcb.CurrentThread`：`PsGetCurrentProcess` 就是 `gs:[0x188]` 再取 `+0xB8`。
/// Win7→24H2 未变；`tools/gen_win_offsets.py` 解析导出时会断言它没漂。
pub const KPCR_CURRENT_THREAD: u32 = 0x188;

/// 取当前上下文的 KPCR：`CPL0` 时 `gs` 就是它，`CPL3` 时在 `IA32_KERNEL_GS_BASE`。
fn kpcr_base(cpl: u8) -> Option<u64> {
    if cpl == 3 {
        Some(unsafe { x86::msr::rdmsr(IA32_KERNEL_GS_BASE) })
    } else {
        Some(crate::arch::x86_64::vmx::vmcs::guest::GsBase::read().ok()?.as_raw())
    }
}

/// 当前线程的 KTHREAD 指针：`[KPCR + 0x188]`。翻译用 `system_cr3`（KVA shadow 下
/// 用户 CR3 没映射内核半区）；指针不在内核半区按读不到处理。
pub fn current_kthread(cpl: u8) -> Option<u64> {
    let cr3 = crate::hv::GLOBAL_ARGS.get().system_cr3 as u64;
    let kpcr = kpcr_base(cpl)?;

    read_guest_u64(cr3, kpcr.wrapping_add(KPCR_CURRENT_THREAD as u64))
        .filter(|kthread| *kthread >= 0xffff_8000_0000_0000)
}

/// 当前进程 ID：`[KPCR+0x188]` → KTHREAD，`[+kthread_process]` → EPROCESS，
/// `[+eproc_pid]` → PID。两次解引用缺一不可（`gs` 是 KPCR 不是 KTHREAD）。
pub fn current_process_id(
    cr3: u64,
    kpcr: u64,
    offsets: &crate::kargs::WinOffsets,
) -> Option<u32> {
    if !offsets.is_valid() {
        return None;
    }

    let kthread = read_guest_u64(cr3, kpcr.wrapping_add(KPCR_CURRENT_THREAD as u64))?;

    if kthread < 0xffff_8000_0000_0000 {
        return None;
    }

    let eprocess = read_guest_u64(cr3, kthread.wrapping_add(offsets.kthread_process as u64))?;

    // 内核指针必须落在内核半区，否则说明翻译到了垃圾数据。
    if eprocess < 0xffff_8000_0000_0000 {
        return None;
    }

    read_guest_u32(cr3, eprocess.wrapping_add(offsets.eproc_pid as u64))
}

/// 进程链的中间结果，只用于诊断日志：哪一跳读不出来，看这个。
#[derive(Clone, Copy, Default)]
pub struct ChainProbe {
    /// `[KPCR + 0x188]` 读到的 KTHREAD（原样，可能不是合法内核指针）。
    pub kthread: Option<u64>,
    /// `[KTHREAD + kthread_process]` 读到的 EPROCESS。
    pub eprocess: Option<u64>,
    /// `[EPROCESS + eproc_pid]` 读到的 PID。
    pub pid: Option<u32>,
}

/// 走一遍 `KPCR → KTHREAD → EPROCESS → PID`，把每一步的原始结果都带回来。
///
/// 和 `current_process_id` 的区别：这里**不提前 bail**，读不到就留 `None`，
/// 让调用方看得出是哪一跳失败（偏移表错、CR3 不对、还是 KPCR 不是 KPCR）。
pub fn probe_chain(cr3: u64, kpcr: u64, offsets: &crate::kargs::WinOffsets) -> ChainProbe {
    let mut probe = ChainProbe::default();

    probe.kthread = read_guest_u64(cr3, kpcr.wrapping_add(KPCR_CURRENT_THREAD as u64));

    let Some(kthread) = probe.kthread.filter(|k| *k >= 0xffff_8000_0000_0000) else {
        return probe;
    };

    if !offsets.is_valid() {
        return probe;
    }

    probe.eprocess = read_guest_u64(cr3, kthread.wrapping_add(offsets.kthread_process as u64));

    let Some(eprocess) = probe.eprocess.filter(|e| *e >= 0xffff_8000_0000_0000) else {
        return probe;
    };

    probe.pid = read_guest_u32(cr3, eprocess.wrapping_add(offsets.eproc_pid as u64));

    probe
}
