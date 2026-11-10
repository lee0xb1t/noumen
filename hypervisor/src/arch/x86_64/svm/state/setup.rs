use crate::{
    arch::x86_64::{
        cr, host_state::HostState, msr,
        safe_routine::{self, HostExceptionInfo},
        vcpu::Vcpu,
    },
    error::{HvError, HvResult},
    klog,
};

use super::super::guest;
#[cfg(feature = "svm-npt")]
use super::super::hook::mark_online;
use super::SvmVcpu;

impl SvmVcpu {
    pub(super) fn capture_guest_state(&mut self) {
        // Capture guest state while FS still points at the OS's TCB.
        let _guest_fs_base = guest::write_guest_state(&mut self.vmcb);

        // Initial guest DR state: architectural reset values. DR0–3 are loaded
        // by the stub from `GuestContext`; DR6/DR7 by VMRUN from the VMCB.
        const GUEST_DR6_RESET: u64 = 0xFFFF_0FF0;
        const GUEST_DR7_RESET: u64 = 0x400;

        self.ctx.dr0 = 0;
        self.ctx.dr1 = 0;
        self.ctx.dr2 = 0;
        self.ctx.dr3 = 0;
        self.vmcb.save.dr6 = GUEST_DR6_RESET;
        self.vmcb.save.dr7 = GUEST_DR7_RESET;

        self.capture_cet_state();
    }

    /// VMCB 的 s_cet/ssp/isst 由 VMRUN/#VMEXIT 自动换入换出：先存下 guest 当前
    /// 值，再关掉 root 的 supervisor 影子栈。
    fn capture_cet_state(&mut self) {
        if !shadow_stack_supported() {
            return;
        }

        let mut exception_info = HostExceptionInfo::new();
        let s_cet = safe_routine::rdmsr_safe(&mut exception_info, msr::IA32_S_CET).unwrap_or(0);
        let isst =
            safe_routine::rdmsr_safe(&mut exception_info, msr::IA32_INTERRUPT_SSP_TABLE_ADDR)
                .unwrap_or(0);
        let real_cr4 = cr::Cr4::read().bits();
        let ssp = if real_cr4 & (1 << 23) != 0 {
            cr::read_ssp()
        } else {
            0
        };

        self.vmcb.save.s_cet = s_cet;
        self.vmcb.save.ssp = ssp;
        self.vmcb.save.isst_addr = isst;

        safe_routine::wrmsr_safe(&mut exception_info, msr::IA32_S_CET, 0);

        klog!("SVM: CET s_cet={:#x} ssp={:#x} isst={:#x}", s_cet, ssp, isst);
    }

    pub(super) fn switch_root(&mut self, vcpu: *const Vcpu) {
        // Host FS base must point at the `Vcpu` so `Vcpu::current()` works in
        // the exit handler. The guest value stays in the VMCB and is restored
        // by `vmload` on every #VMEXIT.
        guest::set_host_fs_base(vcpu);

        // Root must run on the hypervisor page table (direct map + kernel).
        // `VMRUN` saves it into HSAVE and restores it on every exit.
        guest::switch_to_host_cr3(crate::hv::get_host_cr3());

        // Identity NPT covers `min(MAXPHYADDR, 64 GiB)`; the guest CR3 was captured above;
        // `VMRUN` loads it while the NPT root lives in `nested_cr3`.
        #[cfg(feature = "svm-npt")]
        self.npt.prepare();
    }

    pub(super) fn assign_asid(&mut self, vcpu: *const Vcpu) -> HvResult<()> {
        // ASID 0 is reserved for the host; give every vCPU its own ASID so a
        // leaf change on one core cannot be served from another core's TLB.
        let cpu_id = unsafe { (*vcpu).cpu_id };
        self.cpu_id = cpu_id;
        #[cfg(feature = "svm-npt")]
        mark_online(cpu_id);
        let asid = cpu_id + 1;
        if asid >= self.cached.nasid {
            klog!(
                "SVM: vCPU#{} needs ASID {} but NASID={}",
                cpu_id,
                asid,
                self.cached.nasid
            );
            self.disable();
            return Err(HvError::UnsupportedCpu);
        }
        self.asid = asid;
        Ok(())
    }

    pub(super) fn finish_setup(&mut self, host: &HostState) {
        // Field diagnostic: prove which MSRPM is actually loaded.
        // [0x106a] covers MSR 0xC001029A (AMD core energy status); with the
        // correct map it must be 0 (allowed), not 0xff.
        klog!(
            "SVM: msrpm[0]={:#x} [0x820]={:#x} [0x1045]={:#x} [0x106a]={:#x}",
            self.msrpm.0[0],
            self.msrpm.0[0x820],
            self.msrpm.0[0x1045],
            self.msrpm.0[0x106a]
        );

        self.host_rsp = host.rsp().as_u64();
        debug_assert_eq!(self.host_rsp & 0xf, 0, "host stack must be 16-byte aligned");

        klog!(
            "SVM: ready vmcb={:#x} hsave={:#x} msrpm={:#x} asid={}",
            self.vmcb.phys().as_u64(),
            self.hsave.phys().as_u64(),
            self.msrpm.phys().as_u64(),
            self.asid
        );
    }
}

fn shadow_stack_supported() -> bool {
    x86::cpuid::cpuid!(7, 0).ecx & (1 << 7) != 0
}
