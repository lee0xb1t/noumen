use core::sync::atomic::{AtomicU32, Ordering};

use crate::arch::x86_64::{
    guest_context::GuestContext,
    safe_routine,
    svm::{
        exit_info::ExitInfo,
        injection,
        instruction::advance_rip,
        state::{EFER_SVME, MSR_EFER, MSR_VM_CR, MSR_VM_HSAVE_PA, VM_CR_LOCK, VM_CR_SVMDIS},
        vmcb::access::Efer,
    },
    vcpu::Vcpu,
};

use crate::klog;

pub(super) fn emulate(vcpu: &mut Vcpu, ctx: &mut GuestContext, exit: &ExitInfo) {
    let msr = ctx.ecx();
    let write = exit.info1 & 1 != 0;
    let value = (ctx.rdx << 32) | (ctx.rax & 0xffff_ffff);
    let svm = vcpu.backend.as_svm_mut();

    // First few MSR exits identify any unexpected intercept (MSRPM bug).
    static MSR_LOGS: AtomicU32 = AtomicU32::new(0);

    if MSR_LOGS.fetch_add(1, Ordering::Relaxed) < 8 {
        klog!(
            "SVM MSR: msr={:#x} write={} rip={:#x}",
            msr,
            write,
            svm.vmcb.save.rip
        );
    }

    match msr {
        MSR_EFER => {
            if write {
                // guest 看到的 EFER.SVME 是 0；写 SVME 与“SVM 被禁用”矛盾。
                if value & EFER_SVME != 0 {
                    injection::inject_gp(&mut svm.vmcb, 0);
                    return;
                }

                Efer::new((value & !EFER_SVME) | EFER_SVME).write(&mut svm.vmcb);
            } else {
                let efer = Efer::read(&svm.vmcb).raw() & !EFER_SVME;
                ctx.rax = efer & 0xffff_ffff;
                ctx.rdx = efer >> 32;
            }
        }
        MSR_VM_CR => {
            if write {
                // VM_CR.LOCK=1：写入 #GP。
                injection::inject_gp(&mut svm.vmcb, 0);
                return;
            }

            // “固件禁用了 SVM”：SVMDIS=1, LOCK=1。
            let vmcr = VM_CR_SVMDIS | VM_CR_LOCK;
            ctx.rax = vmcr & 0xffff_ffff;
            ctx.rdx = vmcr >> 32;
        }
        MSR_VM_HSAVE_PA => {
            // SVME=0 时这个 MSR 不存在。
            injection::inject_gp(&mut svm.vmcb, 0);
            return;
        }
        _ => {
            // MSRPM covers only three ranges; MSRs outside them are always
            // intercepted and must be forwarded (reads) or emulated.
            if !pass_through_msr(ctx, msr, write, value) {
                injection::inject_gp(&mut svm.vmcb, 0);
                return;
            }
        }
    }

    advance_rip(&mut svm.vmcb, 2, svm.cached.nrips);
}

/// Forward an unhandled MSR to hardware with the host-exception guard. Returns
/// `false` when the access faulted (caller injects #GP).
fn pass_through_msr(ctx: &mut GuestContext, msr: u32, write: bool, value: u64) -> bool {
    let mut info = safe_routine::HostExceptionInfo::new();

    if write {
        safe_routine::wrmsr_safe(&mut info, msr, value);
    } else {
        match safe_routine::rdmsr_safe(&mut info, msr) {
            Some(read) => {
                ctx.rax = read & 0xffff_ffff;
                ctx.rdx = read >> 32;
            }
            None => return false,
        }
    }

    !info.has_occurred
}
