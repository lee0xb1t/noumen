//! Hypercall ABI：`RAX` 携带 loader 提供的 key，`RCX` 选择命令。
//!
//! 这里只定义编号和参数约定。VMX 的分发在 `vmx/hypercall/mod.rs`，SVM 的分发在
//! `svm/hypercall/mod.rs`；两个后端的命令实现互不共享。
//!
//! 每个命令只做一件事；入参固定按 `RDX → R8 → R9` 排列，返回值放 `RAX`。
//! `-1` 统一表示“无限 / 任意”（owner CPU、等待超时）。
//!
//! | `RCX` | 命令 | 作用 | `RDX` | `R8` | `R9` | `RAX` |
//! | --- | --- | --- | --- | --- | --- | --- |
//! | 1 | `FLUSH_LOGS` | 取日志 | 缓冲区 | 容量 | - | 条数 |
//! | 2 | `LOGGER_INFO` | 日志统计 | - | - | - | 总数（`RDX`=丢弃，`R8`=缓存） |
//! | 3 | `PING` | 探测 | nonce | - | - | `PONG_MAGIC ^ nonce` |
//! | 4 | `STOP` | 停止本核虚拟化 | - | - | - | -（返回即已 devirt） |
//! | 5 | `HOOK_INSTALL` | 安装 hook | VA | 类型 | handler | `0` 或 `HookError`（`R10`=origin trampoline VA） |
//! | 6 | `HOOK_REMOVE` | 按 VA 移除 hook | VA | - | - | `0` 或 `HookError` |
//! | 7 | `HOOK_REMOVE_PAGE` | 按 GPA 移除整页 hook | GPA | - | - | `0` 或 `HookError` |
//! | 9 | `HIDE_SELF` | 隐藏内核镜像页 | - | - | - | 隐藏页数 |
//! | 10 | `READ_SELF` | root 直映射读镜像页 | 页索引 | 缓冲区 | - | 字节数 |
//! | 11 | `READ_SELF_EPT` | 经 EPT 读镜像页 | 页索引 | 缓冲区 | - | 字节数 |
//! | 13 | `USER_BREAKPOINT_WAIT_LIST` | 列出用户断点等待项 | 缓冲区 | 容量 | - | 条数 |
//! | 14 | `USER_BREAKPOINT_WAIT_REPLY` | 应答等待项 | `seq` | 动作 | 参数 | `0` 或 `HookError` |
//! | 15 | `USER_BREAKPOINT_WAIT_TIMEOUT` | 设置等待超时 | 毫秒 / `-1` | - | - | `0` |
//! | 16 | `USER_BREAKPOINT_WAIT_SET_REG` | 改等待项里的通用寄存器 | `seq` | 寄存器号 | 值 | `0` 或 `HookError` |
//! | 17 | `USER_BREAKPOINT_WAIT_FILTER` | 断点的线程过滤 | VA | `ETHREAD` / `0` | - | `0` 或 `HookError` |
//! | 18 | `USER_BREAKPOINT_WAIT_CANCEL` | 取消等待项（线程不再命中时用） | `seq`（`0`=全部） | - | - | 取消条数 |
//! | 19 | `USER_BREAKPOINT_ONESHOT` | 一次性断点开关（命中后禁用） | VA | `0`=关 / `1`=开 | - | `0` 或 `HookError` |
//! | 20 | `HOOK_APPLY` | 让当前核应用最新一代 hook 状态 | - | - | - | `0` |
//! | 21 | `USER_BREAKPOINT_COMMIT` | 所有核切到 clean 视图后写入 `0xCC` | VA | - | - | `0` 或 `HookError` |
//! | 22 | `HOOK_RETIRED` | 所有在线 vCPU 是否已 retire 当前代 | - | - | - | `1`/`0` |
//! | 0x100 | `HOOK_HIT` | 内部：detour stub 上报命中 | hook id | 参数 | - | `0` |
//!
//! 未知 key -> `#UD`；未知命令：VMX 注入 `#UD`，SVM 返回 `RAX = 3`。flush 失败会注入 `#PF`/`#GP` 且不改动环形缓冲区，
//! guest 可以重试。记录是 `logger_msg`（152 字节）。
//!
//! `RCX = 4`（STOP）比较特殊：VMX 执行 `VMXOFF`；SVM 清 `EFER.SVME` 并 devirt。
//! guest 从调用之后继续运行，但该核不再虚拟化（再次调用会 `#UD`）。
//!
//! hook 安装成功返回 `0`，失败返回 [`HookError`] 编号，不向 guest 注入异常。
//! `RCX = 13/14/15` 是用户断点（`USER_BREAKPOINT`）的等待协议：
//!
//! - 13 只读快照，不消费等待项；
//! - 14 的 `RAX = 0` 成功，否则 [`HookError`] 编号（`7` = seq 不存在，
//!   `3` = 动作未实现，`1` = 动作编号非法）；
//! - 15 只影响之后建立的等待项；`0` = 默认 1000 ms，`-1` = 无限等待，
//!   其它值按 60000 ms 截断。



/// hypercall 编号（`RCX`；`RAX` 带 key）。
///
/// C loader 与 调用方 各自镜像同一组数字，所以编号是 ABI：只能追加，不能重排。
#[repr(u64)]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Hypercall {
    /// 把缓冲区里的记录复制到 guest 缓冲区（`buffer-log`）。
    FlushLogs = 1,
    /// 返回 `RAX = total`、`RDX = dropped`、`R8 = buffered`（`buffer-log`）。
    LoggerInfo = 2,
    /// ABI/key 探测；`RDX = nonce`，返回 `RAX = PONG_MAGIC ^ nonce`。
    Ping = 3,
    /// 停止当前 CPU 的虚拟化。
    Stop = 4,
    /// 安装 hook。
    HookInstall = 5,
    /// 移除指定 VA 的 hook。
    HookRemove = 6,
    /// 移除一个 guest 物理页上的所有 hook。
    HookRemovePage = 7,
    /// 隐藏内核镜像页。
    HideSelf = 9,
    /// 通过直接映射读镜像页。
    ReadSelf = 10,
    /// 通过 EPT/NPT 视图读镜像页。
    ReadSelfEpt = 11,
    /// 列出用户断点的等待项。
    UserBreakpointWaitList = 13,
    /// 应答等待项。
    UserBreakpointWaitReply = 14,
    /// 设置等待超时。
    UserBreakpointWaitTimeout = 15,
    /// 修改等待项缓存的寄存器。
    UserBreakpointWaitSetReg = 16,
    /// 设置线程过滤。
    UserBreakpointWaitFilter = 17,
    /// 取消等待项。
    UserBreakpointWaitCancel = 18,
    /// 一次性用户断点开关。
    UserBreakpointOneshot = 19,
    /// 让当前核立即应用最新 hook 状态（提交前的按核准备）。
    HookApply = 20,
    /// 提交用户断点的字节补丁。
    UserBreakpointCommit = 21,
    /// 查询所有在线 vCPU 是否已经 retire 最新一代 hook。
    HookRetired = 22,
    /// 查询内存服务 ABI。
    MemQuery = 0x20,
    /// 批量内存操作环。
    MemBatch = 0x21,
    /// 分段扫描。
    MemScan = 0x22,
    /// 物理区间表。
    MemMap = 0x23,
    /// 每 CPU 统计。
    MemStats = 0x24,
    /// SVM：带显式 method 安装 hook。
    SvmHookInstall = 0x30,
    /// SVM：按 VA 移除 hook。
    SvmHookRemove = 0x31,
    /// SVM：TF 单步（保留编号，当前返回不支持）。
    SvmTfStep = 0x32,
    /// SVM 用户断点等待表，和 VMX 的 13..18 隔离。
    SvmUserBreakpointWaitList = 0x33,
    SvmUserBreakpointWaitReply = 0x34,
    SvmUserBreakpointWaitTimeout = 0x35,
    SvmUserBreakpointWaitSetReg = 0x36,
    SvmUserBreakpointWaitFilter = 0x37,
    SvmUserBreakpointWaitCancel = 0x38,
    /// SVM：DR0 命中计数（已弃用）。
    #[deprecated(note = "hardware breakpoint path removed")]
    SvmHwDrStatus = 0x39,
    /// SVM：NPT 函数 hook。
    SvmHookInstallNpt = 0x3a,
    /// detour stub 的内部命中上报。
    HookHit = 0x100,
    /// 保留：选择性 TSC 已移除；VMX/SVM 都返回 `3`。
    TscSetTargetPid = 0x200,
    TscSetTargetName = 0x201,
    TscSetTargetModule = 0x202,
    TscSetWinOffsets = 0x203,
    TscSetExitOverhead = 0x204,
    TscSetDivisor = 0x205,
    TscSetHitLog = 0x206,
}

impl Hypercall {
    /// 解析 guest 传入的编号；未知编号返回 `None`。
    #[allow(deprecated)]
    pub const fn from_raw(raw: u64) -> Option<Self> {
        Some(match raw {
            1 => Self::FlushLogs,
            2 => Self::LoggerInfo,
            3 => Self::Ping,
            4 => Self::Stop,
            5 => Self::HookInstall,
            6 => Self::HookRemove,
            7 => Self::HookRemovePage,
            9 => Self::HideSelf,
            10 => Self::ReadSelf,
            11 => Self::ReadSelfEpt,
            13 => Self::UserBreakpointWaitList,
            14 => Self::UserBreakpointWaitReply,
            15 => Self::UserBreakpointWaitTimeout,
            16 => Self::UserBreakpointWaitSetReg,
            17 => Self::UserBreakpointWaitFilter,
            18 => Self::UserBreakpointWaitCancel,
            19 => Self::UserBreakpointOneshot,
            20 => Self::HookApply,
            21 => Self::UserBreakpointCommit,
            22 => Self::HookRetired,
            0x20 => Self::MemQuery,
            0x21 => Self::MemBatch,
            0x22 => Self::MemScan,
            0x23 => Self::MemMap,
            0x24 => Self::MemStats,
            0x30 => Self::SvmHookInstall,
            0x31 => Self::SvmHookRemove,
            0x32 => Self::SvmTfStep,
            0x33 => Self::SvmUserBreakpointWaitList,
            0x34 => Self::SvmUserBreakpointWaitReply,
            0x35 => Self::SvmUserBreakpointWaitTimeout,
            0x36 => Self::SvmUserBreakpointWaitSetReg,
            0x37 => Self::SvmUserBreakpointWaitFilter,
            0x38 => Self::SvmUserBreakpointWaitCancel,
            0x39 => Self::SvmHwDrStatus,
            0x3a => Self::SvmHookInstallNpt,
            0x100 => Self::HookHit,
            0x200 => Self::TscSetTargetPid,
            0x201 => Self::TscSetTargetName,
            0x202 => Self::TscSetTargetModule,
            0x203 => Self::TscSetWinOffsets,
            0x204 => Self::TscSetExitOverhead,
            0x205 => Self::TscSetDivisor,
            0x206 => Self::TscSetHitLog,
            _ => return None,
        })
    }
}

/// ping 回复 magic；guest 检查 `RAX == PONG_MAGIC ^ nonce`。
pub const PONG_MAGIC: u64 = 0x504F_4E47;

