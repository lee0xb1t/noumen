use crate::{
    arch::x86_64::{cr, msr, segment, vmx::vmcs},
    error::{HvError, HvResult, VmResult},
};

use super::VmxVcpu;

impl VmxVcpu {
    pub(super) fn setup_guest(&self) -> HvResult<()> {
        self.write_guest_state()
            .map_err(HvError::SetupGuestFieldsFail)?;

        Ok(())
    }

    fn write_guest_state(&self) -> VmResult<()> {
        use vmcs::guest;

        guest::Cr0::from_bits(cr::Cr0::read().bits()).write()?;
        guest::Cr4::from_bits(cr::Cr4::read().bits()).write()?;
        guest::Cr3::from_raw(unsafe { x86::controlregs::cr3() }).write()?;
        guest::Dr7::from_raw(unsafe { x86::debugregs::dr7() }.0 as u64).write()?;

        guest::Rsp::from_raw(0).write()?;
        guest::Rip::from_raw(0).write()?;

        guest::Rflags::from_raw(x86::bits64::rflags::read().bits()).write()?;

        let cs = segment::SegmentSelector::read_cs();
        let ss = segment::SegmentSelector::read_ss();
        let ds = segment::SegmentSelector::read_ds();
        let es = segment::SegmentSelector::read_es();
        let tr = segment::SegmentSelector::read_tr();
        let ldtr = segment::SegmentSelector::read_ldtr();
        let fs = segment::SegmentSelector::read_fs();
        let gs = segment::SegmentSelector::read_gs();

        guest::CsSelector::from_selector(&cs).write()?;
        guest::SsSelector::from_selector(&ss).write()?;
        guest::DsSelector::from_selector(&ds).write()?;
        guest::EsSelector::from_selector(&es).write()?;
        guest::TrSelector::from_selector(&tr).write()?;
        guest::LdtrSelector::from_selector(&ldtr).write()?;
        guest::FsSelector::from_selector(&fs).write()?;
        guest::GsSelector::from_selector(&gs).write()?;

        let gdtr = segment::Gdtr::sgdt();
        let idtr = segment::Idtr::sidt();

        guest::CsBase::from_raw(segment::segment_base(&gdtr, &cs)).write()?;
        guest::SsBase::from_raw(segment::segment_base(&gdtr, &ss)).write()?;
        guest::DsBase::from_raw(segment::segment_base(&gdtr, &ds)).write()?;
        guest::EsBase::from_raw(segment::segment_base(&gdtr, &es)).write()?;
        guest::TrBase::from_raw(segment::segment_base(&gdtr, &tr)).write()?;
        guest::LdtrBase::from_raw(segment::segment_base(&gdtr, &ldtr)).write()?;
        guest::FsBase::from_raw(msr::FsBase::read().as_raw()).write()?;
        guest::GsBase::from_raw(msr::GsBase::read().as_raw()).write()?;

        guest::CsLimit::from_raw(segment::segment_limit(&cs)).write()?;
        guest::SsLimit::from_raw(segment::segment_limit(&ss)).write()?;
        guest::DsLimit::from_raw(segment::segment_limit(&ds)).write()?;
        guest::EsLimit::from_raw(segment::segment_limit(&es)).write()?;
        guest::TrLimit::from_raw(segment::segment_limit(&tr)).write()?;
        guest::LdtrLimit::from_raw(segment::segment_limit(&ldtr)).write()?;
        guest::FsLimit::from_raw(segment::segment_limit(&fs)).write()?;
        guest::GsLimit::from_raw(segment::segment_limit(&gs)).write()?;

        guest::CsAccessRights::from_raw(segment::segment_access(&gdtr, &cs).raw()).write()?;
        guest::SsAccessRights::from_raw(segment::segment_access(&gdtr, &ss).raw()).write()?;
        guest::DsAccessRights::from_raw(segment::segment_access(&gdtr, &ds).raw()).write()?;
        guest::EsAccessRights::from_raw(segment::segment_access(&gdtr, &es).raw()).write()?;
        guest::TrAccessRights::from_raw(segment::segment_access(&gdtr, &tr).raw()).write()?;
        guest::LdtrAccessRights::from_raw(segment::segment_access(&gdtr, &ldtr).raw()).write()?;
        guest::FsAccessRights::from_raw(segment::segment_access(&gdtr, &fs).raw()).write()?;
        guest::GsAccessRights::from_raw(segment::segment_access(&gdtr, &gs).raw()).write()?;

        guest::GdtrBase::from_raw(gdtr.base_address).write()?;
        guest::IdtrBase::from_raw(idtr.base_address).write()?;
        guest::GdtrLimit::from_raw(gdtr.limit as u32).write()?;
        guest::IdtrLimit::from_raw(idtr.limit as u32).write()?;

        guest::Ia32SysenterCs::from_raw(msr::Ia32SysenterCs::read().as_raw() as u32).write()?;
        guest::Ia32SysenterEsp::from_raw(msr::Ia32SysenterEsp::read().as_raw()).write()?;
        guest::Ia32SysenterEip::from_raw(msr::Ia32SysenterEip::read().as_raw()).write()?;

        guest::Ia32DebugctlRegister::from_raw(msr::Ia32Debugctl::read().as_raw()).write()?;
        guest::Ia32Pat::from_raw(msr::Ia32Pat::read().as_raw()).write()?;
        guest::Ia32PerfGlobalCtrl::from_raw(msr::Ia32PerfGlobalCtrl::read().as_raw()).write()?;

        guest::ActivityState::from_raw(0).write()?;
        guest::VmxInterruptibilityState::default().write()?;
        guest::VmxPendingDebugExceptions::default().write()?;
        guest::LinkPointer::from_raw(u64::MAX).write()?;

        Ok(())
    }
}
