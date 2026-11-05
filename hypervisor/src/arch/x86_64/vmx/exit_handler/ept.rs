use crate::{
    arch::x86_64::{
        guest_context::GuestContext,
        resume::{EmulationError, EmulationResult, Terminate},
        vcpu::Vcpu,
        vmx::vmcs,
    },
    klog,
};

#[cfg(not(feature = "vmx-ept"))]
use crate::arch::x86_64::vmx::exit_info::VmxExitQualification;

#[cfg(feature = "vmx-ept")]
use crate::arch::x86_64::vmx::{
    ept,
    exit_info::{
        VmxExitGuestLinearAddress, VmxExitGuestPhysicalAddress, VmxExitQualificationEptViolation,
        VmxExitReason as ExitReason,
    },
};

/// A non-MTF exit during a Hide single-step invalidates the shadow page. Abort
/// it before dispatching, but keep breakpoint single-steps intact.
#[cfg(feature = "vmx-ept")]
pub(super) fn abort_hide_step(vcpu: &mut Vcpu, reason: ExitReason) -> EmulationResult<()> {
    let basic = reason.basic_exit_reason() as u32;

    if basic == ExitReason::MONITOR_TRAP_FLAG
        || basic == ExitReason::EPT_VIOLATION
        || basic == ExitReason::EXCEPTION_OR_NMI
    {
        return Ok(());
    }

    let cpu_id = vcpu.cpu_id;
    let vmx = vcpu.backend.as_vmx_mut();

    if let Some(pending) = vmx.hook_pending {
        if pending.hide {
            klog!(
                "diag: non-MTF exit {basic:#x} cancels Hide single-step page={:#x}",
                pending.page_gpa
            );

            ept::hook::abort_pending(vmx, cpu_id)?;
        }
    }

    Ok(())
}

pub(super) fn handle_ept_violation(vcpu: &mut Vcpu, _: &mut GuestContext) -> EmulationResult<()> {
    #[cfg(not(feature = "vmx-ept"))]
    let _ = vcpu;

    #[cfg(feature = "vmx-ept")]
    {
        let qualification = VmxExitQualificationEptViolation::read()?;
        let gpa = VmxExitGuestPhysicalAddress::read()?.as_raw();

        let cpu_id = vcpu.cpu_id;

        if ept::hook::handle_ept_violation(vcpu.backend.as_vmx_mut(), &qualification, gpa, cpu_id)?
        {
            return Ok(());
        }

        // 线性地址只有在 qualification 标记有效时才有意义；未处理的
        // violation 也要记录它，用来区分数据访问和取指。
        let gla = if qualification.guest_linear_address_valid() {
            VmxExitGuestLinearAddress::read().map_or(0, |address| address.as_raw())
        } else {
            0
        };

        // 不在这里补映射：恒等映射的覆盖由 setup 决定，未覆盖的访问直接注入异常。
        static LOGGED: core::sync::atomic::AtomicU32 = core::sync::atomic::AtomicU32::new(0);

        if LOGGED.fetch_add(1, core::sync::atomic::Ordering::Relaxed) < 4 {
            klog!(
                "Unhandled EPT violation: qualification={:#x} gpa={:#x} gla={:#x} rip={:#x}",
                qualification.as_raw(),
                gpa,
                gla,
                vmcs::guest::Rip::read()?.as_raw()
            );
        }
    }

    #[cfg(not(feature = "vmx-ept"))]
    {
        // IMPORTANT: 恒等映射正常情况下不该产生 violation; 真出现了说明 EPT
        // 覆盖或内存类型有问题. 注入 #GP 只是兜底.
        klog!(
            "Unhandled EPT violation: qualification={:#x} rip={:#x}",
            VmxExitQualification::read()?.as_raw(),
            vmcs::guest::Rip::read()?.as_raw()
        );
    }

    Err(EmulationError::gp(0))
}

pub(super) fn handle_monitor_trap_flag(
    vcpu: &mut Vcpu,
    _: &mut GuestContext,
) -> EmulationResult<()> {
    // MTF 的唯一使用者是 EPT 断点单步。其它情况说明控制位被破坏；这里
    // fail-closed，而不是静默吞掉一个没有代码 armed 的退出。
    #[cfg(feature = "vmx-ept")]
    {
        let cpu_id = vcpu.cpu_id;
        ept::hook::handle_monitor_trap(vcpu.backend.as_vmx_mut(), cpu_id)
    }

    #[cfg(not(feature = "vmx-ept"))]
    {
        let _ = vcpu;
        Err(EmulationError::Terminate(Terminate::internal()))
    }
}

pub(super) fn handle_ept_misconfiguration(_: &Vcpu, _: &mut GuestContext) -> EmulationResult<()> {
    // EPT 表结构坏了. 同样只记录并停机, 不要 panic.
    Err(EmulationError::Terminate(Terminate::internal()))
}
