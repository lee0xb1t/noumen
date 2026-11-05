use crate::arch::x86_64::{
    guest_context::GuestContext,
    hypercall::PONG_MAGIC,
    resume::EmulationResult,
};

#[cfg(feature = "vmx-ept")]
use crate::arch::x86_64::vcpu::Vcpu;

#[cfg(any(feature = "vmx-ept", feature = "buffer-log"))]
use crate::arch::x86_64::resume::EmulationError;

#[cfg(any(feature = "vmx-ept", feature = "buffer-log"))]
use crate::arch::x86_64::guest_memory;

#[cfg(feature = "vmx-ept")]
use crate::arch::x86_64::{memory::PAGE_SIZE, vmx::ept::hide_self};

#[cfg(feature = "buffer-log")]
use crate::arch::x86_64::{
    guest_memory::MemoryFault,
    idt::ExceptionVector,
    resume::Exception,
    vmx::vmcs::guest,
};

#[cfg(feature = "vmx-ept")]
pub(super) fn hide_self(context: &mut GuestContext, vcpu: &mut Vcpu) -> EmulationResult<()> {
    let cpu_id = vcpu.cpu_id;
    let result = hide_self::hide(vcpu.backend.as_vmx_mut(), cpu_id);

    context.rax = match result {
        Ok(count) => count as u64,
        // HideSelf 的失败码和“隐藏页数”共用 RAX；页数远小于 u32::MAX，
        // 用 u32::MAX 表示失败，避免和 HookError 1..10 混淆。
        Err(_) => u32::MAX as u64,
    };

    Ok(())
}

#[cfg(feature = "vmx-ept")]
pub(super) fn read_self(
    context: &mut GuestContext,
    vcpu: &mut Vcpu,
    via_ept: bool,
) -> EmulationResult<()> {
    let index = context.rdx as u32;
    let destination = context.r8;
    let mut page = [0u8; PAGE_SIZE];

    let copied = if via_ept {
        hide_self::read_ept(vcpu.backend.as_vmx_mut(), index, &mut page)
    } else {
        hide_self::read_root(index, &mut page)
    };

    if copied.is_none() {
        context.rax = 0;
        return Ok(());
    }

    let cr3 = crate::arch::x86_64::vmx::vmcs::guest::Cr3::read()?.as_raw();
    let user = (crate::arch::x86_64::vmx::vmcs::guest::CsSelector::read()?.as_raw() & 3) == 3;
    let write_protect = crate::arch::x86_64::vmx::vmcs::guest::Cr0::read_effective()?.write_protect();

    guest_memory::copy_to_guest(cr3, destination, &page, user, write_protect)
        .map_err(|_| EmulationError::gp(0))?;

    context.rax = PAGE_SIZE as u64;

    Ok(())
}


pub(super) fn ping(context: &mut GuestContext) -> EmulationResult<()> {
    context.rax = PONG_MAGIC ^ context.rdx;
    Ok(())
}

#[cfg(feature = "buffer-log")]
pub(super) fn logger_info(context: &mut GuestContext) -> EmulationResult<()> {
    let stats = crate::log::stats();

    context.rax = stats.total;
    context.rdx = stats.dropped;
    context.r8 = stats.buffered as u64;

    Ok(())
}

#[cfg(feature = "buffer-log")]
pub(super) fn flush_logs(context: &mut GuestContext) -> EmulationResult<()> {
    let destination = context.rdx;
    let capacity = context.r8.min(crate::log::MAX_MSG_COUNT as u64) as u32;

    // Zero capacity is an existence probe; nothing to translate.
    if capacity == 0 {
        context.rax = 0;
        return Ok(());
    }

    let cr3 = guest::Cr3::read()?.as_raw();
    let user = (guest::CsSelector::read()?.as_raw() & 3) == 3;
    let write_protect = guest::Cr0::read_effective()?.write_protect();

    let mut fault = None;
    let mut offset = 0u64;

    let copied = crate::log::flush(capacity, |message| {
        let Some(address) = destination.checked_add(offset) else {
            fault = Some(MemoryFault::NonCanonical);
            return false;
        };

        match guest_memory::copy_to_guest(cr3, address, message.as_bytes(), user, write_protect) {
            Ok(()) => {
                offset += crate::log::LOGGER_MSG_SIZE as u64;
                true
            }
            Err(error) => {
                fault = Some(error);
                false
            }
        }
    });

    match copied {
        Some(count) => {
            context.rax = count as u64;
            Ok(())
        }
        None => Err(memory_fault_to_exception(
            context,
            fault.unwrap_or(MemoryFault::NonCanonical),
        )),
    }
}

/// Maps a copy failure to the guest exception.
#[cfg(feature = "buffer-log")]
fn memory_fault_to_exception(context: &mut GuestContext, fault: MemoryFault) -> EmulationError {
    match fault {
        MemoryFault::NonCanonical => EmulationError::gp(0),

        MemoryFault::Page(page) => {
            // #PF handler reads this from CR2; the stub writes it back.
            context.cr2 = page.address;

            let mut error_code = 0;
            if page.present {
                error_code |= 1 << 0;
            }
            if page.write {
                error_code |= 1 << 1;
            }
            if page.user {
                error_code |= 1 << 2;
            }

            EmulationError::Guest(Exception::new(ExceptionVector::PageFault, Some(error_code)))
        }
    }
}
