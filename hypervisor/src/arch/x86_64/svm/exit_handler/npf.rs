use crate::arch::x86_64::{
    addr::PhysAddr,
    svm::{exit_info::ExitInfo, hook},
    vcpu::Vcpu,
};

use crate::klog;

pub(super) fn handle(vcpu: &mut Vcpu, exit: &ExitInfo) -> bool {
    let svm = vcpu.backend.as_svm_mut();
    if let Some(npf) = exit.npf() {
        if hook::handle_npf(svm, npf.gpa, npf.fetch) {
            return false;
        }

        // A retired view can leave this core's leaf NX until `sync`
        // restored it above; retry instead of stopping the CPU.
        if npf.fetch && svm.npt.executable(PhysAddr::new(npf.gpa)).unwrap_or(false) {
            return false;
        }
    }

    log_npf(exit);
    true
}

fn log_npf(exit: &ExitInfo) {
    match exit.npf() {
        Some(npf) => klog!(
            "SVM: unhandled #NPF gpa={:#x} present={} write={} fetch={} rsvd={}",
            npf.gpa,
            npf.present,
            npf.write,
            npf.fetch,
            npf.rsvd
        ),
        None => klog!("SVM: NPF exit without valid info1={:#x}", exit.info1),
    }
}
