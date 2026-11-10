//! VMX CPUID handler: all CPUID exits go through here (no raw fast path).

use crate::arch::x86_64::{guest_context::GuestContext, resume::EmulationResult, vcpu::Vcpu};

use super::skip_instruction;

#[inline]
pub(super) fn emulate_cpuid(_: &mut Vcpu, context: &mut GuestContext) -> EmulationResult<()> {
    let leaf = context.rax as u32;
    let subleaf = context.rcx as u32;
    let cpuid_res = x86::cpuid::native_cpuid::cpuid_count(leaf, subleaf);

    // Passthrough: write the native CPUID result unchanged. This includes
    // leaves that do not exist on the host. Intel returns the highest basic
    // leaf's data for an out-of-range leaf; AMD may return zeros. Whatever the
    // hardware does is what the guest sees, so invalid leaves such as
    // 0x4000_0000..=0x4000_0010 follow the same rule.
    context.rax = cpuid_res.eax as u64;
    context.rbx = cpuid_res.ebx as u64;
    context.rcx = cpuid_res.ecx as u64;
    context.rdx = cpuid_res.edx as u64;

    skip_instruction(context)
}
