//! TSC 虚拟化已禁用。
//!
//! 当前 guest RDTSC 直接读物理 TSC（无 VM-exit），所有核时钟一致。
//!
//! 如需选择性隐藏特定进程的 TSC（隐藏 VM-exit 开销），使用 `selective_tsc` 模块。

/// 占位符结构，保留用于未来扩展。
pub struct VirtualTsc {
    _phantom: core::marker::PhantomData<()>,
}

impl VirtualTsc {
    pub const fn new() -> Self {
        VirtualTsc {
            _phantom: core::marker::PhantomData,
        }
    }
}
