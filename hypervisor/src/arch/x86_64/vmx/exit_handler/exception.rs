use crate::{
    arch::x86_64::{
        idt,
        resume::{EmulationError, EmulationResult, Exception, Terminate},
        vcpu::Vcpu,
        vmx::{
            exit_info::{VmxExitInterruptionErrorCode, VmxExitInterruptionInformation},
            injection, vmcs,
        },
    },
};

#[cfg(feature = "vmx-ept")]
use crate::klog;

#[cfg(feature = "vmx-ept")]
use crate::arch::x86_64::vmx::ept;

pub(super) fn handle_exception_or_nmi(
    vcpu: &mut Vcpu,
    context: &mut crate::arch::x86_64::guest_context::GuestContext,
) -> EmulationResult<()> {
    #[cfg(not(feature = "vmx-ept"))]
    let _ = context;

    let info = VmxExitInterruptionInformation::read()?;

    // 单步中的指令如果先触发 #PF/#GP 或 NMI，MTF 退出不会到来；在这里撤销
    // pending 并重新 arm，避免该 CPU 之后的 #BP 全部被误判给 guest。
    #[cfg(feature = "vmx-ept")]
    {
        let cpu_id = vcpu.cpu_id;
        let vmx = vcpu.backend.as_vmx_mut();

        if let Some(pending) = vmx.hook_pending {
            let rip = vmcs::guest::Rip::read().map_or(0, |rip| rip.as_raw());

            klog!(
                "diag: exception interrupted single-step vec={} type={} rip={rip:#x} page={:#x} (pending cancelled)",
                info.vector(),
                info.interruption_type(),
                pending.page_gpa
            );

            ept::hook::abort_pending(vmx, cpu_id)?;
        }
    }

    // 异常位图拦截 INT3 时，它以软件异常（type 6）交付。隐藏的 EPT 断点会
    // 消费它；其它情况都是 guest 自己的断点，必须原样注入。
    if info.vector() == idt::ExceptionVector::Breakpoint.as_u8() {
        #[cfg(feature = "vmx-ept")]
        let cpu_id = vcpu.cpu_id;

        #[cfg(feature = "vmx-ept")]
        if ept::hook::handle_breakpoint(vcpu.backend.as_vmx_mut(), context, cpu_id)? {
            return Ok(());
        }

        return Err(EmulationError::Guest(exception_of(info)?));
    }

    match info.interruption_type() {
        t if t == idt::InterruptionType::NonMaskableInterrupt.as_u8() => handle_nmi(vcpu),
        t if t == idt::InterruptionType::HardwareException.as_u8() => {
            handle_guest_exception(vcpu, info)
        }
        _ => Err(EmulationError::Terminate(Terminate::internal())),
    }
}

fn handle_nmi(vcpu: &mut Vcpu) -> EmulationResult<()> {
    vcpu.host.queued_nmis += 1;

    let mut proc_based = vmcs::control::ProcBasedCtls::read()?;
    proc_based.set_nmi_window_exiting(true);
    proc_based.write_adjust()?;

    Ok(())
}

fn handle_guest_exception(
    _vcpu: &mut Vcpu,
    info: VmxExitInterruptionInformation,
) -> EmulationResult<()> {
    match info.vector() {
        _ => Err(EmulationError::Guest(exception_of(info)?)),
    }
}

fn exception_of(info: VmxExitInterruptionInformation) -> EmulationResult<Exception> {
    let vector = idt::ExceptionVector::from_u8(info.vector())
        .ok_or_else(|| EmulationError::Terminate(Terminate::internal()))?;

    // Only meaningful when the deliver bit is set; the field is stale otherwise.
    let error_code = if info.deliver_error_code() {
        Some(VmxExitInterruptionErrorCode::read()?.as_u32())
    } else {
        None
    };

    Ok(Exception::new(vector, error_code))
}

pub(super) fn handle_nmi_window(vcpu: &mut Vcpu) -> EmulationResult<()> {
    // An NMI-window exit only means that the guest is able to take an NMI at
    // this point; it does not guarantee that one is actually queued. Guard the
    // decrement, otherwise it underflows to `u64::MAX` and we keep injecting
    // NMIs forever.
    if vcpu.host.queued_nmis > 0 {
        vcpu.host.queued_nmis -= 1;

        // inject the NMI into the guest
        injection::inject_nmi()?;
    }

    // Keep NMI-window exiting armed only while NMIs are still queued. Do it
    // with a single read-modify-write, and only when the bit has to change so
    // that a spurious window exit does not rewrite the control field.
    let window_needed = vcpu.host.queued_nmis > 0;
    let mut proc_based = vmcs::control::ProcBasedCtls::read()?;
    if proc_based.nmi_window_exiting() != window_needed {
        proc_based.set_nmi_window_exiting(window_needed);
        proc_based.write_adjust()?;
    }

    Ok(())
}
