//! Extended Page Tables (EPT).
//!
//! See Intel SDM Vol3C, Chapter 28 "VMX Support for Address Translation".

pub mod data;
pub mod hide_self;
pub mod hook;
#[cfg(feature = "mem")]
pub mod mem;
pub mod state;
pub mod types;

pub use data::EptData;
pub use types::Eptp;

use core::sync::atomic::{AtomicU32, Ordering};

/// EPT 内存类型的代。
///
/// IMPORTANT: MTRR 是 package 级全局资源，EPT 却是每核一份。guest 改 MTRR 或
/// CR0.CD 时把代数 +1，其它核在下一次 VM-exit 比对它、落后就重算自己的表。
/// 用计数而不是 bool：两个核同时改 MTRR 时 bool 会丢通知。
static MEMORY_TYPE_GENERATION: AtomicU32 = AtomicU32::new(0);

/// 标记「内存类型已变」，返回新代。
pub fn bump_memory_type_generation() -> u32 {
    MEMORY_TYPE_GENERATION.fetch_add(1, Ordering::AcqRel) + 1
}

/// 当前代。等于某个 vCPU 记录的代数时，它的 EPT 类型是最新的。
pub fn memory_type_generation() -> u32 {
    MEMORY_TYPE_GENERATION.load(Ordering::Acquire)
}
