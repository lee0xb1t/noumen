//! `mem` 的错误码。
//!
//! 这些值会直接穿过 `MEM_BATCH` 的条目状态和 `MEM_SCAN` 的 `status` 字段，
//! 也用于 `RAX`。**编号是 ABI**：只能追加，不能重排。

use crate::arch::x86_64::guest_memory::MemoryFault;

// ABI error codes; not every backend produces every variant.
#[allow(dead_code)]
#[repr(u16)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MemError {
    Ok = 0,
    /// 页表某级 not present（换出 / 未提交 / CR3 不对）。
    NotPresent = 1,
    /// VA 不是 48 位规范形式。
    NonCanonical = 2,
    /// GPA 超出直接映射，或目标页超出 512 GiB。
    OutOfMap = 3,
    /// CR3 非法（为 0、未对齐、超出直接映射）。
    BadCr3 = 4,
    /// 物理地址不在任何 RAM 区间（MMIO 空洞）。
    NotRam = 5,
    /// 单条超过 `MAX_OP_BYTES`，或载荷越界。
    TooLarge = 6,
    /// 环/请求结构本身非法。
    BadEntry = 7,
    /// 扫描缓冲被占用（并发扫描）。
    Busy = 8,
    /// `memcpy_safe` 在宿主触发 fault。
    HostFault = 9,
    /// 特性未编译，或不在 VM-exit 上下文。
    Unsupported = 10,
    /// 目标页被 EPT hook / hide 占用（仅写）。
    Hooked = 11,
    /// 要求页对齐的操作收到未对齐地址。
    Unaligned = 12,
}

impl MemError {
    pub const fn as_u16(self) -> u16 {
        self as u16
    }

    pub const fn as_u64(self) -> u64 {
        self as u64
    }
}

impl From<MemoryFault> for MemError {
    fn from(fault: MemoryFault) -> Self {
        match fault {
            MemoryFault::NonCanonical => MemError::NonCanonical,
            // 本模块只做 present 检查，权限位不参与；`present = true` 的分支
            // 只会来自宿主读到坏 PTE，按“不存在”处理。
            MemoryFault::Page(_) => MemError::NotPresent,
        }
    }
}
