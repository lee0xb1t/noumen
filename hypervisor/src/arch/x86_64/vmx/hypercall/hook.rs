use crate::arch::x86_64::{
    guest_context::GuestContext,
    resume::EmulationResult,
    vcpu::Vcpu,
};

#[cfg(feature = "vmx-ept")]
use crate::arch::x86_64::vmx::ept::hook::{self as ept_hook, HookKind};

#[cfg(feature = "vmx-ept")]
pub(super) fn install(context: &mut GuestContext, vcpu: &mut Vcpu) -> EmulationResult<()> {
    let kind = match HookKind::try_from(context.r8 as u32) {
        Ok(kind) => kind,
        Err(error) => {
            context.rax = error.as_u32() as u64;
            return Ok(());
        }
    };

    let cpu_id = vcpu.cpu_id;
    let result = ept_hook::install(
        vcpu.backend.as_vmx_mut(),
        cpu_id,
        context.rdx,
        kind,
        context.r9,
        context.r10,
    );

    context.rax = match result {
        Ok(()) => 0,
        Err(error) => error.as_u32() as u64,
    };

    Ok(())
}

#[cfg(feature = "vmx-ept")]
pub(super) fn remove(context: &mut GuestContext, vcpu: &mut Vcpu) -> EmulationResult<()> {
    let result = ept_hook::remove(vcpu.backend.as_vmx_mut(), vcpu.cpu_id, context.rdx);

    context.rax = match result {
        Ok(()) => 0,
        Err(error) => error.as_u32() as u64,
    };

    Ok(())
}

#[cfg(feature = "vmx-ept")]
pub(super) fn remove_page(context: &mut GuestContext, vcpu: &mut Vcpu) -> EmulationResult<()> {
    let result = ept_hook::remove_page(vcpu.backend.as_vmx_mut(), vcpu.cpu_id, context.rdx);

    context.rax = match result {
        Ok(()) => 0,
        Err(error) => error.as_u32() as u64,
    };

    Ok(())
}

#[cfg(feature = "vmx-ept")]
pub(super) fn apply(_context: &mut GuestContext, vcpu: &mut Vcpu) -> EmulationResult<()> {
    ept_hook::sync_now(vcpu.cpu_id, vcpu.backend.as_vmx_mut());

    Ok(())
}

/// `RAX = 1` when every online vCPU has retired the current generation.
///
/// The driver forces `HOOK_APPLY` on every virtualized CPU after removing the
/// proxy hook, then polls this before freeing the origin page.
#[cfg(feature = "vmx-ept")]
pub(super) fn retired(context: &mut GuestContext) -> EmulationResult<()> {
    context.rax = if ept_hook::all_retired() { 1 } else { 0 };

    Ok(())
}

#[cfg(feature = "vmx-ept")]
pub(super) fn commit(context: &mut GuestContext, _vcpu: &mut Vcpu) -> EmulationResult<()> {
    context.rax = ept_hook::commit_byte_patch(context.rdx) as u64;

    Ok(())
}

#[cfg(feature = "vmx-ept")]
pub(super) fn oneshot(context: &mut GuestContext, _vcpu: &mut Vcpu) -> EmulationResult<()> {
    context.rax = ept_hook::set_one_shot(context.rdx, context.r8 != 0) as u64;

    Ok(())
}

#[cfg(feature = "vmx-ept")]
pub(super) fn hit(context: &mut GuestContext, vcpu: &mut Vcpu) -> EmulationResult<()> {
    // stub 不消费返回值；RAX 会从栈上恢复。
    ept_hook::on_hook_hit(context.rdx, context.r8, vcpu.cpu_id)?;
    context.rax = 0;

    Ok(())
}
