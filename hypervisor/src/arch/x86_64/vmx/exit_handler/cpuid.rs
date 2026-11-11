//! VMX CPUID handler: CPUID always exits, so execute it natively and copy all
//! four result registers back unchanged. No leaf filtering.

use crate::arch::x86_64::{guest_context::GuestContext, resume::EmulationResult, vcpu::Vcpu};

use super::skip_instruction;

#[inline]
pub(super) fn emulate_cpuid(_: &mut Vcpu, context: &mut GuestContext) -> EmulationResult<()> {
    let leaf = context.rax as u32;
    let subleaf = context.rcx as u32;
    let cpuid_res = x86::cpuid::native_cpuid::cpuid_count(leaf, subleaf);

    context.rax = cpuid_res.eax as u64;
    context.rbx = cpuid_res.ebx as u64;
    context.rcx = cpuid_res.ecx as u64;
    context.rdx = cpuid_res.edx as u64;

    skip_instruction(context)
}
