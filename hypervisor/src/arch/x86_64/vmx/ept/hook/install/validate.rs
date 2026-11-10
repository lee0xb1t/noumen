//! Address and capability validation for EPT hook installation.

use crate::{
    arch::x86_64::{
        guest_memory,
        memory::PAGE_SIZE,
        paging::DIRECT_MAP_SIZE,
        vmx::{VmxVcpu, vmcs::guest},
    },
    hv, klog,
};

use super::super::{
    state::{HookError, HookKind, PAGE_MASK},
    support::{detour_needs_second_page, guest_is_user},
};

/// Everything `install` needs after the input has been validated and the
/// target pages have been resolved.
#[derive(Clone, Copy)]
pub(super) struct InstallTarget {
    pub va: u64,
    pub cr3: u64,
    pub page_hint: u64,
    pub page_gpa: u64,
    pub offset: u16,
    pub page2_gpa: u64,
    pub trampoline_phys: u64,
}

pub(super) fn resolve(
    vmx: &VmxVcpu,
    va: u64,
    kind: HookKind,
    handler: u64,
    trampoline: u64,
) -> Result<InstallTarget, HookError> {
    validate_kind(kind)?;

    if !guest_memory::is_canonical(va) {
        return Err(HookError::InvalidAddress);
    }

    let cr3 = guest::Cr3::read()
        .map_err(|_| HookError::NotMapped)?
        .as_raw();
    let user = guest_is_user().map_err(|_| HookError::NotMapped)?;

    let page_hint = page_hint(kind, handler);
    let phys = resolve_target_phys(cr3, va, user, kind, page_hint)?;

    if phys >= DIRECT_MAP_SIZE {
        return Err(HookError::NotMapped);
    }

    let page_gpa = phys & !PAGE_MASK;
    let offset = (phys & PAGE_MASK) as u16;

    // The identity EPT and the host direct map both stop at 512 GiB.
    if page_gpa + PAGE_SIZE as u64 > DIRECT_MAP_SIZE {
        return Err(HookError::NotMapped);
    }

    // User breakpoints depend on CPL3 delivery; a kernel address cannot
    // guarantee that the waiting caller can be scheduled.
    if kind == HookKind::UserBreakpoint && ((va >> 47) & 1) != 0 {
        return Err(HookError::Unsupported);
    }

    validate_detour_handler(cr3, kind, handler, user)?;
    let trampoline_phys = validate_trampoline(cr3, kind, trampoline)?;

    if matches!(kind, HookKind::DetourInternal | HookKind::DetourTrampoline) {
        validate_detour_preconditions(va)?;
    }

    if kind.is_breakpoint() && !vmx.cached.monitor_trap_flag {
        return Err(HookError::UnsupportedCpu);
    }

    let page2_gpa = resolve_second_page(cr3, va, user, kind, page_gpa)?;

    Ok(InstallTarget {
        va,
        cr3,
        page_hint,
        page_gpa,
        offset,
        page2_gpa,
        trampoline_phys,
    })
}

fn validate_kind(kind: HookKind) -> Result<(), HookError> {
    // The hardware-breakpoint path has been removed. Return "unsupported"
    // explicitly instead of silently falling back to another kind.
    if kind == HookKind::Breakpoint {
        return Err(HookError::Unsupported);
    }

    #[cfg(not(feature = "native-bp"))]
    if kind == HookKind::UserBreakpoint {
        return Err(HookError::Unsupported);
    }

    Ok(())
}

/// `handler` means "driver-provided physical page" only for user breakpoints.
fn page_hint(kind: HookKind, handler: u64) -> u64 {
    if kind == HookKind::UserBreakpoint {
        handler & !PAGE_MASK
    } else {
        0
    }
}

fn resolve_target_phys(
    cr3: u64,
    va: u64,
    user: bool,
    kind: HookKind,
    page_hint: u64,
) -> Result<u64, HookError> {
    if kind != HookKind::UserBreakpoint || page_hint == 0 {
        return guest_memory::translate_guest(cr3, va, user, false, false)
            .map_err(|_| HookError::NotMapped);
    }

    // The driver attaches to the target process, locks the page with an MDL and
    // passes that page's physical address in `handler`. Do not trust a
    // translation through the VMCALL-time CR3: under KVA shadow it may be the
    // kernel CR3 where the user page is not mapped at all.
    let translated = guest_memory::translate_guest(cr3, va, user, false, false).ok();

    match translated {
        Some(translated) if (translated & !PAGE_MASK) != page_hint => klog!(
            "user breakpoint: driver page={page_hint:#x} CR3 translated page={:#x} mismatch; installing with driver page va={va:#x}",
            translated & !PAGE_MASK
        ),
        Some(_) => klog!("user breakpoint: using driver page {page_hint:#x} (va={va:#x}, matches translation)"),
        None => klog!("user breakpoint: using driver page {page_hint:#x} (va={va:#x}, CR3 translation failed)"),
    }

    Ok(page_hint | (va & PAGE_MASK))
}

fn validate_detour_handler(
    cr3: u64,
    kind: HookKind,
    handler: u64,
    user: bool,
) -> Result<(), HookError> {
    if kind != HookKind::DetourTrampoline {
        return Ok(());
    }

    if handler == 0 || !guest_memory::is_canonical(handler) {
        return Err(HookError::InvalidAddress);
    }

    // The direct handler runs at guest CPL0, so only the kernel half is valid.
    if user || ((handler >> 47) & 1) == 0 {
        return Err(HookError::Unsupported);
    }

    let handler_phys = guest_memory::translate_guest(cr3, handler, false, false, false)
        .map_err(|_| HookError::NotMapped)?;

    if handler_phys >= DIRECT_MAP_SIZE {
        return Err(HookError::NotMapped);
    }

    Ok(())
}

/// `DetourTrampoline`'s origin page is driver-allocated NonPagedPool. It must be
/// in the kernel half and translate below 512 GiB so the hypervisor can write it.
fn validate_trampoline(cr3: u64, kind: HookKind, trampoline: u64) -> Result<u64, HookError> {
    if kind != HookKind::DetourTrampoline {
        return Ok(0);
    }

    if trampoline == 0 || !guest_memory::is_canonical(trampoline) || ((trampoline >> 47) & 1) == 0 {
        return Err(HookError::InvalidAddress);
    }

    let phys = guest_memory::translate_guest(cr3, trampoline, false, false, false)
        .map_err(|_| HookError::NotMapped)?;

    if phys + PAGE_SIZE as u64 > DIRECT_MAP_SIZE {
        return Err(HookError::NotMapped);
    }

    Ok(phys)
}

fn validate_detour_preconditions(va: u64) -> Result<(), HookError> {
    if (va >> 47) & 1 == 0 {
        return Err(HookError::Unsupported);
    }

    if hv::hypercall_key() == 0 {
        return Err(HookError::HypercallsDisabled);
    }

    Ok(())
}

fn resolve_second_page(
    cr3: u64,
    va: u64,
    user: bool,
    kind: HookKind,
    page_gpa: u64,
) -> Result<u64, HookError> {
    if !matches!(kind, HookKind::DetourInternal | HookKind::DetourTrampoline) {
        return Ok(0);
    }

    let offset = (va & PAGE_MASK) as u16;
    if !detour_needs_second_page(page_gpa, offset)? {
        return Ok(0);
    }

    let va2 = (va & !PAGE_MASK) + PAGE_SIZE as u64;

    if !guest_memory::is_canonical(va2) {
        return Err(HookError::Unsupported);
    }

    let phys2 = guest_memory::translate_guest(cr3, va2, user, false, false)
        .map_err(|_| HookError::NotMapped)?;

    if phys2 >= DIRECT_MAP_SIZE {
        return Err(HookError::NotMapped);
    }

    let page2_gpa = phys2 & !PAGE_MASK;

    if page2_gpa == page_gpa || page2_gpa + PAGE_SIZE as u64 > DIRECT_MAP_SIZE {
        return Err(HookError::NotMapped);
    }

    Ok(page2_gpa)
}
