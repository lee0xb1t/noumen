pub mod addr;
pub mod cr;
pub mod guest_context;
// Guest VA→PA translation is shared by the hypercall buffers (buffer-log /
// vmx-ept) and the SVM hook engine, so it is always compiled;
// builds without either feature simply have no callers.
#[cfg_attr(
    not(any(feature = "buffer-log", feature = "vmx-ept")),
    allow(dead_code)
)]
pub mod guest_memory;
#[cfg(any(feature = "vmx-ept", feature = "svm-npt"))]
pub mod hook_events;
pub mod host_state;
pub mod hypercall;
pub mod idt;
pub mod init_stack;
pub mod interrupt_handler;
pub mod interrupt_stub;
#[cfg(any(feature = "vmx-ept", feature = "svm-npt"))]
pub mod ldisasm;
pub mod memory;
pub mod msr;
#[cfg(feature = "vmx-ept")]
pub mod mtrr;
pub mod paging;
pub mod resume;
pub mod safe_routine;
pub mod segment;
pub mod serial;
pub mod svm;
pub mod sync;
#[cfg(any(feature = "vmx-ept", feature = "svm-npt"))]
pub mod tsc;
pub mod vcpu;
pub mod vendor;
pub mod vmx;
