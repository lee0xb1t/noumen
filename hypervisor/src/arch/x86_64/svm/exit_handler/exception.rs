use core::sync::atomic::{AtomicU32, Ordering};

use crate::arch::x86_64::{
    guest_context::GuestContext,
    svm::{exit_info::ExitInfo, injection},
    vcpu::Vcpu,
};

#[cfg(feature = "svm-npt")]
use crate::arch::x86_64::svm::hook;
use crate::klog;

pub(super) fn handle(vcpu: &mut Vcpu, ctx: &mut GuestContext, exit: &ExitInfo) -> bool {
    #[cfg(not(feature = "svm-npt"))]
    let _ = ctx;

    let Some(vector) = exit.code.exception_vector() else {
        return true;
    };

    let svm = vcpu.backend.as_svm_mut();

    // Any INT3 at all (ours or the guest's) is worth knowing about: it tells
    // "the patched byte was fetched" apart from "the function never ran".
    static BP_EXITS: AtomicU32 = AtomicU32::new(0);

    if vector == 3 && BP_EXITS.fetch_add(1, Ordering::Relaxed) < 8 {
        klog!(
            "SVM #BP exit rip={:#x} cs={:#x} cr3={:#x}",
            svm.vmcb.save.rip,
            svm.vmcb.save.cs.selector,
            svm.vmcb.save.cr3
        );
    }

    let handled = match vector {
        1 => {
            #[cfg(feature = "svm-npt")]
            {
                hook::handle_db(svm)
            }
            #[cfg(not(feature = "svm-npt"))]
            {
                false
            }
        }
        3 => {
            #[cfg(feature = "svm-npt")]
            {
                hook::handle_bp(svm, ctx)
            }
            #[cfg(not(feature = "svm-npt"))]
            {
                false
            }
        }
        _ => false,
    };

    if handled {
        return false;
    }

    // A guest-owned INT3 may leave RIP at the INT3 (the intercept happens before
    // the trap updates RIP). INT3 is one byte, so advancing is always correct
    // and does not depend on reading guest memory again.
    if vector == 3 {
        svm.vmcb.save.rip = svm.vmcb.save.rip.wrapping_add(1);
    }

    // Not ours: inject the same event back. The exit was taken before delivery,
    // so RIP/CR2 still describe the guest's fault/trap.
    let error = exit.exception().and_then(|info| info.error_code);
    injection::inject_exception(&mut svm.vmcb, vector, error);

    false
}
