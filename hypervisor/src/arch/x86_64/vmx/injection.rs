//! VM-entry event injection: the field is delivered on the next VM entry, so
//! `inject` is the single exception writer and runs once per exit.

use crate::arch::x86_64::{
    idt::{self},
    resume::{Exception, TerminateResult},
    vmx::{exit_info::VmxExitInstructionLength, vmcs::control},
};

pub fn inject_nmi() -> TerminateResult<()> {
    let mut interrupt_info = control::InterruptInformation::default();

    interrupt_info.set_vector(idt::ExceptionVector::Nmi.as_u8());
    interrupt_info.set_interruption_type(idt::InterruptionType::NonMaskableInterrupt.as_u8());
    interrupt_info.set_deliver_error_code(false);
    interrupt_info.set_valid(true);
    interrupt_info.write()?;

    Ok(())
}

pub fn inject(event: Exception) -> TerminateResult<()> {
    let mut info = control::InterruptInformation::default();

    // The valid bit is what makes the CPU act on the field, so write it last.
    if let Some(code) = event.error_code {
        info.write_error_code(code)?;
    }

    // 重要：VT-x 要求 #BP 和 #OF 以软件异常注入；用硬件异常（type 3）注入
    // 两者中的任何一个都会被拒绝。它们都是 trap 语义：VM exit 保存的 guest
    // RIP 仍指向 INT3/INTO 本身，因此 VM entry 必须被告知指令长度，让压栈的
    // 返回地址指向下一条指令。
    let software_exception = matches!(
        event.vector,
        idt::ExceptionVector::Breakpoint | idt::ExceptionVector::Overflow
    );

    info.set_vector(event.vector.as_u8());
    info.set_interruption_type(if software_exception {
        idt::InterruptionType::SoftwareException.as_u8()
    } else {
        event.interruption_type().as_u8()
    });
    info.set_deliver_error_code(event.error_code.is_some());
    info.set_valid(true);
    info.write()?;

    if software_exception {
        let length = VmxExitInstructionLength::read()
            .map(|length| length.as_raw() as u32)
            .unwrap_or(0);

        // INT3/INTO 分别宽 1/2 字节；拒绝 0 可以避免退出长度字段过期或不可用
        // 时把 RIP 指回原指令。
        control::VmentryInstructionLength::write(if length == 0 { 1 } else { length })?;
    }

    Ok(())
}
