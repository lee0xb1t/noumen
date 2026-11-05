//! User-mode side of the Noumen hypercall ABI.
//!
//! `kernel/hypercall_stub.asm` maps the C ABI to `RAX = key`,
//! `RCX = function`, `RDX = arg0`, `R8 = arg1`; the hypervisor puts its result
//! in `RAX`. The loader-generated key is returned by `GetInfo` in
//! [`crate::command::Info::hypercall_key`].
//!
//! A `VMCALL` is a VM exit only on a CPU in VMX non-root operation. Executing
//! it anywhere else raises `#UD` (and Windows terminates the process):
//!
//! - outside VMX operation, e.g. on a CPU that never ran `VMXON`, it is `#UD`;
//! - in VMX root operation it fails with `VMfail` and does not exit.
//!
//! On AMD the instruction is **`VMMCALL`** (0F 01 D9); Intel's `VMCALL` is not
//! implemented there and raises `#UD`. [`is_amd`] caches CPUID.0 and each asm
//! site picks the right instruction.
//!
//! In single-core mode the loader virtualizes exactly one CPU
//! (`Info::start_processor`), so a direct call must first pin the calling
//! thread to that CPU — see [`PinnedThread`]. The race-free alternative is
//! [`crate::command::Device::hypercall_ping`], which lets the loader pick the
//! CPU under its hypervisor lock.

use std::{io, marker::PhantomData};

use windows_sys::Win32::System::Threading::{
    GetCurrentProcessorNumber, GetCurrentThread, SetThreadAffinityMask, Sleep,
};

/// ABI/key probe; the hypervisor replies with [`expected_pong`].
pub const FUNCTION_PING: u64 = 3;

/// 把内核镜像页从 guest EPT 隐藏。
pub const FUNCTION_HIDE_SELF: u64 = 9;
/// 通过 root 直接映射读镜像页。
pub const FUNCTION_READ_SELF: u64 = 10;
/// 列出用户断点的等待项（只读快照）。
pub const FUNCTION_USER_BREAKPOINT_WAIT_LIST: u64 = 13;
/// 应答等待项。
pub const FUNCTION_USER_BREAKPOINT_WAIT_REPLY: u64 = 14;
/// 设置用户断点的等待超时：毫秒，`0` = 默认，`-1` = 无限。
pub const FUNCTION_USER_BREAKPOINT_WAIT_TIMEOUT: u64 = 15;
/// 修改等待项里缓存的通用寄存器。
pub const FUNCTION_USER_BREAKPOINT_WAIT_SET_REG: u64 = 16;
/// 设置用户断点的线程过滤：`0` = 清除。
pub const FUNCTION_USER_BREAKPOINT_WAIT_FILTER: u64 = 17;
/// 取消等待项：`seq`，`0` = 全部。用于“线程已经离开断点”的滞留等待项。
pub const FUNCTION_USER_BREAKPOINT_WAIT_CANCEL: u64 = 18;

/// SVM/NPT-only DR status.
pub const FUNCTION_SVM_DR_STATUS: u64 = 0x39;
/// 一次性断点开关：`RDX` = VA，`R8` = `0`/`1`。命中一次后被禁用。
pub const FUNCTION_BREAKPOINT_ONESHOT: u64 = 19;

/// 内存服务：查询 ABI 与能力。返回 `(abi, max_ops, max_batch_bytes, direct_map)`。
pub const FUNCTION_MEM_QUERY: u64 = 0x20;
/// 内存服务：批量环。`RDX` = 环 VA，`R8` = 环字节数，`R9` bit0 = 只校验。
pub const FUNCTION_MEM_BATCH: u64 = 0x21;
/// 内存服务：分段扫描。`RDX` = `MemScanRequest` VA，`R8` = 结构大小。
pub const FUNCTION_MEM_SCAN: u64 = 0x22;
/// 物理内存区间表（每条 16 B：start u64, size u64）。
pub const FUNCTION_MEM_MAP: u64 = 0x23;
/// 内存服务：物理区间。`RDX` = 目的 VA，`R8` = 条目容量。
/// 内存服务：每 CPU 统计。`RDX` = 目的 VA，`R8` = 条目容量。
pub const FUNCTION_MEM_STATS: u64 = 0x24;

/// TSC 选择性隐藏：设置目标进程 PID（`RDX` = pid，`R8` = 目标模式）。
///
/// `pid = 0` 清除。只有**目标模式允许的特权级**上、且属于目标进程的 `RDTSC`
/// 才返回扣掉 VM-exit host 时间的值；其它情况原样返回物理 TSC（因此不会出现
/// 跨核时钟差）。模式见 [`TSC_MODE_KERNEL`]/[`TSC_MODE_USER`]/[`TSC_MODE_BOTH`]。
pub const FUNCTION_TSC_SET_TARGET_PID: u64 = 0x200;

/// 目标模式：只隐藏内核态（CPL0）的 `RDTSC`。
pub const TSC_MODE_KERNEL: u32 = 0;
/// 目标模式：只隐藏用户态（CPL3）的 `RDTSC`。
pub const TSC_MODE_USER: u32 = 1;
/// 目标模式：内核态和用户态都隐藏。
pub const TSC_MODE_BOTH: u32 = 2;

/// 目标模式的可读名称。
pub const fn tsc_mode_name(mode: u32) -> &'static str {
    match mode {
        TSC_MODE_KERNEL => "内核",
        TSC_MODE_USER => "用户",
        _ => "内核+用户",
    }
}
/// TSC 选择性隐藏：按进程名查 PID（`RDX` = 名称 VA，`R8` = 长度）。
///
/// 需要遍历 `PsActiveProcessHead`，当前 hypervisor 侧返回 `-1`（未实现），
/// 调用方应改用 [`FUNCTION_TSC_SET_TARGET_PID`] 直接传 PID —— 所以这里暂时
/// 没有调用点，但编号是 ABI，必须保留。
#[allow(dead_code)]
pub const FUNCTION_TSC_SET_TARGET_NAME: u64 = 0x201;
/// TSC 选择性隐藏：设置目标内核模块（`RDX` = 名称 VA，`R8` = 基址，`R9` = 大小）。
///
/// `R9 = 0` 清除。模块内的内核代码读 `RDTSC` 同样返回隐藏后的值。
pub const FUNCTION_TSC_SET_TARGET_MODULE: u64 = 0x202;
/// TSC 选择性隐藏：运行时下发 Windows 内核结构偏移表。
///
/// `RDX` = `WinOffsets` 结构 VA（14 个 u32，56 字节），`R8` = 字节数。
/// `build == 0` 表示清除覆盖、退回 `KernelArgs` 里那份。
pub const FUNCTION_TSC_SET_WIN_OFFSETS: u64 = 0x203;
/// TSC 选择性隐藏：设置每次**命中退出**的补偿 tick（`RDX` = tick，`0` 关闭）。
///
/// 账本只能测到「上一次退出 → 本次进入」的 root 时间，guest→root 转换本身在
/// root 侧读不到，所以每次退出都会漏一截。目标进程量到的 `rdtsc; cpuid; rdtsc`
/// 差值里夹着两次退出，补偿值取「实测残差 / 2」。
pub const FUNCTION_TSC_SET_EXIT_OVERHEAD: u64 = 0x204;

/// `-1`：owner CPU 取“任意核”，等待超时取“无限”。
pub const INFINITE: u64 = u64::MAX;

/// `USER_BREAKPOINT_WAIT_REPLY` 动作：恢复原页 + MTF 执行一次原指令。
pub const WAIT_ACTION_CONTINUE: u32 = 0;
/// `USER_BREAKPOINT_WAIT_REPLY` 动作：单步一条指令，停在下一条指令上。
///
/// hypervisor 在 MTF 单步完成后，会给“下一条指令”装一个一次性断点；目标再次
/// 命中时仍是一个普通的 `等待应答` 等待项（`va`/`rip` 就是下一条指令）。
pub const WAIT_ACTION_SINGLE_STEP: u32 = 5;
/// `USER_BREAKPOINT_WAIT_REPLY` 的 `arg` 约定：`1` = 自动应答线程发出，`0` = 手动点击。
///
/// hypervisor 只把这个值写进日志（`断点等待: ACK … arg=0x1（自动应答）`），
/// 不参与任何判断；当前 `Continue`/`SingleStep` 都不使用 action arg。
pub const WAIT_ARG_AUTO: u64 = 1;
/// `USER_BREAKPOINT_WAIT_REPLY` 动作：把 `#BP` 注入 guest。
///
/// 原型阶段 hypervisor 还没有实现阶段 3（需要 guest 侧异常处理），应答会返回
/// “不支持”（`HookError` 3）；编号保留以匹配 ABI。
#[allow(dead_code)]
pub const WAIT_ACTION_INJECT: u32 = 2;

/// `HookWait::state`：没有待处理命中（条目保留，等下一次命中）。
pub const WAIT_STATE_IDLE: u32 = 0;
/// `HookWait::state`：等待应答。
pub const WAIT_STATE_PENDING: u32 = 1;
/// `HookWait::state`：已应答，等待目标线程下一次 `#BP` 应用。
pub const WAIT_STATE_APPLYING: u32 = 2;

/// `HookWait::state` 的可读名称。
pub const fn wait_state_name(state: u32) -> &'static str {
    match state {
        WAIT_STATE_IDLE => "空闲",
        WAIT_STATE_PENDING => "等待应答",
        WAIT_STATE_APPLYING => "已应答",
        _ => "未知",
    }
}

/// 通用寄存器名称，索引与 hypervisor 的 `GeneralPurposeRegister` 一致。
pub const GPR_NAMES: [&str; 16] = [
    "rax", "rcx", "rdx", "rbx", "rsp", "rbp", "rsi", "rdi", "r8", "r9", "r10", "r11", "r12",
    "r13", "r14", "r15",
];

/// 把 UI 输入解析成寄存器索引：`rax`/`eax`/`r8`/`0`..`15`（大小写不敏感）。
pub fn gpr_index(text: &str) -> Option<u8> {
    let text = text.trim().to_ascii_lowercase();

    if text.is_empty() {
        return None;
    }

    if let Some(index) = GPR_NAMES
        .iter()
        .position(|name| *name == text)
    {
        return Some(index as u8);
    }

    // 允许 "eax" 这类 32 位别名，方便从其它调试器抄名字。
    if let Some(name) = text.strip_prefix('e') {
        if let Some(index) = GPR_NAMES
            .iter()
            .position(|candidate| candidate.trim_start_matches('r') == name)
        {
            return Some(index as u8);
        }
    }

    text.parse::<u8>().ok().filter(|index| *index < 16)
}

/// 与 hypervisor 的 `HookWait` 一致（208 字节）。
///
/// 命中时的完整通用寄存器上下文：`thread` 是 `ETHREAD`（读不到为 0），
/// `deadline` 是绝对 TSC；寄存器索引见 [`GPR_NAMES`]。
#[repr(C)]
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct HookWait {
    pub seq: u64,
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
    pub tsc: u64,
    pub deadline: u64,
    pub slot: u32,
    pub cpu: u32,
    pub action: u32,
    pub state: u32,
    pub arg: u64,
    /// 本槽位累计命中次数（从 1 开始，不随应答清零）。
    pub hits: u64,
}

const _: () = assert!(std::mem::size_of::<HookWait>() == 216);

impl HookWait {
    /// 按 [`GPR_NAMES`] 的索引读寄存器。
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

    /// 全部寄存器的一行摘要，便于状态栏显示。
    pub fn gpr_summary(&self) -> String {
        GPR_NAMES
            .iter()
            .enumerate()
            .filter_map(|(index, name)| {
                self.gpr(index as u8)
                    .map(|value| format!("{name}=0x{value:X}"))
            })
            .collect::<Vec<_>>()
            .join(" ")
    }
}

pub const PONG_MAGIC: u64 = 0x504F_4E47;

/// Cached ISA: 0 = unknown, 1 = Intel (`vmcall`), 2 = AMD (`vmmcall`).
static HYPERCALL_ISA: core::sync::atomic::AtomicU8 = core::sync::atomic::AtomicU8::new(0);

/// Intel uses `VMCALL` (0F 01 C1); AMD uses `VMMCALL` (0F 01 D9).
/// Executing the wrong one raises `#UD`, which terminates the process.
fn is_amd() -> bool {
    use core::sync::atomic::Ordering;

    match HYPERCALL_ISA.load(Ordering::Relaxed) {
        2 => return true,
        1 => return false,
        _ => {}
    }

    let leaf = core::arch::x86_64::__cpuid(0);
    let amd = leaf.ebx == 0x6874_7541  // "Auth"
        && leaf.edx == 0x6974_6E65     // "enti"
        && leaf.ecx == 0x444D_4163; // "cAMD"

    HYPERCALL_ISA.store(if amd { 2 } else { 1 }, Ordering::Relaxed);

    amd
}

/// The reply the hypervisor produces for a ping `nonce`.
pub const fn expected_pong(nonce: u64) -> u64 {
    PONG_MAGIC ^ nonce
}

/// Execute a raw `VMCALL` with the Noumen register ABI.
///
/// # Safety
///
/// The CPU must be in VMX non-root operation and `key` must match the key
/// currently installed by the loader. Otherwise the instruction raises `#UD`,
/// which normally terminates the calling process. A different hypervisor may
/// also intercept the instruction and return an arbitrary value.
#[cfg(target_arch = "x86_64")]
pub unsafe fn vmcall(key: u64, function: u64, arg0: u64, arg1: u64) -> u64 {
    let mut result = key;

    if is_amd() {
        unsafe {
            core::arch::asm!(
                "vmmcall",
                inlateout("rax") result,
                in("rcx") function,
                in("rdx") arg0,
                in("r8") arg1,
                options(nostack),
            );
        }
    } else {
        unsafe {
            core::arch::asm!(
                "vmcall",
                inlateout("rax") result,
                in("rcx") function,
                in("rdx") arg0,
                in("r8") arg1,
                options(nostack),
            );
        }
    }

    result
}

#[cfg(not(target_arch = "x86_64"))]
compile_error!("Noumen hypercalls use the x86_64 VMCALL ABI");

/// Execute a raw `VMCALL` with three arguments (`R9` = `arg2`).
///
/// # Safety
///
/// See [`vmcall`].
#[cfg(target_arch = "x86_64")]
pub unsafe fn vmcall3(key: u64, function: u64, arg0: u64, arg1: u64, arg2: u64) -> u64 {
    let mut result = key;

    if is_amd() {
        unsafe {
            core::arch::asm!(
                "vmmcall",
                inlateout("rax") result,
                in("rcx") function,
                in("rdx") arg0,
                in("r8") arg1,
                in("r9") arg2,
                options(nostack),
            );
        }
    } else {
        unsafe {
            core::arch::asm!(
                "vmcall",
                inlateout("rax") result,
                in("rcx") function,
                in("rdx") arg0,
                in("r8") arg1,
                in("r9") arg2,
                options(nostack),
            );
        }
    }

    result
}

/// Ping the hypervisor and return its raw reply.
///
/// # Safety
///
/// See [`vmcall`].
pub unsafe fn ping(key: u64, nonce: u64) -> u64 {
    unsafe { vmcall(key, FUNCTION_PING, nonce, 0) }
}

/// `MEM_QUERY` 的返回值。
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct MemQuery {
    pub abi: u64,
    pub max_ops: u64,
    pub max_batch_bytes: u64,
    pub direct_map: u64,
}

/// 查询内存服务能力。
///
/// 与其它 hypercall 不同，`MEM_QUERY` 用 `RDX/R8/R9` 作为**输出**；因此这里不能
/// 复用 [`vmcall`]，需要单独声明这几个寄存器为 `lateout`。
///
/// # Safety
///
/// 同 [`vmcall`]：调用线程必须运行在已虚拟化的 CPU 上。
pub unsafe fn mem_query(key: u64) -> MemQuery {
    let mut rax = key;
    let rdx: u64;
    let r8: u64;
    let r9: u64;

    unsafe {
        if is_amd() {
            core::arch::asm!(
                "vmmcall",
                inlateout("rax") rax,
                in("rcx") FUNCTION_MEM_QUERY,
                lateout("rdx") rdx,
                lateout("r8") r8,
                lateout("r9") r9,
                options(nostack),
            );
        } else {
            core::arch::asm!(
                "vmcall",
                inlateout("rax") rax,
                in("rcx") FUNCTION_MEM_QUERY,
                lateout("rdx") rdx,
                lateout("r8") r8,
                lateout("r9") r9,
                options(nostack),
            );
        }
    }

    MemQuery {
        abi: rax,
        max_ops: rdx,
        max_batch_bytes: r8,
        direct_map: r9,
    }
}

/// The logical processor the calling thread is currently running on.
pub fn current_processor() -> u32 {
    // SAFETY: GetCurrentProcessorNumber has no preconditions.
    unsafe { GetCurrentProcessorNumber() }
}

/// Pins the calling thread to `processor` for as long as the value is alive.
///
/// `SetThreadAffinityMask` only says where the scheduler may place the thread;
/// the thread can still be executing on its old CPU when the call returns. A
/// `VMCALL` in that window would run on a CPU without a guest and raise `#UD`,
/// so [`PinnedThread::new`] waits until `GetCurrentProcessorNumber` confirms the
/// migration before returning.
///
/// This is only needed for the direct `VMCALL` demo. Prefer
/// [`crate::command::Device::hypercall_ping`], where the loader pins itself
/// under the hypervisor lock and cannot race with `Stop`.
#[must_use = "the thread is only pinned while the guard is alive"]
pub struct PinnedThread {
    previous: usize,
    /// Raw pointers are `!Send`: restoring the mask from another thread would
    /// overwrite that thread's affinity instead of this one's.
    _not_send: PhantomData<*mut ()>,
}

impl PinnedThread {
    /// Restrict the calling thread to `processor` and wait until it runs there.
    pub fn new(processor: u32) -> io::Result<Self> {
        let mask = 1usize.checked_shl(processor).ok_or_else(|| {
            io::Error::new(
                io::ErrorKind::InvalidInput,
                format!("处理器号 {processor} 超出 64 位亲和性掩码"),
            )
        })?;

        let previous = unsafe { SetThreadAffinityMask(GetCurrentThread(), mask) };

        if previous == 0 {
            return Err(io::Error::last_os_error());
        }

        let pinned = Self {
            previous,
            _not_send: PhantomData,
        };

        // `Sleep(1)` forces a real reschedule, unlike `SwitchToThread`, which
        // can return immediately when no other thread is ready and leave the
        // calling thread on the old CPU.
        for _ in 0..32 {
            if unsafe { GetCurrentProcessorNumber() } == processor {
                return Ok(pinned);
            }

            unsafe { Sleep(1) };
        }

        Err(io::Error::new(
            io::ErrorKind::TimedOut,
            format!("线程未能迁移到处理器 {processor}"),
        ))
    }
}

impl Drop for PinnedThread {
    fn drop(&mut self) {
        unsafe {
            SetThreadAffinityMask(GetCurrentThread(), self.previous);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pong_reply_xors_the_nonce() {
        assert_eq!(expected_pong(0), PONG_MAGIC);
        assert_eq!(
            expected_pong(0x1234_5678_9ABC_DEF0),
            PONG_MAGIC ^ 0x1234_5678_9ABC_DEF0
        );
    }

    #[test]
    fn ping_function_matches_the_hypervisor() {
        assert_eq!(FUNCTION_PING, 3);
    }

    #[test]
    fn user_breakpoint_wait_matches_the_hypervisor_layout() {
        assert_eq!(std::mem::size_of::<HookWait>(), 216);
        assert_eq!(std::mem::offset_of!(HookWait, seq), 0);
        assert_eq!(std::mem::offset_of!(HookWait, thread), 8);
        assert_eq!(std::mem::offset_of!(HookWait, cr3), 16);
        assert_eq!(std::mem::offset_of!(HookWait, va), 24);
        assert_eq!(std::mem::offset_of!(HookWait, rip), 32);
        assert_eq!(std::mem::offset_of!(HookWait, rsp), 40);
        assert_eq!(std::mem::offset_of!(HookWait, rax), 48);
        assert_eq!(std::mem::offset_of!(HookWait, r15), 160);
        assert_eq!(std::mem::offset_of!(HookWait, tsc), 168);
        assert_eq!(std::mem::offset_of!(HookWait, deadline), 176);
        assert_eq!(std::mem::offset_of!(HookWait, slot), 184);
        assert_eq!(std::mem::offset_of!(HookWait, cpu), 188);
        assert_eq!(std::mem::offset_of!(HookWait, action), 192);
        assert_eq!(std::mem::offset_of!(HookWait, state), 196);
        assert_eq!(std::mem::offset_of!(HookWait, arg), 200);
        assert_eq!(std::mem::offset_of!(HookWait, hits), 208);

        assert_eq!(FUNCTION_USER_BREAKPOINT_WAIT_LIST, 13);
        assert_eq!(FUNCTION_USER_BREAKPOINT_WAIT_REPLY, 14);
        assert_eq!(FUNCTION_USER_BREAKPOINT_WAIT_TIMEOUT, 15);
        assert_eq!(FUNCTION_USER_BREAKPOINT_WAIT_SET_REG, 16);
        assert_eq!(FUNCTION_USER_BREAKPOINT_WAIT_FILTER, 17);
        assert_eq!(FUNCTION_USER_BREAKPOINT_WAIT_CANCEL, 18);
        assert_eq!(WAIT_ACTION_CONTINUE, 0);
        assert_eq!(WAIT_ACTION_INJECT, 2);
        assert_eq!(WAIT_ACTION_SINGLE_STEP, 5);
        assert_eq!(INFINITE, u64::MAX);
    }

    #[test]
    fn gpr_names_parse_for_the_set_reg_ui() {
        assert_eq!(gpr_index("rax"), Some(0));
        assert_eq!(gpr_index(" RSP "), Some(4));
        assert_eq!(gpr_index("r15"), Some(15));
        assert_eq!(gpr_index("eax"), Some(0));
        assert_eq!(gpr_index("7"), Some(7));
        assert_eq!(gpr_index("15"), Some(15));
        assert_eq!(gpr_index("16"), None);
        assert_eq!(gpr_index("r16"), None);
        assert_eq!(gpr_index(""), None);
    }
}
