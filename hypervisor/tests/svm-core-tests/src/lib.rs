//! Host-side test harness for the pure SVM modules.
//!
//! The modules are included by path so they are compiled exactly as they are in
//! the hypervisor crate (including their `#[cfg(test)]` tests) without pulling
//! in `no_std`, the page allocator or any hardware access.

#![allow(dead_code)]

#[path = "../../../src/arch/x86_64/svm/vmcb/mod.rs"]
pub mod vmcb;

#[path = "../../../src/arch/x86_64/svm/msrpm.rs"]
pub mod msrpm;

#[path = "../../../src/arch/x86_64/svm/exit_info.rs"]
pub mod exit_info;

#[path = "../../../src/arch/x86_64/svm/injection.rs"]
pub mod injection;

#[path = "../../../src/arch/x86_64/svm/instruction.rs"]
pub mod instruction;

#[path = "../../../src/arch/x86_64/hook_events.rs"]
pub mod hook_events;

#[path = "../../../src/arch/x86_64/svm/npt/hook/wait.rs"]
pub mod wait;

pub mod npt {
    #[path = "../../../../src/arch/x86_64/svm/npt/types.rs"]
    pub mod types;

    #[path = "../../../../src/arch/x86_64/svm/npt/state.rs"]
    pub mod state;
}
