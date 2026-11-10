use crate::arch::x86_64::{
    guest_context::GuestContext,
    msr,
    resume::{EmulationError, EmulationResult},
    safe_routine::{self, HostExceptionInfo},
    vcpu::Vcpu,
    vmx::vmcs,
};

#[cfg(feature = "vmx-ept")]
use crate::arch::x86_64::vmx::{ept, instruction::Invept};

use super::skip_instruction;

#[inline]
pub(super) fn emulate_rdmsr(vcpu: &mut Vcpu, context: &mut GuestContext) -> EmulationResult<()> {
    let msr = context.ecx();

    // APERF/MPERF 是物理计数器：返回只统计 guest 运行时间的视图（读时同步）。
    // 放在最前面：这两个 MSR 是性能工具最常见的读，先避开后面的分支。
    if msr == x86::msr::IA32_APERF || msr == x86::msr::IA32_MPERF {
        let value = vcpu.backend.as_vmx_mut().perf.value(msr);

        context.rax = value & 0xffff_ffff;
        context.rdx = (value >> 32) & 0xffff_ffff;

        return skip_instruction();
    }

    // CET MSR 没有 VMCS 字段时必须读硬件：CPU 自己会改它们（XRSTOR/SYSRET 会更新
    // U_CET/PL3_SSP），缓存副本会过期，返回陈旧值会让 guest 拿到 SSP=0。
    // 放在 IA32_FEATURE_CONTROL 之前：CET 读比特性控制 MSR 探测更常见。
    if vcpu.backend.as_vmx().cached.cet_supported
        && vcpu.backend.as_vmx().cet.get(msr).is_some()
    {
        let mut exception_info = HostExceptionInfo::new();
        let value = safe_routine::rdmsr_safe(&mut exception_info, msr).unwrap_or(0);

        context.rax = value & 0xffff_ffff;
        context.rdx = (value >> 32) & 0xffff_ffff;

        return skip_instruction();
    }

    // `enable_vmx()` locked the hardware MSR and set VMXON-outside-SMX, so
    // reading it directly would advertise VT-x to the guest. Return the value
    // cached before virtualization instead: lock bit set, VMX/SENTER bits
    // clear, i.e. exactly what `emulate_vmxon` claims when it injects #GP(0).
    if msr == x86::msr::IA32_FEATURE_CONTROL {
        let value = vcpu.backend.as_vmx().cached.feature_control;

        context.rax = value & 0xffff_ffff;
        context.rdx = (value >> 32) & 0xffff_ffff;

        return skip_instruction();
    }

    // VMX capability MSRs exist only when CPUID.01H:ECX.VMX is set. The guest
    // sees VMX=0, so probing them must fault exactly as on a CPU without VT-x;
    // never let the host's capability set leak through.
    if (0x480..=0x491).contains(&msr) {
        return Err(EmulationError::gp(0));
    }

    // Hypervisor-reserved MSRs (0x4000_0000..=0x4000_00ff) have no architectural
    // backing. Real CPUs raise #GP, and WRMSR already reaches the same #GP via
    // `wrmsr_safe`. Returning zero here is a detection oracle: a probe can tell
    // this is a hypervisor by the successful RDMSR (and by the RDMSR/WRMSR
    // asymmetry). Fault like bare metal instead. If a specific VMware backdoor
    // MSR must be emulated, handle that exact MSR explicitly.
    if is_hypervisor_msr(msr) {
        return Err(EmulationError::gp(0));
    }

    let mut exception_info = HostExceptionInfo::new();
    let msr_value = safe_routine::rdmsr_safe(&mut exception_info, msr);

    // An unsupported MSR is #GP(0) on real hardware.
    if exception_info.has_occurred {
        return Err(EmulationError::gp(0));
    }

    if let Some(v) = msr_value {
        context.rax = v & 0xffff_ffff;
        context.rdx = (v >> 32) & 0xffff_ffff;
    }

    skip_instruction()
}

pub(super) fn emulate_wrmsr(vcpu: &mut Vcpu, context: &mut GuestContext) -> EmulationResult<()> {
    let msr = context.ecx();
    let value = (context.rdx << 32) | context.eax() as u64;

    // APERF/MPERF 不写真实硬件：保留 guest 写入值。
    if msr == x86::msr::IA32_APERF || msr == x86::msr::IA32_MPERF {
        vcpu.backend.as_vmx_mut().perf.set(msr, value);

        return skip_instruction();
    }

    // CET MSR 不写真实硬件：已虚拟化时写 VMCS 字段，否则直接丢弃。
    if vcpu.backend.as_vmx().cached.cet_supported {
        let virtualized = vcpu.backend.as_vmx().cached.cet_virtualized;

        match msr {
            msr::IA32_S_CET => {
                if virtualized {
                    vmcs::guest::Ia32SCet::from_raw(value)
                        .write()
                        .map_err(|_| EmulationError::gp(0))?;
                }
                return skip_instruction();
            }
            msr::IA32_INTERRUPT_SSP_TABLE_ADDR => {
                if virtualized {
                    vmcs::guest::Ia32InterruptSspTable::from_raw(value)
                        .write()
                        .map_err(|_| EmulationError::gp(0))?;
                }
                return skip_instruction();
            }
            _ => {}
        }

        // 没有 VMCS 字段：保存并同步到硬件。
        if vcpu.backend.as_vmx().cet.get(msr).is_some() {
            let mut exception_info = HostExceptionInfo::new();
            safe_routine::wrmsr_safe(&mut exception_info, msr, value);

            if exception_info.has_occurred {
                return Err(EmulationError::gp(0));
            }

            vcpu.backend.as_vmx_mut().cet.set(msr, value);

            return skip_instruction();
        }
    }

    // Mirror RDMSR: the hypervisor-reserved range has no real MSR, so fault
    // instead of letting an outer hypervisor's implementation leak through.
    if is_hypervisor_msr(msr) {
        return Err(EmulationError::gp(0));
    }

    let mut exception_info = HostExceptionInfo::new();
    safe_routine::wrmsr_safe(&mut exception_info, msr, value);

    if exception_info.has_occurred {
        return Err(EmulationError::gp(0));
    }

    // MTRR 是全局资源: 改了就重算本核 EPT 的类型, 并把旧翻译刷掉.
    #[cfg(feature = "vmx-ept")]
    if is_mtrr_msr(msr) {
        let vmx_vcpu = vcpu.backend.as_vmx_mut();
        let cache_disable = vmcs::guest::Cr0::read_effective()?.cache_disable();

        // IMPORTANT: MTRR 是 package 级全局资源, EPT 却是每核一份. 这里只更新
        // 发起写的这个核, bump 代数让**其它核**在下一次 VM-exit 自己补; 只刷
        // 自己的 EPTP, 不要用 INVEPT all 去牵连别人.
        let generation = ept::bump_memory_type_generation();

        if !cache_disable {
            vmx_vcpu.ept.update_memory_type();
        }

        Invept::single(vmx_vcpu.ept.eptp().as_raw());
        vmx_vcpu.ept_generation = generation;
    }

    skip_instruction()
}

/// Hypervisor-reserved MSRs. Reads in this range are virtualized and never
/// reach the host, and the guest must not observe a #GP from them.
fn is_hypervisor_msr(msr: u32) -> bool {
    (0x4000_0000..=0x4000_00ff).contains(&msr)
}

#[cfg(feature = "vmx-ept")]
fn is_mtrr_msr(msr: u32) -> bool {
    matches!(
        msr,
        x86::msr::IA32_MTRR_DEF_TYPE
            | x86::msr::IA32_MTRR_FIX64K_00000
            | x86::msr::IA32_MTRR_FIX16K_80000
            | x86::msr::IA32_MTRR_FIX16K_A0000
    ) || (x86::msr::IA32_MTRR_FIX4K_C0000..=x86::msr::IA32_MTRR_FIX4K_F8000).contains(&msr)
        || (x86::msr::IA32_MTRR_PHYSBASE0..=x86::msr::IA32_MTRR_PHYSBASE0 + 511).contains(&msr)
}
