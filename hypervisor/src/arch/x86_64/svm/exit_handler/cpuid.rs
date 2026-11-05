use crate::arch::x86_64::{guest_context::GuestContext, svm::instruction::advance_rip, vcpu::Vcpu};

pub(super) fn emulate(vcpu: &mut Vcpu, ctx: &mut GuestContext) {
    let leaf = ctx.rax as u32;
    let sub = ctx.rcx as u32;
    let result = x86::cpuid::native_cpuid::cpuid_count(leaf, sub);

    let (mut eax, mut ebx, mut ecx, mut edx) = (result.eax, result.ebx, result.ecx, result.edx);

    match leaf {
        // 不暴露 VT-x；同时清 hypervisor-present 位。
        1 => ecx &= !((1 << 5) | (1 << 31)),
        // CPUID.8000_0001H:ECX[2] = SVM.
        0x8000_0001 => ecx &= !(1 << 2),
        // SVM 能力叶子、SEV 叶子：报告“不存在”。
        0x8000_000a | 0x8000_001f => {
            eax = 0;
            ebx = 0;
            ecx = 0;
            edx = 0;
        }
        // 上层 hypervisor 的签名/TSC 频率叶子：归零，避免暴露嵌套环境。
        0x4000_0000..=0x4000_0010 => {
            eax = 0;
            ebx = 0;
            ecx = 0;
            edx = 0;
        }
        _ => {}
    }

    ctx.rax = eax as u64;
    ctx.rbx = ebx as u64;
    ctx.rcx = ecx as u64;
    ctx.rdx = edx as u64;

    let svm = vcpu.backend.as_svm_mut();
    // CPUID is 0F A2.
    advance_rip(&mut svm.vmcb, 2, svm.cached.nrips);
}
