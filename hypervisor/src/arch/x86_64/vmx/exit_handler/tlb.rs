use crate::arch::x86_64::{
    guest_context::GuestContext,
    resume::EmulationResult,
    vcpu::Vcpu,
    vmx::instruction::{Invvpid, InvvpidType},
};

use super::skip_instruction;

pub(super) fn emulate_invpcid(vcpu: &mut Vcpu, context: &mut GuestContext) -> EmulationResult<()> {
    let vpid = vcpu.backend.as_vmx().vpid;

    match context.ecx() & 3 {
        2 => Invvpid::new(InvvpidType::SingleContext, vpid, 0),
        _ => Invvpid::new(InvvpidType::SingleContextRetainingGlobals, vpid, 0),
    }
    .invvpid();

    skip_instruction(context)
}
