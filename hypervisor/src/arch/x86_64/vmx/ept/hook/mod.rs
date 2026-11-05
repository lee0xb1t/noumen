use core::borrow::Borrow;
use core::ops::{Deref, DerefMut};
use core::sync::atomic::{AtomicBool, AtomicU32, AtomicU64, Ordering};

use super::state;
use crate::{
    arch::x86_64::{
        addr::PhysAddr,
        guest_context::GuestContext,
        guest_memory,
        memory::{self, Page, PAGE_SIZE},
        paging::{self, DIRECT_MAP_SIZE},
        resume::{EmulationError, EmulationResult, Terminate, TerminateResult},
        safe_routine::{self, HostExceptionInfo},
        sync as sync_lock,
        vcpu::MAX_CPUS,
        vmx::{
            exit_info::VmxExitQualificationEptViolation,
            vmcs::{control, guest},
            VmxVcpu,
        },
    },
    klog,
};

pub use state::{HookError, HookKind};
use state::{
    HookEntry, PteState, SyncAction, MAX_BREAKPOINTS_PER_PAGE, MAX_HOOKS, PAGE_MASK,
};

pub use user_breakpoint::wait::{
    HookWait, WaitContext, WaitError, WaitTable, DEFAULT_TIMEOUT_MS, INFINITE_TIMEOUT,
    MAX_HOOK_WAITS, MAX_TIMEOUT_MS,
};

#[derive(Clone, Copy, Debug)]
pub struct HookPending {
    pub page_gpa: u64,
    /// 单步之前的 guest RFLAGS（只用于恢复 IF）。
    pub rflags: u64,
    /// 单步开始时 guest RIP（断点地址），用于校验单步是否真的执行在这条指令上。
    pub rip: u64,
    /// 这一次单步是不是 `SingleStep`（是的话，MTF 退出后要在下一条指令上装一次性断点）。
    pub single_step: bool,
    /// 被单步的 `ETHREAD`（`SingleStep` 装临时断点时要用它记 owner）。
    pub thread: u64,
    /// 这一次单步的是 Hide 的 shadow 页（页里只有这一条指令，跨过别的退出继续执行
    /// 会把后面的 0 当代码），所以任何非 MTF 退出都必须撤销它。
    pub hide: bool,
}

mod breakpoint;
mod detour;
mod hit;
mod install;
mod page;
mod support;
mod sync;
mod user_breakpoint;
mod violation;

pub use breakpoint::{abort_pending, handle_breakpoint, handle_monitor_trap};
pub use crate::arch::x86_64::hook_events::HookEvent;
pub use hit::on_hook_hit;
pub use install::{install, remove, remove_page};
pub use sync::sync;
pub use violation::handle_ept_violation;

struct HookSlot {
    entry: HookEntry,
    fake: Page<[u8; PAGE_SIZE]>,
    fake2: Page<[u8; PAGE_SIZE]>,
    trampoline: Page<[u8; PAGE_SIZE]>,
}

impl HookSlot {
    const fn empty() -> Self {
        Self {
            entry: HookEntry::empty(),
            fake: Page::empty(),
            fake2: Page::empty(),
            trampoline: Page::empty(),
        }
    }
}

impl Deref for HookSlot {
    type Target = HookEntry;

    fn deref(&self) -> &Self::Target {
        &self.entry
    }
}

impl DerefMut for HookSlot {
    fn deref_mut(&mut self) -> &mut Self::Target {
        &mut self.entry
    }
}

impl Borrow<HookEntry> for HookSlot {
    fn borrow(&self) -> &HookEntry {
        &self.entry
    }
}

// 私有页跨 vCPU 共享；EPT 和 applied generation 是每 vCPU 的。
static HOOKS: sync_lock::Mutex<[HookSlot; MAX_HOOKS]> =
    sync_lock::Mutex::new([const { HookSlot::empty() }; MAX_HOOKS]);

static GENERATION: AtomicU32 = AtomicU32::new(1);

static APPLIED: [AtomicU32; MAX_CPUS] = [const { AtomicU32::new(0) }; MAX_CPUS];

/// vCPUs that finished `setup`. Only these can hold a stale EPT view, so
/// [`all_retired`] must not wait for processors that were never virtualized
/// (single-core mode leaves the other cores untouched).
static ONLINE: [AtomicBool; MAX_CPUS] = [const { AtomicBool::new(false) }; MAX_CPUS];

pub fn mark_online(cpu_id: u32) {
    if let Some(online) = ONLINE.get(cpu_id as usize) {
        online.store(true, Ordering::Release);
    }
}

/// True once every online vCPU has applied (or retired) the current generation.
///
/// The driver polls this after removing the proxy hook and forcing
/// `HOOK_APPLY` on each core, before freeing the origin page.
pub fn all_retired() -> bool {
    let generation = GENERATION.load(Ordering::Acquire);

    for cpu in 0..MAX_CPUS {
        if ONLINE[cpu].load(Ordering::Acquire)
            && APPLIED[cpu].load(Ordering::Acquire) < generation
        {
            return false;
        }
    }

    true
}


static BREAKPOINT_CPU: AtomicU32 = AtomicU32::new(u32::MAX);

// owner 变化只重写断点页，不重刷 detour/hide。
static BREAKPOINT_GENERATION: AtomicU32 = AtomicU32::new(0);

const BREAKPOINT_ANY_CPU: u32 = u32::MAX;

fn breakpoint_owner(cpu_id: u32) -> bool {
    let owner = BREAKPOINT_CPU.load(Ordering::Acquire);

    owner == BREAKPOINT_ANY_CPU || owner == cpu_id
}

/// 用户断点 INT3 自旋等待的等待项。
///
/// 与 [`HOOKS`] 是两个独立的锁，但顺序固定为 `HOOKS → WAITS`：`#BP` 处理、
/// 安装和移除都先持有 hook 表；`HYPERCALL_USER_BREAKPOINT_WAIT_REPLY` 只取等待表。等待
/// 期间两个锁都必须释放。
static WAITS: sync_lock::Mutex<WaitTable<MAX_HOOK_WAITS>> =
    sync_lock::Mutex::new(WaitTable::new());

const _: () = assert!(MAX_HOOK_WAITS == MAX_HOOKS);

/// 等待超时（毫秒）；[`wait::INFINITE_TIMEOUT`] 表示无限等待。
///
/// 等待本身由 hook 类型 [`HookKind::UserBreakpoint`] 决定，不需要开关。
static WAIT_TIMEOUT_MS: AtomicU64 = AtomicU64::new(DEFAULT_TIMEOUT_MS);

/// 等待超时。
fn wait_timeout_ms() -> u64 {
    WAIT_TIMEOUT_MS.load(Ordering::Acquire)
}

/// 用户断点命中诊断日志的类别。
///
/// 这些日志解释“这次 #BP 为什么没有进等待”。**每个断点每类只打印一次**：命中
/// 由 guest 自旋产生时频率是每秒百万级，任何节流都会刷爆日志，而第一次出现就
/// 足够说明现场条件了。
#[derive(Clone, Copy)]
pub(crate) enum WaitNote {
    /// 命中来自 CPL0（用户页被内核执行）。
    Cpl0,
    /// 同一地址的其它线程命中。
    OtherThread,
    /// 命中线程没有通过该地址的线程过滤。
    Filtered,
    /// 拿不到命中线程的 ETHREAD。
    NoThread,
    /// 断点已移除，或不在 owner CPU 上。
    NotOwner,
    /// 别的线程命中了单步留下的一次性断点（只单步放行，不建等待项）。
    TempOtherThread,
}

impl WaitNote {
    const fn reason(self) -> &'static str {
        match self {
            WaitNote::Cpl0 => "skip: CPL0 execution",
            WaitNote::OtherThread => "skip: other thread hit user breakpoint",
            WaitNote::Filtered => "skip: thread filter mismatch",
            WaitNote::NoThread => "skip: cannot get current thread ETHREAD",
            WaitNote::NotOwner => "skip: non-owner CPU or breakpoint removed",
            WaitNote::TempOtherThread => "skip: single-step one-shot hit by another thread",
        }
    }

    const fn bit(self) -> u32 {
        1 << (self as u32)
    }
}

/// 打一条用户断点命中诊断；同一个断点、同一类原因只打第一次。
pub(crate) fn log_wait_note(
    entry: &mut HookEntry,
    note: WaitNote,
    cpu_id: u32,
    va: u64,
    rip: u64,
) {
    if entry.noted & note.bit() != 0 {
        return;
    }

    entry.noted |= note.bit();

    klog!(
        "breakpoint wait: {} cpu={cpu_id} va={va:#x} rip={rip:#x} (reported once per breakpoint)",
        note.reason()
    );
}

/// 让槽位 `slot` 上的一次性断点失效（等待项被取消、过滤变化、owner 变化）。
///
/// 不在这里重建 fake page：改页要发生在**目标 CPU** 上，由它下一次命中时
/// `handle_breakpoint` 摘除并单步放行（见 `temp_thread == 0` 的分支）。
fn invalidate_temp_breakpoint(table: &mut [HookSlot; MAX_HOOKS], slot: usize, va: u64) {
    let Some(slot_entry) = table.get_mut(slot) else {
        return;
    };

    let offset = (va & PAGE_MASK) as u16;

    if slot_entry.entry.is_temp_breakpoint(offset) && slot_entry.entry.invalidate_temp_breakpoint()
    {
        klog!("breakpoint wait: single-step one-shot 0x{va:#x} marked stale (wait entry cancelled)");
    }
}

/// 把等待项按 `seq` 从旧到新拷给 guest；返回写入条数。
///
/// 与事件环不同，这是**查询**：等待项只有被应答 + 应用、取消或超时后才消失，
/// 重复读不会消费它们。
pub fn wait_list(capacity: usize, consume: impl FnMut(&HookWait) -> bool) -> usize {
    WAITS.lock().snapshot(capacity, consume)
}

/// 给 `seq` 的等待项设置动作；0 成功，否则是 [`HookError`] 编号。
///
/// 只记录动作，不直接改 EPT/VMCS：真正的应用发生在等待它的 vCPU 下一次
/// `#BP`。
pub fn wait_reply(seq: u64, action: u32, arg: u64) -> u32 {
    let mut waits = WAITS.lock();

    match waits.ack(seq, action, arg) {
        Ok(slot) => {
            let info = waits.lookup(slot).copied();
            drop(waits);

            let (va, thread) = match info {
                Some(info) => (info.va, info.thread),
                None => (0, 0),
            };

            // ACK 是控制路径：一次点击 / 一次自动应答只有一条，**不做节流**。
            // 排查“点了 Continue 没反应”时，这一行决定了 ACK 有没有到 hypervisor。
            // `arg == 1` 是 调用方 自动应答线程打的标记（手动点击是 0）。
            klog!(
                "breakpoint wait: ACK seq={seq} slot={slot} action={action} arg={arg:#x}{} \
                 va={va:#x} thread={thread:#x}",
                if arg == 1 { " (automatic)" } else { " (manual)" }
            );

            0
        }
        Err(WaitError::NotFound) => HookError::NotFound.as_u32(),
        Err(WaitError::InvalidAction)
        | Err(WaitError::InvalidRegister)
        | Err(WaitError::Filtered)
        | Err(WaitError::NoThread) => HookError::InvalidAddress.as_u32(),
        Err(WaitError::Unsupported) => HookError::Unsupported.as_u32(),
        Err(WaitError::AlreadyPending) => HookError::AlreadyExists.as_u32(),
        Err(WaitError::NoSlot) => HookError::NoFreeSlot.as_u32(),
    }
}

/// 取消等待项；`seq == 0` 取消全部。返回取消的数量。
///
/// 用途：**线程已经离开断点的等待项**。应答只在目标线程下一次 `#BP` 生效，如果
/// 线程不再回到断点（`system("pause")`、控制台输出、被挂起），应答就永远躺在
/// 槽位里，`begin` 对后续命中返回 `AlreadyPending`，断点看起来“不再等待”。
pub fn wait_cancel(seq: u64) -> usize {
    // 先记下会被取消的 (slot, va)：取消完成后要把对应的一次性断点标记失效，
    // 否则单步留下的 0xCC 会变成永久断点。
    let mut affected: [Option<(usize, u64)>; MAX_HOOK_WAITS] = [None; MAX_HOOK_WAITS];
    let mut affected_count = 0usize;

    let mut waits = WAITS.lock();

    for slot in 0..MAX_HOOK_WAITS {
        let Some(wait) = waits.lookup(slot) else {
            continue;
        };

        if seq != 0 && wait.seq != seq {
            continue;
        }

        if affected_count < MAX_HOOK_WAITS {
            affected[affected_count] = Some((slot, wait.va));
            affected_count += 1;
        }
    }

    let cancelled = waits.cancel_seq(seq);
    drop(waits);

    if cancelled != 0 {
        let mut table = HOOKS.lock();

        for (slot, va) in affected[..affected_count].iter().flatten() {
            invalidate_temp_breakpoint(&mut table, *slot, *va);
        }

        klog!("breakpoint wait: cancel seq={seq} (0=all): {cancelled}");
    }

    cancelled
}

/// 改等待项里缓存的通用寄存器；0 成功，否则是 [`HookError`] 编号。
///
/// 只改缓存值：等待项被应用时（`Continue`/`Skip`/超时）整份上下文写回目标
/// 线程。`index` 与 `GeneralPurposeRegister` 一致（`0=rax` … `15=r15`）。
pub fn wait_set_reg(seq: u64, index: u8, value: u64) -> u32 {
    match WAITS.lock().set_reg(seq, index, value) {
        Ok(slot) => {
            klog!("breakpoint wait: SET_REG seq={seq} slot={slot} reg={index} value={value:#x}");

            0
        }
        Err(WaitError::NotFound) => HookError::NotFound.as_u32(),
        Err(WaitError::InvalidRegister) => HookError::InvalidAddress.as_u32(),
        Err(_) => HookError::InvalidAddress.as_u32(),
    }
}

/// 给 `va` 处的用户断点设置线程过滤；0 成功，否则是 [`HookError`] 编号。
///
/// * `thread != 0`：只等待这个 `ETHREAD` 的命中，其它线程照常单步执行；
/// * `thread == 0`：清除过滤，恢复“谁先命中就等谁”。
///
/// 设置时会取消该地址上已有的等待项，让新条件立即生效。
pub fn set_wait_filter(va: u64, thread: u64) -> u32 {
    if thread != 0 && ((thread >> 47) & 1) == 0 {
        return HookError::InvalidAddress.as_u32();
    }

    let cr3 = match guest::Cr3::read() {
        Ok(cr3) => cr3.as_raw(),
        Err(_) => return HookError::NotMapped.as_u32(),
    };

    let mut table = HOOKS.lock();
    let Some(index) = state::find_by_va(&table[..], va, cr3) else {
        return HookError::NotFound.as_u32();
    };

    if !table[index].kind.is_user_breakpoint() {
        return HookError::Unsupported.as_u32();
    }

    let result = WAITS.lock().set_filter(index, va, thread);

    if result.is_ok() {
        invalidate_temp_breakpoint(&mut table, index, va);
    }

    drop(table);

    match result {
        Ok(()) => {
            if thread == 0 {
                klog!("breakpoint wait: clear thread filter va={va:#x} slot={index}");
            } else {
                klog!(
                    "breakpoint wait: thread filter va={va:#x} slot={index} thread={thread:#x}"
                );
            }

            0
        }
        Err(_) => HookError::NoFreeSlot.as_u32(),
    }
}

/// 设置断点的“一次性”开关；0 成功，否则是 [`HookError`] 编号。
///
/// `one_shot = true`：这个断点在**第一次**命中被 Continue（或超时）之后置
/// `enabled = false`（禁用），页恢复原状，之后执行到这里不再停。
/// `one_shot = false`：恢复普通断点，并把 `enabled` 重新置 true。
pub fn set_one_shot(va: u64, one_shot: bool) -> u32 {
    let cr3 = match guest::Cr3::read() {
        Ok(cr3) => cr3.as_raw(),
        Err(_) => return HookError::NotMapped.as_u32(),
    };

    let mut table = HOOKS.lock();
    let Some(index) = state::find_by_va(&table[..], va, cr3) else {
        return HookError::NotFound.as_u32();
    };

    if !table[index].kind.is_user_breakpoint() {
        return HookError::Unsupported.as_u32();
    }

    table[index].one_shot = one_shot;
    table[index].enabled = true;
    drop(table);

    klog!("breakpoint one-shot: va={va:#x} one_shot={one_shot} (after hit: {})", if one_shot { "disabled" } else { "keep firing" });

    0
}

/// 让某个核立即应用最新一代 hook 状态（驱动按核轮询调用）。
///
/// 安装用户断点分两步就是靠它：先 `install` 只把各核 leaf 切到 clean 视图，
/// 驱动对每个核调一次这里，全部切完之后才 `commit_byte_patch` 写 `0xCC`。
pub fn sync_now(cpu_id: u32, vmx: &mut VmxVcpu) {
    sync::sync(cpu_id, vmx);
}

/// 把所有核的 clean 视图准备好之后，真正把 `0xCC` 写进真实页。
///
/// 顺序保证：任何核在任何时刻做数据读，看到的都是 clean 副本里的原字节；
/// 补丁只会在所有核都已经走副本之后出现。
pub fn commit_byte_patch(va: u64) -> u32 {
    let cr3 = match guest::Cr3::read() {
        Ok(cr3) => cr3.as_raw(),
        Err(_) => return HookError::NotMapped.as_u32(),
    };

    let mut table = HOOKS.lock();
    let Some(index) = state::find_by_va(&table[..], va, cr3) else {
        return HookError::NotFound.as_u32();
    };

    if !table[index].byte_mode || !table[index].pending_patch {
        return HookError::NotFound.as_u32();
    }

    let target = table[index].page_gpa + table[index].patched_offset as u64;

    if let Err(error) = support::write_gpa_byte(target, 0xCC) {
        return error.as_u32();
    }

    table[index].pending_patch = false;
    drop(table);

    klog!("user breakpoint: all CPUs switched to clean view; writing 0xCC to gpa={target:#x} (va={va:#x})");

    0
}

/// 停掉 hypervisor 之前，把所有“字节补丁”恢复成原字节。
///
/// 用户断点现在是直接改真实页的 `0xCC`；如果带着补丁退出虚拟化，guest 之后
/// 执行到那里会触发没人处理的 `#BP`（蓝屏）。逐个槽位写回原字节，保证退出后
/// 系统还是干净的。
pub fn restore_patched_bytes() -> usize {
    let mut table = HOOKS.lock();
    let mut restored = 0;

    for slot in table.iter_mut() {
        if !slot.entry.byte_mode || slot.entry.pending_patch {
            continue;
        }

        let target = slot.entry.page_gpa + slot.entry.patched_offset as u64;

        if support::write_gpa_byte(target, slot.entry.saved_byte).is_ok() {
            restored += 1;
        }

        slot.entry.byte_mode = false;
    }

    restored
}

///
/// * `timeout_ms == 0`：恢复默认 [`DEFAULT_TIMEOUT_MS`]；
/// * `timeout_ms == -1`（[`INFINITE_TIMEOUT`]）：无限等待，只由 ACK、
///   移除 hook 或 owner 变化放行；
/// * 其它值按 [`MAX_TIMEOUT_MS`] 截断。
///
/// 已经在等待的项保留原 deadline；要立刻让它们重新计时就移除/重装断点。
/// 设置用户断点的等待超时；`0` 恢复默认（1000 ms），`-1` 无限。
///
/// 已经在等待的项保留原 deadline；要立刻让它们重新计时就移除/重装断点。
pub fn set_wait_timeout(timeout_ms: u64) -> u32 {
    let timeout = if timeout_ms == 0 {
        DEFAULT_TIMEOUT_MS
    } else if timeout_ms == INFINITE_TIMEOUT {
        INFINITE_TIMEOUT
    } else {
        timeout_ms.min(MAX_TIMEOUT_MS)
    };

    WAIT_TIMEOUT_MS.store(timeout, Ordering::Release);

    // owner 直接决定断点在哪个核上生效：owner 绑到别的核时，目标线程所在的核
    // 根本不会命中，等待也就无从谈起。把 owner 和 TSC 频率一起打进日志。
    let owner = BREAKPOINT_CPU.load(Ordering::Acquire);
    let tsc_hz = crate::arch::x86_64::tsc::tsc_hz();
    let pending = WAITS.lock().len();

    if timeout == INFINITE_TIMEOUT {
        klog!(
            "breakpoint wait: timeout=infinite owner={} tsc_hz={} pending={pending}",
            if owner == BREAKPOINT_ANY_CPU {
                "any"
            } else {
                "one-cpu"
            },
            tsc_hz
        );
    } else {
        klog!(
            "breakpoint wait: timeout={}ms owner={} tsc_hz={} pending={pending}",
            timeout,
            if owner == BREAKPOINT_ANY_CPU {
                "any"
            } else {
                "one-cpu"
            },
            tsc_hz
        );
    }

    0
}
