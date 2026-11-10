//! 指令模拟：`VMCALL`、`GETSEC`、`INVD`、`XSETBV`、`VMXON` 等。
//!
//! TSC 相关的模拟与目标判定在 [`super::tsc`]。

use crate::arch::x86_64::{
    cr,
    guest_context::GuestContext,
    resume::{EmulationError, EmulationResult},
    safe_routine::{self, HostExceptionInfo},
    vcpu::Vcpu,
    vmx::vmcs,
};

use super::skip_instruction;

pub(super) fn handle_vmx_instruction(_: &Vcpu, _: &mut GuestContext) -> EmulationResult<()> {
    Err(EmulationError::ud())
}

pub(super) fn emulate_getsec(_vcpu: &mut Vcpu) -> EmulationResult<()> {
    // SMX is disabled in the IA32_FEATURE_CONTROL we expose.
    Err(EmulationError::gp(0))
}

pub(super) fn emulate_invd(_vcpu: &mut Vcpu) -> EmulationResult<()> {
    // TODO: real INVD (flush the affected lines); denied until then.
    Err(EmulationError::gp(0))
}

pub(super) fn emulate_xsetbv(vcpu: &mut Vcpu, context: &mut GuestContext) -> EmulationResult<()> {
    let vmx_vcpu = vcpu.backend.as_vmx();

    let guest_cr4 = vmcs::guest::Cr4::read_effective()?;
    if !guest_cr4.os_xsave() {
        return Err(EmulationError::ud());
    }

    let new_xcr0 = cr::Xcr0::from_bits(((context.edx() as u64) << 32) | context.eax() as u64);

    // only XCR0 is supported
    if context.ecx() != 0 {
        return Err(EmulationError::gp(0));
    }

    // #GP(0) if trying to set an unsupported bit
    if (new_xcr0.bits() & vmx_vcpu.cached.xcr0_unsupported_mask) != 0 {
        return Err(EmulationError::gp(0));
    }

    // #GP(0) if clearing XCR0.X87
    if !new_xcr0.x87() {
        return Err(EmulationError::gp(0));
    }

    // #GP(0) if XCR0.AVX is 1 while XCRO.SSE is cleared
    if new_xcr0.avx() && !new_xcr0.sse() {
        return Err(EmulationError::gp(0));
    }

    // #GP(0) if XCR0.AVX is clear and XCR0.opmask, XCR0.ZMM_Hi256, or XCR0.Hi16_ZMM is set
    if !new_xcr0.avx() && (new_xcr0.opmask() || new_xcr0.zmm_hi16() || new_xcr0.zmm_hi256()) {
        return Err(EmulationError::gp(0));
    }

    // #GP(0) if setting XCR0.BNDREG or XCR0.BNDCSR while not setting the other
    if new_xcr0.bndreg() != new_xcr0.bndcsr() {
        return Err(EmulationError::gp(0));
    }

    // #GP(0) if setting XCR0.opmask, XCR0.ZMM_Hi256, or XCR0.Hi16_ZMM while not setting all of them
    if new_xcr0.opmask() != new_xcr0.zmm_hi256() || new_xcr0.zmm_hi256() != new_xcr0.zmm_hi16() {
        return Err(EmulationError::gp(0));
    }

    let exception_info = HostExceptionInfo::new();
    safe_routine::xsetbv_safe(&exception_info, context.ecx(), new_xcr0.bits());

    if exception_info.has_occurred {
        return Err(EmulationError::gp(0));
    }

    skip_instruction(context)
}

pub(super) fn emulate_vmxon(_: &Vcpu, _: &mut GuestContext) -> EmulationResult<()> {
    // usually a #UD doesn't trigger a vm-exit, but in this case it is possible
    // that CR4.VMXE is 1 while guest shadow CR4.VMXE is 0.
    let guest_cr4 = vmcs::guest::Cr4::read_effective()?;
    if !guest_cr4.vmx_enable() {
        return Err(EmulationError::ud());
    }

    // we are spoofing the value of the IA32_FEATURE_CONTROL MSR in
    // order to convince the guest that VMX has been disabled by BIOS.
    Err(EmulationError::gp(0))
}

pub(super) fn emulate_vmcall(vcpu: &mut Vcpu, context: &mut GuestContext) -> EmulationResult<()> {
    // RIP advances only after the hypercall fully succeeds. STOP 也一样: 先把
    // RIP 推过 VMCALL, 停止之后 guest 才从下一条指令继续.
    match crate::arch::x86_64::vmx::hypercall::dispatch(context, crate::hv::hypercall_key(), vcpu) {
        Ok(()) => skip_instruction(context),
        Err(EmulationError::Stop) => {
            skip_instruction(context)?;
            Err(EmulationError::Stop)
        }
        Err(e) => Err(e),
    }
}
