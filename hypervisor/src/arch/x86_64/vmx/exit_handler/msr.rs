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

        return skip_instruction(context);
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

        return skip_instruction(context);
    }

    // `enable_vmx()` locked the hardware MSR and set VMXON-outside-SMX, so
    // reading it directly would advertise VT-x to the guest. Return the value
    // cached before virtualization instead: lock bit set, VMX/SENTER bits
    // clear, i.e. exactly what `emulate_vmxon` claims when it injects #GP(0).
    if msr == x86::msr::IA32_FEATURE_CONTROL {
        let value = vcpu.backend.as_vmx().cached.feature_control;

        context.rax = value & 0xffff_ffff;
        context.rdx = (value >> 32) & 0xffff_ffff;

        return skip_instruction(context);
    }

    // VMX capability MSRs exist only when CPUID.01H:ECX.VMX is set. The guest
    // sees VMX=0, so probing them must fault exactly as on a CPU without VT-x;
    // never let the host's capability set leak through.
    if (0x480..=0x491).contains(&msr) {
        return Err(EmulationError::gp(0));
    }

    // Strict default: do not expose the outer hypervisor's MSR range.
    #[cfg(not(feature = "hypervisor-msr"))]
    if is_hypervisor_msr(msr) {
        return Err(EmulationError::gp(0));
    }

    // Hypervisor-reserved MSRs cannot be hidden from a Windows guest that
    // already discovered the outer hypervisor at boot. The kdmapper failure
    // was Windows executing RDMSR 0x4000_00f0 (and WRMSR 0x4000_00b0/B1);
    // turning those into #GP produced 0x1E/0x3D with
    // STATUS_PRIVILEGED_INSTRUCTION. With `hypervisor-msr` enabled, try the
    // real MSR first, then fall back to a benign zero for this range.
    let mut exception_info = HostExceptionInfo::new();
    let msr_value = safe_routine::rdmsr_safe(&mut exception_info, msr);

    // An unsupported MSR is #GP(0) on real hardware. Hypervisor MSRs are the
    // exception: a Windows guest may have been told about them by the outer
    // hypervisor before noumen started, and cannot recover from this #GP.
    if exception_info.has_occurred {
        if is_hypervisor_msr(msr) {
            context.rax = 0;
            context.rdx = 0;

            return skip_instruction(context);
        }

        return Err(EmulationError::gp(0));
    }

    if let Some(v) = msr_value {
        context.rax = v & 0xffff_ffff;
        context.rdx = (v >> 32) & 0xffff_ffff;
    }

    skip_instruction(context)
}

pub(super) fn emulate_wrmsr(vcpu: &mut Vcpu, context: &mut GuestContext) -> EmulationResult<()> {
    let msr = context.ecx();
    let value = (context.rdx << 32) | context.eax() as u64;

    // APERF/MPERF 不写真实硬件：保留 guest 写入值。
    if msr == x86::msr::IA32_APERF || msr == x86::msr::IA32_MPERF {
        vcpu.backend.as_vmx_mut().perf.set(msr, value);

        return skip_instruction(context);
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
                return skip_instruction(context);
            }
            msr::IA32_INTERRUPT_SSP_TABLE_ADDR => {
                if virtualized {
                    vmcs::guest::Ia32InterruptSspTable::from_raw(value)
                        .write()
                        .map_err(|_| EmulationError::gp(0))?;
                }
                return skip_instruction(context);
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

            return skip_instruction(context);
        }
    }

    // Strict default: reject hypervisor MSRs exactly like the CPUID hiding.
    #[cfg(not(feature = "hypervisor-msr"))]
    if is_hypervisor_msr(msr) {
        return Err(EmulationError::gp(0));
    }

    // With `hypervisor-msr`: pass through so an outer hypervisor can service
    // synthetic timers. If it does not implement one, ignore the write rather
    // than injecting #GP into a Windows guest that already uses this range.
    let mut exception_info = HostExceptionInfo::new();
    safe_routine::wrmsr_safe(&mut exception_info, msr, value);

    if exception_info.has_occurred {
        if is_hypervisor_msr(msr) {
            return skip_instruction(context);
        }

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

    skip_instruction(context)
}

/// Hypervisor-reserved MSRs. Pass through when the outer hypervisor supports
/// them; unsupported access is emulated as zero/ignored instead of #GP because
/// a Windows guest may have learned about this range before noumen started.
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
