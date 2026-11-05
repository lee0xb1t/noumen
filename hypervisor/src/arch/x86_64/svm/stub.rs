//! SVM entry point and the thin exit-stub bridge.

use core::sync::atomic::{AtomicU32, Ordering};

use crate::{
    arch::x86_64::{
        guest_context::GuestContext,
        svm::{
            exit_handler,
            exit_info::ExitInfo,
            vmcb::{Vmcb, save::StateSaveArea},
        },
        vcpu::Vcpu,
    },
    klog,
};

// Keep the `.equ` constants in `stub.S` honest: a layout change must fail the
// build instead of silently corrupting guest state.
const _: () = assert!(core::mem::offset_of!(Vmcb, save) == 0x400);
const _: () = assert!(
    core::mem::offset_of!(Vmcb, save) + core::mem::offset_of!(StateSaveArea, rip) == 0x578
);
const _: () = assert!(
    core::mem::offset_of!(Vmcb, save) + core::mem::offset_of!(StateSaveArea, rsp) == 0x5d8
);
const _: () = assert!(
    core::mem::offset_of!(Vmcb, save) + core::mem::offset_of!(StateSaveArea, rax) == 0x5f8
);

unsafe extern "C" {
    /// Enter the guest. Returns only as the guest, through the launch
    /// trampoline in `stub.S`; a failed `VMRUN` is handled as an invalid exit.
    pub fn svm_vm_launch(
        vmcb_va: u64,
        vmcb_pa: u64,
        host_rsp: u64,
        hsave_pa: u64,
        ctx: *mut GuestContext,
    );
}

/// Exit diagnostic counter: log only the first few.
static EXIT_COUNT: AtomicU32 = AtomicU32::new(0);
const EXIT_LOG_LIMIT: u32 = 4;

/// Returns `true` to stop this CPU.
#[unsafe(no_mangle)]
extern "C" fn handle_svm_exit(ctx: *mut GuestContext) -> bool {
    let vcpu = unsafe { Vcpu::current() };
    let exit = ExitInfo::from_control(&vcpu.backend.as_svm().vmcb.control);
    let ctx = unsafe { &mut *ctx };

    let count = EXIT_COUNT.fetch_add(1, Ordering::Relaxed);
    if count < EXIT_LOG_LIMIT {
        klog!(
            "SVM exit #{}: code={:#x} info1={:#x} info2={:#x} rip={:#x}",
            count,
            exit.code.raw(),
            exit.info1,
            exit.info2,
            vcpu.backend.as_svm().vmcb.save.rip
        );
    }

    if exit.code.is_invalid() {
        klog!("SVM: VMRUN failed (exit_code=0xFFFF_FFFF); halting");
        return true;
    }

    exit_handler::dispatch(vcpu, ctx, &exit)
}
