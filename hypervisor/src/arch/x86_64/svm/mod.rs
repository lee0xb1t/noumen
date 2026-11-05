//! AMD SVM backend.

pub mod devirt;
pub mod exit_handler;
pub mod exit_info;
pub mod guest;
pub mod hypercall;
pub mod injection;
pub mod instruction;
pub mod msrpm;
#[cfg(feature = "svm-npt")]
pub mod npt;
pub mod state;
pub mod stub;
pub mod vmcb;

// 实现位于 npt/ 下；保留 `svm::hook` 等旧路径给现有调用方。
#[cfg(feature = "svm-npt")]
pub use npt::{hide_self, hook, mem};
#[cfg(feature = "svm-npt")]
pub use npt::mem::scan;
#[cfg(feature = "svm-npt")]
pub use npt::tlb;

pub use state::SvmVcpu;

use crate::error::HvResult;

/// Probe SVM support. Touches no architectural state.
pub fn check_svm_features() -> HvResult<()> {
    state::check_features()
}

/// Whether SVM is currently enabled on this CPU.
pub fn is_enabled() -> bool {
    state::is_enabled()
}
