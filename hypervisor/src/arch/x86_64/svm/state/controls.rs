use super::super::vmcb::clean::CleanBit;
use super::{MSR_EFER, MSR_VM_CR, MSR_VM_HSAVE_PA, SvmVcpu};

impl SvmVcpu {
    pub(super) fn setup_intercepts(&mut self) {
        {
            let control = &mut self.vmcb.control;

            control.intercept_misc2.set_vmrun(true);
            control.intercept_misc2.set_vmload(true);
            control.intercept_misc2.set_vmsave(true);
            control.intercept_misc2.set_stgi(true);
            control.intercept_misc2.set_clgi(true);
            control.intercept_misc2.set_skinit(true);
            control.intercept_misc1.set_invlpga(true);

            control.intercept_misc2.set_vmmcall(true);
            control.intercept_misc1.set_msr_prot(true);
            control.intercept_misc1.set_cpuid(true);

            // 软件断点与 TF 单步。
            control.intercept_exception.set_bp(true);
            control.intercept_exception.set_db(true);
            control.clean.mark_dirty(CleanBit::Intercepts);
        }

        self.msrpm.intercept(MSR_EFER);
        self.msrpm.intercept(MSR_VM_CR);
        self.msrpm.intercept(MSR_VM_HSAVE_PA);

        let control = &mut self.vmcb.control;
        control.iopm_base_pa = self.iopm.phys().as_u64();
        control.msrpm_base_pa = self.msrpm.phys().as_u64();
        control.asid = self.asid;
        control.tlb_control = 0;
        #[cfg(feature = "svm-npt")]
        {
            control.nested_ctl = 1; // SVM_NESTED_CTL_NP_ENABLE
            control.nested_cr3 = self.npt.ncr3();
        }
        #[cfg(not(feature = "svm-npt"))]
        {
            control.nested_ctl = 0;
            control.nested_cr3 = 0;
        }
        control.clean.mark_dirty(CleanBit::PermMaps);
        control.clean.mark_dirty(CleanBit::Asid);
        control.clean.mark_dirty(CleanBit::Npt);
    }
}
