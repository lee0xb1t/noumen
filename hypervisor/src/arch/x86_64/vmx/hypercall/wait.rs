use crate::arch::x86_64::{
    guest_context::GuestContext,
    resume::EmulationResult,
    vcpu::Vcpu,
};

#[cfg(feature = "vmx-ept")]
use crate::arch::x86_64::guest_memory;

#[cfg(feature = "vmx-ept")]
use crate::arch::x86_64::vmx::ept::hook;

#[cfg(feature = "vmx-ept")]
pub(super) fn list(context: &mut GuestContext, _vcpu: &mut Vcpu) -> EmulationResult<()> {
    let destination = context.rdx;
    let capacity = (context.r8 as usize).min(hook::MAX_HOOK_WAITS);

    if capacity == 0 {
        context.rax = 0;
        return Ok(());
    }

    let cr3 = crate::arch::x86_64::vmx::vmcs::guest::Cr3::read()?.as_raw();
    let user = (crate::arch::x86_64::vmx::vmcs::guest::CsSelector::read()?.as_raw() & 3) == 3;
    let write_protect = crate::arch::x86_64::vmx::vmcs::guest::Cr0::read_effective()?.write_protect();
    let mut offset = 0u64;

    // 只读快照：等待项不会因为被读走而消失，调用方 可以反复轮询。
    let copied = hook::wait_list(capacity, |wait| {
        let Some(address) = destination.checked_add(offset) else {
            return false;
        };

        let bytes = unsafe {
            core::slice::from_raw_parts(
                wait as *const hook::HookWait as *const u8,
                core::mem::size_of::<hook::HookWait>(),
            )
        };

        match guest_memory::copy_to_guest(cr3, address, bytes, user, write_protect) {
            Ok(()) => {
                offset += core::mem::size_of::<hook::HookWait>() as u64;
                true
            }
            Err(_) => false,
        }
    });

    context.rax = copied as u64;

    Ok(())
}

#[cfg(feature = "vmx-ept")]
pub(super) fn reply(context: &mut GuestContext, _vcpu: &mut Vcpu) -> EmulationResult<()> {
    context.rax = hook::wait_reply(context.rdx, context.r8 as u32, context.r9) as u64;

    Ok(())
}

#[cfg(feature = "vmx-ept")]
pub(super) fn timeout(context: &mut GuestContext, _vcpu: &mut Vcpu) -> EmulationResult<()> {
    context.rax = hook::set_wait_timeout(context.rdx) as u64;

    Ok(())
}

#[cfg(feature = "vmx-ept")]
pub(super) fn set_reg(context: &mut GuestContext, _vcpu: &mut Vcpu) -> EmulationResult<()> {
    let index = if context.r8 > u8::MAX as u64 {
        u8::MAX
    } else {
        context.r8 as u8
    };

    context.rax = hook::wait_set_reg(context.rdx, index, context.r9) as u64;

    Ok(())
}

#[cfg(feature = "vmx-ept")]
pub(super) fn filter(context: &mut GuestContext, _vcpu: &mut Vcpu) -> EmulationResult<()> {
    context.rax = hook::set_wait_filter(context.rdx, context.r8) as u64;

    Ok(())
}

#[cfg(feature = "vmx-ept")]
pub(super) fn cancel(context: &mut GuestContext, _vcpu: &mut Vcpu) -> EmulationResult<()> {
    context.rax = hook::wait_cancel(context.rdx) as u64;

    Ok(())
}
