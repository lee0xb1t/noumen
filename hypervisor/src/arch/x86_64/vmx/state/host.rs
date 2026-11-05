use crate::{
    arch::x86_64::{
        addr::VirtAddr,
        cr,
        host_state::HostState,
        paging, segment,
        vcpu::Vcpu,
        vmx::{self, vmcs},
    },
    error::{HvError, HvResult, VmResult},
    hv,
};

use super::VmxVcpu;

impl VmxVcpu {
    pub(super) fn setup_host(&self, host: &HostState, vcpu: *const Vcpu) -> HvResult<()> {
        self.write_host_state(host, vcpu)
            .map_err(HvError::SetupHostFieldsFail)
    }

    fn write_host_state(&self, host: &HostState, vcpu: *const Vcpu) -> VmResult<()> {
        use vmcs::host;

        let mut cr4 = cr::Cr4::read();
        cr4.set_fsgsbase_enable(true);
        cr4.set_os_xsave(true);
        cr4.set_smap_enable(false);
        cr4.set_smep_enable(false);
        cr4.set_control_flow_enforcement_enable(false);

        host::Cr0::from_bits(cr::Cr0::read().bits()).write()?;
        host::Cr4::from_bits(cr4.bits()).write()?;
        host::Cr3::from_raw(hv::get_host_cr3()).write()?;

        host::Rsp::from_va(host.rsp()).write()?;

        let host_rip = vmx::stub::vm_exit;
        host::Rip::from_va(VirtAddr::new(host_rip as *const () as u64)).write()?;

        host::CsSelector::from_selector(segment::HOST_CS_SELECTOR).write()?;
        host::SsSelector::from_selector(segment::HOST_SS_SELECTOR).write()?;
        host::TrSelector::from_selector(segment::HOST_TR_SELECTOR).write()?;
        // DS/ES must be loadable whenever the host is not in 64-bit mode, so
        // reuse the host SS selector. FS/GS selectors stay zero because the
        // corresponding bases are used instead.
        host::DsSelector::from_selector(segment::HOST_SS_SELECTOR).write()?;
        host::EsSelector::from_selector(segment::HOST_SS_SELECTOR).write()?;
        host::FsSelector::from_raw(0).write()?;
        host::GsSelector::from_raw(0).write()?;

        // Host FS/GS base points at the `Vcpu` itself (this is how
        // `Vcpu::current` finds it again), so the `Vcpu` must never be copied
        // or moved while the VMCS is in use.
        host::FsBase::from_va(VirtAddr::from_ptr(vcpu)).write()?;
        host::GsBase::from_va(VirtAddr::null()).write()?;
        host::TrBase::from_va(host.tss_base()).write()?;

        host::GdtrBase::from_va(host.gdt_base()).write()?;
        host::IdtrBase::from_va(host.idt_base()).write()?;

        host::Ia32SysenterCs::from_raw(0).write()?;
        host::Ia32SysenterEsp::from_raw(0).write()?;
        host::Ia32SysenterEip::from_raw(0).write()?;

        host::Ia32Pat::from_pat(paging::PatRegister::default()).write()?;

        // vmcs01 does not load EFER today, but the host field must never be
        // zero: a later VM-exit control that loads host EFER would otherwise
        // resume L0 without LME/SCE/NX.
        host::Ia32Efer::from_raw(unsafe { x86::msr::rdmsr(x86::msr::IA32_EFER) }).write()?;

        host::Ia32PerfGlobalCtrl::from_raw(0).write()?;

        Ok(())
    }
}
