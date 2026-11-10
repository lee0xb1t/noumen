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
pub(super) fn skip_instruction() -> EmulationResult<()> {
    advance_rip()?;
    finish_instruction()
}

/// Advance `GUEST_RIP` by the current VM-exit instruction length.
fn advance_rip() -> EmulationResult<()> {
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

    Ok(())
}

/// Finish architectural side effects of an emulated instruction.
fn finish_instruction() -> EmulationResult<()> {
    // Only rewrite the interruptibility field when the instruction was in an
    // STI/MOV-SS shadow. The common case is zero, so avoid a VMWRITE.
    let mut interruptibility_state = vmcs::guest::VmxInterruptibilityState::read()?;

    if interruptibility_state.as_u32() & 0b11 != 0 {
        interruptibility_state.set_blocking_by_mov_ss(false);
        interruptibility_state.set_blocking_by_sti(false);
        interruptibility_state.write()?;
    }

    let rflags = vmcs::guest::Rflags::read()?;

    // Single-step is the only remaining reason to touch DEBUGCTL/pending #DB.
    if !rflags.trap_flag() {
        return Ok(());
    }

    let debug_ctl = vmcs::guest::Ia32DebugctlRegister::read()?;

    if debug_ctl.btf() {
        return Ok(());
    }

    let mut pending_debug = vmcs::guest::VmxPendingDebugExceptions::read()?;
    pending_debug.set_bs(true);
    pending_debug.write()?;

    Ok(())
}
