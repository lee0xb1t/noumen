//! SVM hypercall dispatch.

use crate::arch::x86_64::{
    guest_context::GuestContext,
    hypercall::Hypercall,
    svm::{devirt, injection, instruction::advance_rip},
    vcpu::Vcpu,
};

mod command;
mod diag;
#[cfg(feature = "svm-npt")]
mod hook;
#[cfg(feature = "svm-npt")]
mod mem;
#[cfg(feature = "svm-npt")]
mod wait;

#[allow(deprecated)]
pub fn dispatch(vcpu: &mut Vcpu, ctx: &mut GuestContext) -> bool {
    let key = crate::hv::hypercall_key();
    if key == 0 || ctx.rax != key {
        injection::inject_ud(&mut vcpu.backend.as_svm_mut().vmcb);
        return false;
    }

    match Hypercall::from_raw(ctx.rcx) {
        Some(Hypercall::Ping) => diag::ping(ctx),
        Some(Hypercall::Stop) => {
            // VMCB save.rip still points at VMMCALL; devirt iretqs to it.
            {
                let svm = vcpu.backend.as_svm_mut();
                advance_rip(&mut svm.vmcb, 3, svm.cached.nrips);
            }
            devirt::devirtualize(vcpu);
        }
        Some(command) => command::dispatch(vcpu, ctx, command),
        None => ctx.rax = 3,
    }

    let svm = vcpu.backend.as_svm_mut();
    advance_rip(&mut svm.vmcb, 3, svm.cached.nrips);
    false
}
