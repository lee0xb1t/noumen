//! 用户断点 INT3 自旋等待的等待项表。
//!
//! 用户断点命中后可以选择停在 `INT3` 上等调试器应答，而不是立刻 MTF 单步。
//! 本模块只实现**纯状态机**：
//!
//! * 每个 hook 槽位最多一个等待项；
//! * 等待项用全局递增的 `seq` 标识，`HookEvent.id` 不能充当 ACK ID；
//! * 超时和 ACK 都不在等待期间修改 EPT/VMCS，真正应用动作在目标线程下一次
//!   `#BP` 时由 `violation.rs` 完成。
//!
//! 这里刻意不引用 crate 内其它模块（也不读 CPUID/TSC），可以在宿主机上单测。

/// 等待项上限：每个 hook 槽位一个。
pub const MAX_HOOK_WAITS: usize = 32;

/// `HookWait::state`：没有待处理命中（条目保留，等下一次命中）。
pub const WAIT_STATE_IDLE: u32 = 0;
/// `HookWait::state`：等待调试器应答。
pub const WAIT_STATE_PENDING: u32 = 1;
/// `HookWait::state`：已收到应答，等待目标线程下一次 `#BP` 应用。
pub const WAIT_STATE_APPLYING: u32 = 2;

/// ACK 动作：恢复原页 + MTF 执行一次原指令。
pub const WAIT_ACTION_CONTINUE: u32 = 0;
/// ACK 动作：单步执行一条指令，然后停在下一条指令上。
///
/// 实现方式：`Continue` 的 MTF 单步完成后，在“下一条指令”（MTF 退出时的 guest
/// RIP）处临时装一个一次性断点并重建 fake page；目标下一次取指就会再次停住。
/// 只支持下一条指令仍在同一页的情况；跨页时退化为 `Continue`（日志会说明）。
pub const WAIT_ACTION_SINGLE_STEP: u32 = 5;
/// ACK 动作：把 `#BP` 注入 guest。原型阶段未实现。
pub const WAIT_ACTION_INJECT: u32 = 2;
/// ACK 动作：修改 guest 寄存器。原型阶段未实现。
pub const WAIT_ACTION_SET_REG: u32 = 3;
/// ACK 动作：结束目标线程。原型阶段未实现。
pub const WAIT_ACTION_KILL: u32 = 4;
/// 旧动作 `Skip`（编号 1）已移除。
///
/// 原语义是“不解码执行、直接把 `RIP += 指令长度`”，等于让 guest 跳过一条指令：
/// 对函数入口的前几条指令（`push`/`sub rsp`）会直接破坏栈帧，实际调试里没有
/// 安全用法，反而容易把目标搞崩。现在编号 1 返回 `InvalidAction`。

/// 默认超时：目标线程最多在 `INT3` 上自旋这么久。
pub const DEFAULT_TIMEOUT_MS: u64 = 1000;
/// 超时上限，避免调试器配置出实际等于“永不继续”的值。
pub const MAX_TIMEOUT_MS: u64 = 60_000;
/// `-1`（全 1）表示无限等待：只由 ACK/移除/owner 变化结束。
pub const INFINITE_TIMEOUT: u64 = u64::MAX;

/// 等待项（ABI，216 字节）。
///
/// **一个断点（槽位）只有一个等待项**：`seq` 在槽位生命周期内保持不变，命中时
/// 更新上下文和 `hits`，应用动作后回到 [`WAIT_STATE_IDLE`]（条目保留），不再
/// 每次命中都换 `seq`。只有移除断点 / 显式取消才会删掉条目。
///
/// 字段顺序即 调用方 / C 侧看到的顺序；`action` 只在 `state == APPLYING` 时有效。
/// 命中时缓存全部 16 个通用寄存器 + `rip`/`rsp`，调试器可以读、也可以改
/// （`HYPERCALL_USER_BREAKPOINT_WAIT_SET_REG`）；应用等待项时会把这份上下文写回目标线程。
///
/// 寄存器索引与 `GeneralPurposeRegister` 一致：
/// `0=rax 1=rcx 2=rdx 3=rbx 4=rsp 5=rbp 6=rsi 7=rdi 8..15=r8..r15`。
#[repr(C)]
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct HookWait {
    /// 槽位内稳定的应答 ID（直到移除/取消才变）。
    pub seq: u64,
    /// 命中的 guest 线程（Windows `ETHREAD` 指针）；读不到时为 0。
    ///
    /// 等待项属于线程：同一地址被别的线程执行时**不**一起自旋。
    pub thread: u64,
    /// 命中时的 guest CR3（进程页表）。
    pub cr3: u64,
    /// hook 的目标 VA。
    pub va: u64,
    /// 命中时的 guest RIP（trap 语义，仍指向 `INT3`）。
    pub rip: u64,
    /// 命中时的 guest RSP。
    pub rsp: u64,

    pub rax: u64,
    pub rcx: u64,
    pub rdx: u64,
    pub rbx: u64,
    pub rbp: u64,
    pub rsi: u64,
    pub rdi: u64,
    pub r8: u64,
    pub r9: u64,
    pub r10: u64,
    pub r11: u64,
    pub r12: u64,
    pub r13: u64,
    pub r14: u64,
    pub r15: u64,

    /// 建立等待项时的 `RDTSC`（每次新命中都会更新）。
    pub tsc: u64,
    /// 超时的绝对 TSC；到达后自动按 `Continue` 处理。
    pub deadline: u64,
    /// 等待项所属的 hook 槽位。
    pub slot: u32,
    /// 命中 `#BP` 的 vCPU。
    pub cpu: u32,
    /// 已请求的动作（见 `WAIT_ACTION_*`）。
    pub action: u32,
    /// 见 `WAIT_STATE_*`。
    pub state: u32,
    pub arg: u64,
    /// 本槽位累计命中次数（从 1 开始，不随应答清零）。
    pub hits: u64,
}

const _: () = assert!(core::mem::size_of::<HookWait>() == 216);

impl HookWait {
    /// 这次命中是否属于这个等待项。
    ///
    /// 等待项只属于**线程**（`ETHREAD`），不看 vCPU、也不看 vCPU 上的 RSP：
    /// 同一个线程被调度到别的核上，下一次 `#BP` 仍然匹配；别的线程即使在同一个
    /// 核、同一个栈上执行，也不匹配。
    ///
    /// 任一侧拿不到 `ETHREAD`（`thread == 0`，或不是 Windows guest）时返回
    /// `false`：调用方只做普通单步，不进入等待——宁可不等待，也不要让别的线程
    /// 跟着自旋。
    pub const fn owned_by(&self, va: u64, thread: u64) -> bool {
        self.va == va && thread != 0 && self.thread == thread
    }

    /// 按 `GeneralPurposeRegister` 索引读一个通用寄存器。
    pub const fn gpr(&self, index: u8) -> Option<u64> {
        Some(match index {
            0 => self.rax,
            1 => self.rcx,
            2 => self.rdx,
            3 => self.rbx,
            4 => self.rsp,
            5 => self.rbp,
            6 => self.rsi,
            7 => self.rdi,
            8 => self.r8,
            9 => self.r9,
            10 => self.r10,
            11 => self.r11,
            12 => self.r12,
            13 => self.r13,
            14 => self.r14,
            15 => self.r15,
            _ => return None,
        })
    }

    /// 按 `GeneralPurposeRegister` 索引写一个通用寄存器；索引非法返回 `false`。
    pub fn set_gpr(&mut self, index: u8, value: u64) -> bool {
        match index {
            0 => self.rax = value,
            1 => self.rcx = value,
            2 => self.rdx = value,
            3 => self.rbx = value,
            4 => self.rsp = value,
            5 => self.rbp = value,
            6 => self.rsi = value,
            7 => self.rdi = value,
            8 => self.r8 = value,
            9 => self.r9 = value,
            10 => self.r10 = value,
            11 => self.r11 = value,
            12 => self.r12 = value,
            13 => self.r13 = value,
            14 => self.r14 = value,
            15 => self.r15 = value,
            _ => return false,
        }

        true
    }
}

/// 建立一个等待项所需的 guest 上下文快照（全部通用寄存器）。
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct WaitContext {
    pub thread: u64,
    pub cr3: u64,
    pub va: u64,
    pub rip: u64,
    pub rsp: u64,
    pub rax: u64,
    pub rcx: u64,
    pub rdx: u64,
    pub rbx: u64,
    pub rbp: u64,
    pub rsi: u64,
    pub rdi: u64,
    pub r8: u64,
    pub r9: u64,
    pub r10: u64,
    pub r11: u64,
    pub r12: u64,
    pub r13: u64,
    pub r14: u64,
    pub r15: u64,
}

/// 某个用户断点的线程过滤：只等待 `thread` 的命中。
#[repr(C)]
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct WaitFilter {
    /// 过滤针对的断点地址；`0` 表示该槽位没有过滤。
    pub va: u64,
    /// 目标 `ETHREAD`；`0` 表示“任意线程”（等价于清除过滤）。
    pub thread: u64,
}

/// 等待项操作失败的原因；调用方负责映射成 ABI 的错误码。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum WaitError {
    /// `slot` 超出等待表。
    NoSlot,
    /// 同一槽位已经有等待项。
    AlreadyPending,
    /// `seq` 不存在（已应用、已取消或从未建立）。
    NotFound,
    /// 动作编号不在 ABI 里。
    InvalidAction,
    /// 动作编号合法，但原型阶段没有实现。
    Unsupported,
    /// 这次命中的线程不在该断点的过滤条件里。
    Filtered,
    /// 拿不到命中线程的 `ETHREAD`；等待项必须绑定线程，所以不建。
    NoThread,
    /// 寄存器索引不在 0..=15。
    InvalidRegister,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct PendingWait {
    info: HookWait,
    /// `None` 表示还在等调试器；`Some` 是 ACK 请求的动作。
    requested: Option<u32>,
}

impl PendingWait {
    /// 条目是否处于“此刻没有待处理命中”的状态。
    const fn is_idle(&self) -> bool {
        self.info.state == WAIT_STATE_IDLE
    }
}

/// 等待项表。`N` 是槽位数；生产代码用 [`MAX_HOOK_WAITS`]。
pub struct WaitTable<const N: usize> {
    entries: [Option<PendingWait>; N],
    /// 每个槽位的线程过滤（针对某个断点地址）。
    filters: [Option<WaitFilter>; N],
    next_seq: u64,
}

impl<const N: usize> WaitTable<N> {
    pub const fn new() -> Self {
        Self {
            entries: [None; N],
            filters: [None; N],
            next_seq: 1,
        }
    }

    /// 当前等待项数量。
    pub fn len(&self) -> usize {
        self.entries.iter().flatten().count()
    }

    /// 槽位上的等待项。
    pub fn lookup(&self, slot: usize) -> Option<&HookWait> {
        self.entries.get(slot)?.as_ref().map(|wait| &wait.info)
    }

    /// 槽位上的线程过滤（宿主机测试用）。
    #[cfg(test)]
    pub fn filter(&self, slot: usize) -> Option<WaitFilter> {
        self.filters.get(slot).copied().flatten()
    }

    /// 记录一次命中：第一次命中时创建条目，之后**原地更新**同一个条目
    /// （`seq` 不变，`hits += 1`，状态回到 Pending），返回它的 `seq`。
    ///
    /// `timeout_ticks == `[`INFINITE_TIMEOUT`] 时 `deadline` 取 `u64::MAX`：
    /// 目标线程只由 ACK、移除 hook 或 owner 变化放行。
    ///
    /// 该地址设置了线程过滤、而命中的不是目标线程时返回
    /// [`WaitError::Filtered`]：调用方照常单步放行，不建等待项，也不上报事件。
    ///
    /// 条目还处在 Pending/Applying（同一次命中的自旋）时返回
    /// [`WaitError::AlreadyPending`]：调用方据此**不再**上报事件、不增加 `hits`，
    /// 这正是“事件风暴”防护。
    pub fn hit(
        &mut self,
        slot: usize,
        cpu: u32,
        context: WaitContext,
        now: u64,
        timeout_ticks: u64,
    ) -> Result<u64, WaitError> {
        if slot >= N {
            return Err(WaitError::NoSlot);
        }

        // 等待项必须绑定到一个具体的 `ETHREAD`：没有线程身份就不建等待项，
        // 调用方按普通单步放行（“只和 ETHREAD 绑定”）。
        if context.thread == 0 {
            return Err(WaitError::NoThread);
        }

        if let Some(filter) = self.filters[slot] {
            if filter.va == context.va
                && filter.thread != 0
                && filter.thread != context.thread
            {
                return Err(WaitError::Filtered);
            }
        }

        let deadline = if timeout_ticks == INFINITE_TIMEOUT {
            INFINITE_TIMEOUT
        } else {
            now.saturating_add(timeout_ticks)
        };

        if let Some(entry) = self.entries[slot].as_mut() {
            // 同一个命中还在自旋：只更新超时，不动 seq/hits。
            if !entry.is_idle() {
                return Err(WaitError::AlreadyPending);
            }

            entry.requested = None;
            entry.info.thread = context.thread;
            entry.info.cr3 = context.cr3;
            entry.info.va = context.va;
            entry.info.rip = context.rip;
            entry.info.rsp = context.rsp;
            entry.info.rax = context.rax;
            entry.info.rcx = context.rcx;
            entry.info.rdx = context.rdx;
            entry.info.rbx = context.rbx;
            entry.info.rbp = context.rbp;
            entry.info.rsi = context.rsi;
            entry.info.rdi = context.rdi;
            entry.info.r8 = context.r8;
            entry.info.r9 = context.r9;
            entry.info.r10 = context.r10;
            entry.info.r11 = context.r11;
            entry.info.r12 = context.r12;
            entry.info.r13 = context.r13;
            entry.info.r14 = context.r14;
            entry.info.r15 = context.r15;
            entry.info.tsc = now;
            entry.info.deadline = deadline;
            entry.info.cpu = cpu;
            entry.info.action = WAIT_ACTION_CONTINUE;
            entry.info.state = WAIT_STATE_PENDING;
            entry.info.arg = 0;
            entry.info.hits += 1;

            return Ok(entry.info.seq);
        }

        let seq = self.next_seq;
        self.next_seq += 1;

        let info = HookWait {
            seq,
            thread: context.thread,
            cr3: context.cr3,
            va: context.va,
            rip: context.rip,
            rsp: context.rsp,
            rax: context.rax,
            rcx: context.rcx,
            rdx: context.rdx,
            rbx: context.rbx,
            rbp: context.rbp,
            rsi: context.rsi,
            rdi: context.rdi,
            r8: context.r8,
            r9: context.r9,
            r10: context.r10,
            r11: context.r11,
            r12: context.r12,
            r13: context.r13,
            r14: context.r14,
            r15: context.r15,
            tsc: now,
            deadline,
            slot: slot as u32,
            cpu,
            action: WAIT_ACTION_CONTINUE,
            state: WAIT_STATE_PENDING,
            arg: 0,
            hits: 1,
        };

        self.entries[slot] = Some(PendingWait {
            info,
            requested: None,
        });

        Ok(seq)
    }

    /// 设置某个槽位的线程过滤；`thread == 0` 表示清除。
    ///
    /// 过滤条件变化时会取消该地址上已有的等待项（如果有），让它按新条件重新
    /// 判断——否则旧线程会一直自旋到超时。
    pub fn set_filter(
        &mut self,
        slot: usize,
        va: u64,
        thread: u64,
    ) -> Result<(), WaitError> {
        if slot >= N {
            return Err(WaitError::NoSlot);
        }

        self.filters[slot] = (thread != 0).then_some(WaitFilter { va, thread });
        self.cancel_va(va);

        Ok(())
    }

    /// 清除所有针对 `va` 的线程过滤，返回清除的数量。
    pub fn clear_filter_va(&mut self, va: u64) -> usize {
        let mut cleared = 0;

        for filter in self.filters.iter_mut() {
            if filter.is_some_and(|filter| filter.va == va) {
                *filter = None;
                cleared += 1;
            }
        }

        cleared
    }

    /// 按 `seq` 改一个通用寄存器，返回槽位。
    ///
    /// 只改缓存值；等待项被应用时 `violation.rs` 会把整份上下文写回目标线程。
    pub fn set_reg(
        &mut self,
        seq: u64,
        index: u8,
        value: u64,
    ) -> Result<usize, WaitError> {
        let slot = self.find_seq(seq)?;
        let wait = self.entries[slot].as_mut().expect("just matched");

        // 空闲条目没有待处理的命中，下一次命中会用新上下文覆盖整份寄存器，
        // 现在改没有意义。
        if wait.info.state == WAIT_STATE_IDLE {
            return Err(WaitError::NotFound);
        }

        if !wait.info.set_gpr(index, value) {
            return Err(WaitError::InvalidRegister);
        }

        Ok(slot)
    }

    fn find_seq(&self, seq: u64) -> Result<usize, WaitError> {
        self.entries
            .iter()
            .position(|entry| entry.is_some_and(|wait| wait.info.seq == seq))
            .ok_or(WaitError::NotFound)
    }

    /// 给 `seq` 对应的等待项设置动作，返回它的槽位。
    ///
    /// 只有 `Continue` 和 `SingleStep` 已实现；`Inject`/`SetReg`/`Kill` 返回
    /// [`WaitError::Unsupported`]（寄存器改走 `HYPERCALL_USER_BREAKPOINT_WAIT_SET_REG`），
    /// 未知编号返回 [`WaitError::InvalidAction`]。
    pub fn ack(&mut self, seq: u64, action: u32, arg: u64) -> Result<usize, WaitError> {
        let action = match action {
            WAIT_ACTION_CONTINUE | WAIT_ACTION_SINGLE_STEP => action,
            WAIT_ACTION_INJECT | WAIT_ACTION_SET_REG | WAIT_ACTION_KILL => {
                return Err(WaitError::Unsupported);
            }
            _ => return Err(WaitError::InvalidAction),
        };

        let index = self.find_seq(seq)?;
        let wait = self.entries[index].as_mut().expect("just matched");

        // 已经应答过、还在等目标应用：不重复记。
        if !matches!(wait.info.state, WAIT_STATE_PENDING) {
            return Err(WaitError::AlreadyPending);
        }

        wait.requested = Some(action);
        wait.info.action = action;
        wait.info.arg = arg;
        wait.info.state = WAIT_STATE_APPLYING;

        Ok(index)
    }

    /// 目标线程下一次 `#BP` 应该执行的动作。
    ///
    /// 收到 ACK 时返回 ACK 的动作；否则只在 Pending 且超过 `deadline` 时返回
    /// `Continue` —— 超时必须让目标线程继续，不能永久停在 `INT3` 上。
    /// Idle 的条目（已经应用过）不产生动作。
    pub fn take_due(&self, slot: usize, now: u64) -> Option<u32> {
        let wait = self.entries.get(slot)?.as_ref()?;

        match wait.info.state {
            WAIT_STATE_APPLYING => wait.requested,
            WAIT_STATE_PENDING if now >= wait.info.deadline => Some(WAIT_ACTION_CONTINUE),
            _ => None,
        }
    }

    /// 动作已应用：条目**保留**，只把状态退回 Idle，等下一次命中原地更新。
    ///
    /// 返回应用前的快照（`seq`/寄存器），供 `apply_wait_action` 写回目标线程。
    pub fn complete(&mut self, slot: usize) -> Option<HookWait> {
        let entry = self.entries.get_mut(slot)?.as_mut()?;

        entry.requested = None;
        entry.info.state = WAIT_STATE_IDLE;

        Some(entry.info)
    }

    /// 取消 `seq` 对应的等待项；`seq == 0` 取消全部。返回取消的数量。
    ///
    /// 用于“线程已经离开断点、等待项永远等不到下一次命中”的情况（例如被调试
    /// 线程在 `system("pause")` / 控制台 I/O 里）：应答已经记下但没人消费，
    /// 槽位会被一直占着。调试器可以显式取消它。
    pub fn cancel_seq(&mut self, seq: u64) -> usize {
        if seq == 0 {
            return self.cancel_all();
        }

        match self.find_seq(seq) {
            Ok(slot) => {
                self.remove(slot);

                1
            }
            Err(_) => 0,
        }
    }

    /// 删掉一个条目（只用于取消/移除，正常应用不删）。
    fn remove(&mut self, slot: usize) -> Option<HookWait> {
        self.entries.get_mut(slot)?.take().map(|wait| wait.info)
    }

    /// 取消某个槽位的等待项和线程过滤（移除断点、复用槽位）。
    pub fn cancel_slot(&mut self, slot: usize) -> Option<HookWait> {
        if slot < N {
            self.filters[slot] = None;
        }

        self.remove(slot)
    }

    /// 取消所有针对 `va` 的等待项，返回取消的数量。
    pub fn cancel_va(&mut self, va: u64) -> usize {
        let mut cancelled = 0;

        for entry in self.entries.iter_mut() {
            if entry.is_some_and(|wait| wait.info.va == va) {
                *entry = None;
                cancelled += 1;
            }
        }

        cancelled
    }

    /// 取消所有等待项（owner 变化、关闭等待模式），返回取消的数量。
    pub fn cancel_all(&mut self) -> usize {
        let mut cancelled = 0;

        for entry in self.entries.iter_mut() {
            if entry.take().is_some() {
                cancelled += 1;
            }
        }

        cancelled
    }

    /// 按 `seq` 从旧到新遍历等待项，最多 `capacity` 条。
    pub fn snapshot(&self, capacity: usize, mut consume: impl FnMut(&HookWait) -> bool) -> usize {
        let limit = capacity.min(N);
        let mut last = 0u64;
        let mut copied = 0;

        while copied < limit {
            let next = self
                .entries
                .iter()
                .flatten()
                .filter(|wait| wait.info.seq > last)
                .min_by_key(|wait| wait.info.seq);

            let Some(wait) = next else {
                break;
            };

            if !consume(&wait.info) {
                break;
            }

            last = wait.info.seq;
            copied += 1;
        }

        copied
    }
}

/// 把毫秒超时换成 TSC ticks。`tsc_hz` 由 CPUID 估计，见 `vmx/timing.rs`。
///
/// [`INFINITE_TIMEOUT`] 原样返回，调用方据此把 `deadline` 设成永不超时。
pub fn timeout_ticks(timeout_ms: u64, tsc_hz: u64) -> u64 {
    if timeout_ms == INFINITE_TIMEOUT {
        return INFINITE_TIMEOUT;
    }

    // timeout_ms <= MAX_TIMEOUT_MS，3 GHz 时乘积约 1.8e14，不会溢出。
    let hz = if tsc_hz == 0 { 1 } else { tsc_hz };

    timeout_ms.saturating_mul(hz) / 1_000
}

#[cfg(test)]
mod tests {
    use super::*;

    const TSC_HZ: u64 = 3_000_000_000;

    fn table() -> WaitTable<8> {
        WaitTable::new()
    }

    const THREAD_A: u64 = 0xFFFF_8000_1234_5000;
    const THREAD_B: u64 = 0xFFFF_8000_1234_6000;
    const CR3: u64 = 0x1AD_000;

    fn context(va: u64, rip: u64, rsp: u64) -> WaitContext {
        WaitContext {
            thread: THREAD_A,
            cr3: CR3,
            va,
            rip,
            rsp,
            ..WaitContext::default()
        }
    }

    #[test]
    fn lifecycle_is_pending_then_applying_then_idle() {
        let mut waits = table();
        let timeout = timeout_ticks(1000, TSC_HZ);

        let seq = waits
            .hit(0, 1, context(0x1000, 0x1000, 0x8000), 100, timeout)
            .unwrap();

        assert_eq!(waits.len(), 1);

        let wait = waits.lookup(0).unwrap();
        assert_eq!(wait.seq, seq);
        assert_eq!(wait.state, WAIT_STATE_PENDING);
        assert_eq!(wait.deadline, 100 + timeout);
        assert_eq!(wait.cpu, 1);
        assert_eq!(waits.take_due(0, 100), None);

        assert_eq!(waits.ack(seq, WAIT_ACTION_CONTINUE, 0), Ok(0));
        assert_eq!(waits.lookup(0).unwrap().state, WAIT_STATE_APPLYING);
        assert_eq!(waits.take_due(0, 100), Some(WAIT_ACTION_CONTINUE));

        // 应用动作：条目**保留**、状态回到 Idle、seq 不变。
        waits.complete(0);
        assert_eq!(waits.len(), 1);
        assert_eq!(waits.lookup(0).unwrap().state, WAIT_STATE_IDLE);
        assert_eq!(waits.lookup(0).unwrap().seq, seq);
        assert_eq!(waits.take_due(0, u64::MAX), None);

        // 下一次命中原地更新同一个条目：seq 不变、hits += 1、回到 Pending。
        assert_eq!(
            waits.hit(0, 2, context(0x1000, 0x1000, 0x8000), 200, timeout),
            Ok(seq)
        );
        assert_eq!(waits.lookup(0).unwrap().hits, 2);
        assert_eq!(waits.lookup(0).unwrap().state, WAIT_STATE_PENDING);
        assert_eq!(waits.lookup(0).unwrap().cpu, 2);
    }

    #[test]
    fn second_hit_on_the_same_slot_creates_no_entry_and_no_event() {
        let mut waits = table();
        let timeout = timeout_ticks(1000, TSC_HZ);

        let seq = waits
            .hit(2, 0, context(0x2000, 0x2000, 0x9000), 0, timeout)
            .unwrap();

        // 自旋期间的后续 #BP 走这里：不新建等待项，也就不再上报事件。
        assert_eq!(
            waits.hit(2, 0, context(0x2000, 0x2000, 0x9000), 10, timeout),
            Err(WaitError::AlreadyPending)
        );

        assert_eq!(waits.len(), 1);
        assert_eq!(waits.lookup(2).unwrap().seq, seq);
    }

    #[test]
    fn every_slot_gets_its_own_entry_and_lookup_is_by_slot() {
        let mut waits = table();
        let timeout = timeout_ticks(1000, TSC_HZ);

        let first = waits
            .hit(1, 0, context(0x1000, 0x1000, 0x8000), 0, timeout)
            .unwrap();
        let second = waits
            .hit(3, 2, context(0x3000, 0x3000, 0xA000), 0, timeout)
            .unwrap();

        assert_ne!(first, second);
        assert_eq!(waits.lookup(1).unwrap().seq, first);
        assert_eq!(waits.lookup(3).unwrap().seq, second);
        assert!(waits.lookup(0).is_none());

        // 越界槽位被拒绝，而不是 panic。
        assert_eq!(
            waits.hit(8, 0, context(0x1000, 0x1000, 0x8000), 0, timeout),
            Err(WaitError::NoSlot)
        );
    }

    #[test]
    fn timeout_falls_back_to_continue() {
        let mut waits = table();
        let timeout = timeout_ticks(100, TSC_HZ);

        waits
            .hit(1, 0, context(0x1000, 0x1000, 0x8000), 1000, timeout)
            .unwrap();

        assert_eq!(waits.take_due(1, 1000 + timeout - 1), None);
        assert_eq!(
            waits.take_due(1, 1000 + timeout),
            Some(WAIT_ACTION_CONTINUE)
        );
        // 超时不会自己清表：真正的清除在动作应用之后。
        assert_eq!(waits.len(), 1);
    }

    #[test]
    fn ack_with_a_foreign_seq_is_not_applied() {
        let mut waits = table();

        let seq = waits
            .hit(0, 0, context(0x1000, 0x1000, 0x8000), 0, 10_000)
            .unwrap();

        assert_eq!(
            waits.ack(seq + 1, WAIT_ACTION_CONTINUE, 0),
            Err(WaitError::NotFound)
        );
        assert_eq!(waits.lookup(0).unwrap().state, WAIT_STATE_PENDING);
        // 5 < deadline，仍在等待。
        assert_eq!(waits.take_due(0, 5), None);

        assert_eq!(waits.ack(seq, WAIT_ACTION_SINGLE_STEP, 0), Ok(0));
        assert_eq!(waits.take_due(0, 5), Some(WAIT_ACTION_SINGLE_STEP));
    }

    #[test]
    fn unimplemented_and_unknown_actions_are_rejected() {
        let mut waits = table();

        let seq = waits
            .hit(0, 0, context(0x1000, 0x1000, 0x8000), 0, 10_000)
            .unwrap();

        for action in [WAIT_ACTION_INJECT, WAIT_ACTION_SET_REG, WAIT_ACTION_KILL] {
            assert_eq!(
                waits.ack(seq, action, 0),
                Err(WaitError::Unsupported),
                "action {action}"
            );
        }

        assert_eq!(
            waits.ack(seq, 0xFFFF_FFFF, 0),
            Err(WaitError::InvalidAction)
        );
        assert_eq!(waits.lookup(0).unwrap().state, WAIT_STATE_PENDING);
    }

    #[test]
    fn removing_a_breakpoint_cancels_its_wait() {
        let mut waits = table();
        let timeout = timeout_ticks(1000, TSC_HZ);

        waits.hit(0, 0, context(0x1000, 0x1000, 0x8000), 0, timeout);
        waits.hit(1, 1, context(0x2000, 0x2000, 0x9000), 0, timeout);

        assert_eq!(waits.cancel_va(0x1000), 1);
        assert_eq!(waits.len(), 1);
        assert!(waits.lookup(0).is_none());
        assert!(waits.lookup(1).is_some());

        assert_eq!(waits.cancel_va(0x1000), 0);
        assert!(waits.cancel_slot(1).is_some());
        assert_eq!(waits.len(), 0);
    }

    #[test]
    fn owner_change_cancels_every_wait() {
        let mut waits = table();
        let timeout = timeout_ticks(1000, TSC_HZ);

        waits.hit(0, 0, context(0x1000, 0x1000, 0x8000), 0, timeout);
        waits.hit(1, 3, context(0x2000, 0x2000, 0x9000), 0, timeout);

        assert_eq!(waits.cancel_all(), 2);
        assert_eq!(waits.len(), 0);
        assert_eq!(waits.cancel_all(), 0);
    }

    #[test]
    fn snapshot_returns_oldest_first_and_stops_on_demand() {
        let mut waits = table();
        let timeout = timeout_ticks(1000, TSC_HZ);

        // 槽位顺序与 seq 顺序故意不一致。
        let first = waits
            .hit(2, 0, context(0x3000, 0x3000, 0xB000), 0, timeout)
            .unwrap();
        let second = waits
            .hit(0, 0, context(0x1000, 0x1000, 0x9000), 0, timeout)
            .unwrap();
        let third = waits
            .hit(1, 0, context(0x2000, 0x2000, 0xA000), 0, timeout)
            .unwrap();

        let mut seqs = [0u64; 3];
        let mut count = 0;
        let copied = waits.snapshot(8, |wait| {
            seqs[count] = wait.seq;
            count += 1;

            true
        });

        assert_eq!(copied, 3);
        assert_eq!(seqs, [first, second, third]);

        // 容量上限和消费者拒绝都会提前结束。
        assert_eq!(waits.snapshot(1, |_| true), 1);
        assert_eq!(waits.snapshot(8, |_| false), 0);
    }

    #[test]
    fn infinite_timeout_never_expires() {
        let mut waits = table();
        let timeout = timeout_ticks(INFINITE_TIMEOUT, TSC_HZ);

        assert_eq!(timeout, INFINITE_TIMEOUT);

        let seq = waits
            .hit(0, 0, context(0x1000, 0x1000, 0x8000), 100, timeout)
            .unwrap();

        assert_eq!(waits.lookup(0).unwrap().deadline, INFINITE_TIMEOUT);
        // 只有 ACK 能放行；TSC 实际上永远到不了 u64::MAX。
        assert_eq!(waits.take_due(0, u64::MAX - 1), None);

        assert_eq!(waits.ack(seq, WAIT_ACTION_CONTINUE, 0), Ok(0));
        assert_eq!(waits.take_due(0, u64::MAX - 1), Some(WAIT_ACTION_CONTINUE));
    }

    #[test]
    fn timeout_ticks_scales_by_the_tsc_frequency() {
        assert_eq!(timeout_ticks(1, 1_000_000_000), 1_000_000);
        assert_eq!(timeout_ticks(DEFAULT_TIMEOUT_MS, TSC_HZ), TSC_HZ);
        assert_eq!(timeout_ticks(MAX_TIMEOUT_MS, TSC_HZ), 60 * TSC_HZ);
        assert_eq!(timeout_ticks(INFINITE_TIMEOUT, TSC_HZ), INFINITE_TIMEOUT);

        // 频率未知时按 1 Hz 兜底，不会除以 0。
        assert_eq!(timeout_ticks(10, 0), 0);
    }

    #[test]
    fn abi_layout_is_216_bytes() {
        assert_eq!(core::mem::size_of::<HookWait>(), 216);
        assert_eq!(core::mem::align_of::<HookWait>(), 8);
        assert_eq!(core::mem::offset_of!(HookWait, seq), 0);
        assert_eq!(core::mem::offset_of!(HookWait, thread), 8);
        assert_eq!(core::mem::offset_of!(HookWait, cr3), 16);
        assert_eq!(core::mem::offset_of!(HookWait, va), 24);
        assert_eq!(core::mem::offset_of!(HookWait, rip), 32);
        assert_eq!(core::mem::offset_of!(HookWait, rsp), 40);
        assert_eq!(core::mem::offset_of!(HookWait, rax), 48);
        assert_eq!(core::mem::offset_of!(HookWait, rcx), 56);
        assert_eq!(core::mem::offset_of!(HookWait, rdx), 64);
        assert_eq!(core::mem::offset_of!(HookWait, rbx), 72);
        assert_eq!(core::mem::offset_of!(HookWait, rbp), 80);
        assert_eq!(core::mem::offset_of!(HookWait, rsi), 88);
        assert_eq!(core::mem::offset_of!(HookWait, rdi), 96);
        assert_eq!(core::mem::offset_of!(HookWait, r8), 104);
        assert_eq!(core::mem::offset_of!(HookWait, r9), 112);
        assert_eq!(core::mem::offset_of!(HookWait, r10), 120);
        assert_eq!(core::mem::offset_of!(HookWait, r11), 128);
        assert_eq!(core::mem::offset_of!(HookWait, r12), 136);
        assert_eq!(core::mem::offset_of!(HookWait, r13), 144);
        assert_eq!(core::mem::offset_of!(HookWait, r14), 152);
        assert_eq!(core::mem::offset_of!(HookWait, r15), 160);
        assert_eq!(core::mem::offset_of!(HookWait, tsc), 168);
        assert_eq!(core::mem::offset_of!(HookWait, deadline), 176);
        assert_eq!(core::mem::offset_of!(HookWait, slot), 184);
        assert_eq!(core::mem::offset_of!(HookWait, cpu), 188);
        assert_eq!(core::mem::offset_of!(HookWait, action), 192);
        assert_eq!(core::mem::offset_of!(HookWait, state), 196);
        assert_eq!(core::mem::offset_of!(HookWait, arg), 200);
        assert_eq!(core::mem::offset_of!(HookWait, hits), 208);
    }

    #[test]
    fn gpr_index_matches_the_debugger_abi() {
        let mut wait = HookWait::default();

        // 0=rax 1=rcx 2=rdx 3=rbx 4=rsp 5=rbp 6=rsi 7=rdi 8..15=r8..r15
        for (index, value) in (0u8..16).map(|index| (index, 0x1000 + index as u64)) {
            assert!(wait.set_gpr(index, value));
            assert_eq!(wait.gpr(index), Some(value));
        }

        assert_eq!(wait.rax, 0x1000);
        assert_eq!(wait.rsp, 0x1004);
        assert_eq!(wait.gpr(16), None);
        assert!(!wait.set_gpr(16, 0));
        assert!(!wait.set_gpr(u8::MAX, 0));
    }

    #[test]
    fn thread_filter_matches_only_the_target_thread() {
        let mut waits = table();
        let timeout = timeout_ticks(1000, TSC_HZ);

        // 过滤 THREAD_B：其它线程连等待项都不建。
        waits.set_filter(0, 0x1000, THREAD_B).unwrap();
        assert_eq!(
            waits.hit(0, 0, context(0x1000, 0x1000, 0x8000), 0, timeout),
            Err(WaitError::Filtered)
        );
        assert_eq!(waits.len(), 0);

        // 目标线程命中：正常建等待项。
        let mut target = context(0x1000, 0x1000, 0x8000);
        target.thread = THREAD_B;

        let seq = waits.hit(0, 0, target, 0, timeout).unwrap();
        assert_eq!(waits.lookup(0).unwrap().seq, seq);

        // 过滤只针对自己的地址：同页其它断点不受影响。
        assert_eq!(
            waits
                .hit(1, 0, context(0x1010, 0x1010, 0x8000), 0, timeout)
                .is_ok(),
            true
        );
    }

    #[test]
    fn setting_a_filter_cancels_the_pending_wait() {
        let mut waits = table();
        let timeout = timeout_ticks(1000, TSC_HZ);

        waits
            .hit(0, 0, context(0x1000, 0x1000, 0x8000), 0, timeout)
            .unwrap();

        waits.set_filter(0, 0x1000, THREAD_B).unwrap();

        assert_eq!(waits.len(), 0);

        // 清除过滤后任意线程都能建等待项。
        waits.set_filter(0, 0x1000, 0).unwrap();
        assert_eq!(waits.filter(0), None);
        assert!(waits
            .hit(0, 0, context(0x1000, 0x1000, 0x8000), 0, timeout)
            .is_ok());
    }

    #[test]
    fn registers_can_be_edited_before_the_wait_is_applied() {
        let mut waits = table();
        let timeout = timeout_ticks(1000, TSC_HZ);

        let seq = waits
            .hit(0, 0, context(0x1000, 0x1000, 0x8000), 0, timeout)
            .unwrap();

        assert_eq!(waits.set_reg(seq, 0, 0xDEAD_BEEF), Ok(0));
        assert_eq!(waits.set_reg(seq + 1, 0, 1), Err(WaitError::NotFound));
        assert_eq!(
            waits.set_reg(seq, 16, 1),
            Err(WaitError::InvalidRegister)
        );

        let wait = *waits.lookup(0).unwrap();
        assert_eq!(wait.rax, 0xDEAD_BEEF);

        // 改寄存器不影响状态机。
        assert_eq!(waits.take_due(0, 0), None);
        assert_eq!(waits.lookup(0).unwrap().state, WAIT_STATE_PENDING);
    }

    #[test]
    fn wait_is_owned_by_the_thread_not_only_the_address() {
        let mut waits = table();
        let timeout = timeout_ticks(1000, TSC_HZ);

        waits
            .hit(0, 0, context(0x1000, 0x1000, 0x8000), 0, timeout)
            .unwrap();

        let wait = *waits.lookup(0).unwrap();

        assert_eq!(wait.thread, THREAD_A);
        assert_eq!(wait.cr3, CR3);

        // 只要 ETHREAD 相等就属于这个等待项——vCPU、RSP 都不参与匹配，
        // 所以线程迁移到别的核（甚至换了栈）仍然匹配。
        assert!(wait.owned_by(0x1000, THREAD_A));
        // 别的线程执行同一地址：不能一起自旋。
        assert!(!wait.owned_by(0x1000, THREAD_B));
        // 地址不同：不是这个等待项。
        assert!(!wait.owned_by(0x2000, THREAD_A));
        // 拿不到 ETHREAD 时不匹配：宁可不等待，也不要误绑到别的线程。
        assert!(!wait.owned_by(0x1000, 0));

        let mut unknown = wait;
        unknown.thread = 0;

        assert!(!unknown.owned_by(0x1000, THREAD_A));
    }

    #[test]
    fn hit_requires_a_thread_identity() {
        let mut waits = table();
        let timeout = timeout_ticks(1000, TSC_HZ);

        let mut anonymous = context(0x1000, 0x1000, 0x8000);
        anonymous.thread = 0;

        // 等待项只和 ETHREAD 绑定：没有线程身份就不建。
        assert_eq!(
            waits.hit(0, 0, anonymous, 0, timeout),
            Err(WaitError::NoThread)
        );
        assert_eq!(waits.len(), 0);
    }

    #[test]
    fn single_step_is_a_supported_action() {
        let mut waits = table();

        let seq = waits
            .hit(0, 0, context(0x1000, 0x1000, 0x8000), 0, INFINITE_TIMEOUT)
            .unwrap();

        assert_eq!(waits.ack(seq, WAIT_ACTION_SINGLE_STEP, 0), Ok(0));
        assert_eq!(waits.lookup(0).unwrap().state, WAIT_STATE_APPLYING);
        assert_eq!(waits.take_due(0, 0), Some(WAIT_ACTION_SINGLE_STEP));

        waits.complete(0);
        assert_eq!(waits.lookup(0).unwrap().state, WAIT_STATE_IDLE);
    }

    #[test]
    fn a_stalled_wait_can_be_cancelled_by_seq_or_all() {
        let mut waits = table();

        let first = waits
            .hit(0, 0, context(0x1000, 0x1000, 0x8000), 0, INFINITE_TIMEOUT)
            .unwrap();

        waits
            .hit(1, 1, context(0x2000, 0x2000, 0x9000), 0, INFINITE_TIMEOUT)
            .unwrap();

        assert_eq!(waits.len(), 2);

        // 线程已经离开断点时，应答/等待项可以按 seq 清掉，槽位不再被占。
        assert_eq!(waits.cancel_seq(first), 1);
        assert_eq!(waits.len(), 1);
        assert_eq!(waits.cancel_seq(first), 0);
        assert_eq!(waits.len(), 1);

        // seq == 0 = 全部。
        assert_eq!(waits.cancel_seq(0), 1);
        assert_eq!(waits.len(), 0);
    }
}
