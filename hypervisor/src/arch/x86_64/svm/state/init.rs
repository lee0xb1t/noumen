use crate::{
    arch::x86_64::{
        guest_context::GuestContext,
        memory,
        safe_routine::{self, HostExceptionInfo},
    },
    error::{HvError, HvResult},
    klog,
};

use super::super::{msrpm::MsrpmPage, vmcb::access::Efer};
use super::{
    EFER_SVME, MSR_EFER, MSR_VM_CR, MSR_VM_HSAVE_PA, SvmCachedData, SvmVcpu, VM_CR_SVMDIS,
};

impl SvmVcpu {
    /// Allocate the control block, host-save page and MSR permission map.
    ///
    /// Allocation is safe to run before SVM is enabled: it touches no
    /// architectural state.
    pub fn init(&mut self) -> HvResult<()> {
        self.cached = SvmCachedData::probe();

        #[cfg(feature = "svm-npt")]
        if !self.cached.npt {
            klog!("SVM: nested paging is not reported; refusing to continue");
            return Err(HvError::UnsupportedCpu);
        }

        if self.cached.nasid < 2 {
            klog!(
                "SVM: NASID={} is too small (ASID 0 is reserved)",
                self.cached.nasid
            );
            return Err(HvError::UnsupportedCpu);
        }

        self.vmcb = memory::alloc()?;
        self.hsave = memory::alloc()?;
        // Two contiguous 4 KiB pages; 4 KiB alignment is what the VMCB field
        // requires, 8 KiB alignment is only a convenience.
        self.msrpm = memory::alloc_pages::<MsrpmPage>(2)?;
        self.iopm = memory::alloc_pages::<[u8; 0x3000]>(3)?;
        self.ctx = memory::alloc::<GuestContext>()?;
        #[cfg(feature = "svm-npt")]
        self.npt.init()?;

        debug_assert_eq!(self.vmcb.phys().as_u64() & 0xfff, 0);
        debug_assert_eq!(self.hsave.phys().as_u64() & 0xfff, 0);
        debug_assert_eq!(self.msrpm.phys().as_u64() & 0xfff, 0);

        klog!(
            "SVM: revision={:#x} nasid={} npt={} nrips={} decode_assists={}",
            self.cached.revision,
            self.cached.nasid,
            self.cached.npt,
            self.cached.nrips,
            self.cached.decode_assists
        );

        Ok(())
    }

    pub(super) fn enable(&self) -> HvResult<()> {
        let vm_cr = unsafe { x86::msr::rdmsr(MSR_VM_CR) };
        if vm_cr & VM_CR_SVMDIS != 0 {
            klog!("SVM: disabled by firmware (VM_CR.SVMDIS=1)");
            return Err(HvError::SvmDisabledByBios);
        }

        let efer = unsafe { x86::msr::rdmsr(MSR_EFER) };
        if efer & EFER_SVME == 0 {
            unsafe { x86::msr::wrmsr(MSR_EFER, efer | EFER_SVME) };
        } else {
            // Another hypervisor already owns SVM on this CPU.
            klog!("SVM: EFER.SVME is already set");
            return Err(HvError::SvmAlreadyEnabled);
        }

        let hsave_pa = self.hsave.phys().as_u64();
        debug_assert_eq!(hsave_pa & 0xfff, 0);
        unsafe { x86::msr::wrmsr(MSR_VM_HSAVE_PA, hsave_pa) };

        Ok(())
    }

    pub(super) fn disable(&mut self) {
        let efer = Efer::read(&self.vmcb).raw() & !EFER_SVME;
        Efer::new(efer).write(&mut self.vmcb);

        let mut info = HostExceptionInfo::new();
        safe_routine::wrmsr_safe(&mut info, MSR_EFER, efer);
    }
}
