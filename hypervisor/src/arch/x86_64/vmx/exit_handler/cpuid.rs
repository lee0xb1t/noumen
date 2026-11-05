use crate::arch::x86_64::{guest_context::GuestContext, resume::EmulationResult, vcpu::Vcpu};

use super::skip_instruction;

pub(super) fn emulate_cpuid(vcpu: &mut Vcpu, context: &mut GuestContext) -> EmulationResult<()> {
    let leaf = context.rax as u32;
    let cpuid_res = x86::cpuid::native_cpuid::cpuid_count(leaf, context.rcx as u32);

    let (mut eax, mut ebx, mut ecx, mut edx) =
        (cpuid_res.eax, cpuid_res.ebx, cpuid_res.ecx, cpuid_res.edx);

    // CPUID.01H:ECX[5] is VMX; bit 31 is "hypervisor present". Never expose
    // either: a guest that sees VMX=0 must not see a hypervisor signature.
    if leaf == 1 {
        ecx &= !((1 << 5) | (1 << 31));
    }

    // CET 只有 VMCS 真正虚拟化后才暴露。
    if leaf == 7 && !vcpu.backend.as_vmx().cached.cet_virtualized {
        match context.rcx as u32 {
            0 => ecx &= !(1 << 7),
            1 => eax &= !((1 << 6) | (1 << 7)),
            _ => {}
        }
    }

    // Hypervisor vendor/feature leaves are not backed by noumen. If the host
    // itself runs under another hypervisor, do not forward its signature.
    if (0x4000_0000..=0x4000_0010).contains(&leaf) {
        eax = 0;
        ebx = 0;
        ecx = 0;
        edx = 0;
    }

    context.rax = eax as u64;
    context.rbx = ebx as u64;
    context.rcx = ecx as u64;
    context.rdx = edx as u64;

    skip_instruction()
}
