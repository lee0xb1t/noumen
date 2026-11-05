//! SVM exit dispatch.

use crate::arch::x86_64::{
    guest_context::GuestContext,
    svm::{
        exit_info::{ExitInfo, SvmExitCode},
        injection,
        instruction::advance_rip,
    },
    vcpu::Vcpu,
};

use crate::klog;

mod cpuid;
mod exception;
mod msr;
#[cfg(feature = "svm-npt")]
mod npf;
mod prepare;

/// Handle one exit. Returns `true` to stop this CPU.
pub fn dispatch(vcpu: &mut Vcpu, ctx: &mut GuestContext, exit: &ExitInfo) -> bool {
    prepare::sync_views(vcpu, exit);

    match exit.code.0 {
        SvmExitCode::CPUID => {
            cpuid::emulate(vcpu, ctx);
            false
        }
        SvmExitCode::MSR => {
            msr::emulate(vcpu, ctx, exit);
            false
        }
        SvmExitCode::VMMCALL => super::hypercall::dispatch(vcpu, ctx),
        // guest 眼里的 SVM 被固件禁用，这些指令全部 #UD。
        SvmExitCode::VMRUN
        | SvmExitCode::VMLOAD
        | SvmExitCode::VMSAVE
        | SvmExitCode::STGI
        | SvmExitCode::CLGI
        | SvmExitCode::SKINIT
        | SvmExitCode::INVLPGA => {
            let svm = vcpu.backend.as_svm_mut();
            advance_rip(&mut svm.vmcb, 3, svm.cached.nrips);
            injection::inject_ud(&mut svm.vmcb);
            false
        }
        #[cfg(feature = "svm-npt")]
        SvmExitCode::NPF => npf::handle(vcpu, exit),
        code if SvmExitCode(code).is_exception() => exception::handle(vcpu, ctx, exit),
        _ => {
            let svm = vcpu.backend.as_svm_mut();
            klog!(
                "SVM: unhandled exit code={:#x} info1={:#x} info2={:#x} rip={:#x} rsp={:#x}",
                exit.code.raw(),
                exit.info1,
                exit.info2,
                svm.vmcb.save.rip,
                svm.vmcb.save.rsp
            );
            true
        }
    }
}
