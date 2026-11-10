//! VM-exit dispatch and shared exit helpers.
//!
//! Handlers are grouped by exit category in child modules; this file only routes
//! a VM exit to the right group and advances the guest when emulation succeeds.

use super::exit_info::{
    VmxExitInstructionLength, VmxExitQualification, VmxExitReason as ExitReason,
};

use crate::arch::x86_64::{
    guest_context::GuestContext,
    resume::{EmulationError, EmulationResult, Resume, Terminate},
    vcpu::Vcpu,
    vmx::vmcs,
};

mod control_regs;
mod cpuid;
mod ept;
mod exception;
mod misc;
mod msr;
mod tlb;
mod tsc;

#[inline]
pub fn dispatch_vm_exit(context: &mut GuestContext, vcpu: &mut Vcpu, reason: ExitReason) -> Resume {
    // A Hide single-step is only valid for the instruction that armed it. Any
    // unrelated exit invalidates the shadow page; abort it before dispatch.
    #[cfg(feature = "vmx-ept")]
    if let Err(error) = ept::abort_hide_step(vcpu, reason) {
        return error.into();
    }

    // APERF/MPERF 按需同步：只在 guest 真读时累加，退出路径不花时间。

    let result: EmulationResult<()> = match reason.basic_exit_reason() as u32 {
        // CPU state and exceptions.
        ExitReason::EXCEPTION_OR_NMI => exception::handle_exception_or_nmi(vcpu, context),
        ExitReason::TRIPLE_FAULT => Err(unhandled_exit(reason)),
        ExitReason::NMI_WINDOW => exception::handle_nmi_window(vcpu),
        ExitReason::MOV_CR => control_regs::handle_mov_cr(vcpu, context),
        // 没有任何代码置 `activate_vmx_preemption_timer`, 这条退出不该出现.
        // 真出现说明 pin-based 控制位被写坏, 吞掉只会让 guest 看到一次没有
        // 任何来源的停顿 -- 和 MTF 一样按内部错误停机.
        ExitReason::VMX_PREEMPTION_TIMER_EXPIRED => {
            Err(EmulationError::Terminate(Terminate::internal()))
        }

        // Memory virtualization.
        ExitReason::EPT_VIOLATION => ept::handle_ept_violation(vcpu, context),
        ExitReason::MONITOR_TRAP_FLAG => ept::handle_monitor_trap_flag(vcpu, context),
        ExitReason::EPT_MISCONFIGURATION => ept::handle_ept_misconfiguration(vcpu, context),
        ExitReason::EXECUTE_INVPCID => tlb::emulate_invpcid(vcpu, context),

        // Instruction emulation.
        ExitReason::EXECUTE_HLT => Err(unhandled_exit(reason)),
        ExitReason::EXECUTE_GETSEC => misc::emulate_getsec(vcpu),
        ExitReason::EXECUTE_INVD => misc::emulate_invd(vcpu),
        ExitReason::EXECUTE_CPUID => cpuid::emulate_cpuid(vcpu, context),
        ExitReason::EXECUTE_RDMSR => msr::emulate_rdmsr(vcpu, context),
        ExitReason::EXECUTE_WRMSR => msr::emulate_wrmsr(vcpu, context),
        ExitReason::EXECUTE_XSETBV => misc::emulate_xsetbv(vcpu, context),
        ExitReason::EXECUTE_RDTSC => tsc::emulate_rdtsc(vcpu, context),
        ExitReason::EXECUTE_RDTSCP => tsc::emulate_rdtscp(vcpu, context),
        ExitReason::EXECUTE_VMXON => misc::emulate_vmxon(vcpu, context),
        ExitReason::EXECUTE_VMCALL => misc::emulate_vmcall(vcpu, context),

        // Nested-VMX attempts (except VMXON/VMCALL).
        ExitReason::EXECUTE_INVEPT
        | ExitReason::EXECUTE_INVVPID
        | ExitReason::EXECUTE_VMCLEAR
        | ExitReason::EXECUTE_VMLAUNCH
        | ExitReason::EXECUTE_VMPTRLD
        | ExitReason::EXECUTE_VMPTRST
        | ExitReason::EXECUTE_VMREAD
        | ExitReason::EXECUTE_VMRESUME
        | ExitReason::EXECUTE_VMWRITE
        | ExitReason::EXECUTE_VMXOFF
        | ExitReason::EXECUTE_VMFUNC => misc::handle_vmx_instruction(vcpu, context),

        _ => Err(unhandled_exit(reason)),
    };

    match result {
        Ok(()) => Resume::Continue,
        Err(e) => Resume::from(e),
    }
}

fn unhandled_exit(reason: ExitReason) -> EmulationError {
    EmulationError::Terminate(Terminate::UnhandledExit {
        reason: reason.as_raw(),
        qualification: VmxExitQualification::read().map_or(0, |q| q.as_raw()),
        guest_rip: vmcs::guest::Rip::read().map_or(0, |r| r.as_raw()),
        guest_rsp: vmcs::guest::Rsp::read().map_or(0, |r| r.as_raw()),
    })
}

/// Advance the guest past the instruction that caused the current VM exit.
#[inline]
pub(super) fn skip_instruction(context: &GuestContext) -> EmulationResult<()> {
    let new_rip = advance_rip()?;
    finish_instruction(context, new_rip)
}

/// Advance `GUEST_RIP` by the current VM-exit instruction length.
#[inline]
fn advance_rip() -> EmulationResult<u64> {
    let guest_rip = vmcs::guest::Rip::read()?;
    let inst_len = VmxExitInstructionLength::read()?;

    let old_rip = guest_rip.as_raw();
    let mut new_rip = old_rip + inst_len.as_raw();

    // handle wrap-around for 32-bit addresses
    // https://patchwork.kernel.org/project/kvm/patch/20200427165917.31799-1-pbonzini@redhat.com/
    if old_rip < (1u64 << 32) && new_rip >= (1u64 << 32) {
        let guest_cs_ar = vmcs::guest::CsAccessRights::read()?;
        if !guest_cs_ar.long_mode() {
            new_rip &= 0xFFFF_FFFF;
        }
    }

    vmcs::guest::Rip::from_raw(new_rip).write()?;

    Ok(new_rip)
}

/// Finish architectural side effects of an emulated instruction.
#[inline]
fn finish_instruction(context: &GuestContext, new_rip: u64) -> EmulationResult<()> {
    // Only rewrite the interruptibility field when the instruction was in an
    // STI/MOV-SS shadow. The common case is zero, so avoid a VMWRITE.
    let mut interruptibility_state = vmcs::guest::VmxInterruptibilityState::read()?;

    if interruptibility_state.as_u32() & 0b11 != 0 {
        interruptibility_state.set_blocking_by_mov_ss(false);
        interruptibility_state.set_blocking_by_sti(false);
        interruptibility_state.write()?;
    }

    let rflags = vmcs::guest::Rflags::read()?;
    let mut need_bs = false;

    if rflags.trap_flag() {
        let debug_ctl = vmcs::guest::Ia32DebugctlRegister::read()?;
        need_bs = !debug_ctl.btf();
    }

    // Merge enabled execution breakpoints (DR7 RW=00) that match the next RIP
    // into pending debug exceptions. DR0-DR3 are not VMCS fields; the exit stub
    // saved the guest values in GuestContext before zeroing the hardware DRs.
    let dr7 = vmcs::guest::Dr7::read()?.as_raw();
    let mut bp_matched = 0u64;

    for i in 0..4 {
        let enable = (1u64 << (i * 2)) | (1u64 << (i * 2 + 1));
        if dr7 & enable == 0 {
            continue;
        }

        if (dr7 >> (16 + i * 4)) & 0b11 != 0 {
            continue;
        }

        let dr = match i {
            0 => context.dr0,
            1 => context.dr1,
            2 => context.dr2,
            _ => context.dr3,
        };

        if dr == new_rip {
            bp_matched |= 1u64 << i;
        }
    }

    if !need_bs && bp_matched == 0 {
        return Ok(());
    }

    let mut pending_debug = vmcs::guest::VmxPendingDebugExceptions::read()?;

    if need_bs {
        pending_debug.set_bs(true);
    }

    if bp_matched & 1 != 0 {
        pending_debug.set_b0(true);
    }
    if bp_matched & 2 != 0 {
        pending_debug.set_b1(true);
    }
    if bp_matched & 4 != 0 {
        pending_debug.set_b2(true);
    }
    if bp_matched & 8 != 0 {
        pending_debug.set_b3(true);
    }
    if bp_matched != 0 {
        pending_debug.set_enabled_breakpoint(true);
    }

    pending_debug.write()?;

    Ok(())
}
