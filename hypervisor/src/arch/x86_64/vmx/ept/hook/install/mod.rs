//! EPT hook installation orchestration.
//!
//! `install` only routes the request: address/capability validation lives in
//! [`validate`], entry/page construction in [`build`], and teardown in [`remove`].

use super::super::state;
use super::support::*;
use super::sync::{apply_pending_logged, bump_generation};
use super::*;

mod build;
mod validate;

use build::*;
use validate::*;

pub fn install(
    vmx: &mut VmxVcpu,
    cpu_id: u32,
    va: u64,
    kind: HookKind,
    handler: u64,
    trampoline: u64,
) -> Result<(), HookError> {
    let target = validate::resolve(vmx, va, kind, handler, trampoline)?;
    let mut table = HOOKS.lock();

    if let Some(index) = state::find_page(&table[..], target.page_gpa) {
        return install_on_existing_page(vmx, cpu_id, &target, kind, &mut table, index);
    }

    install_new_slot(vmx, cpu_id, &target, kind, handler, trampoline, &mut table)
}

/// Add one more breakpoint offset to a page that already has a matching slot.
fn install_on_existing_page(
    vmx: &mut VmxVcpu,
    cpu_id: u32,
    target: &InstallTarget,
    kind: HookKind,
    table: &mut [HookSlot; MAX_HOOKS],
    index: usize,
) -> Result<(), HookError> {
    if table[index].kind != kind {
        return Err(HookError::WrongPageKind);
    }

    // Byte-patch mode has exactly one saved byte per slot; a second breakpoint
    // on the same page would overwrite the clean copy and make restore ambiguous.
    if table[index].byte_mode {
        return Err(HookError::AlreadyExists);
    }

    if matches!(kind, HookKind::DetourInternal | HookKind::DetourTrampoline)
        || kind == HookKind::Hide
    {
        return Err(HookError::AlreadyExists);
    }

    if table[index].has_breakpoint(target.offset) {
        return Err(HookError::AlreadyExists);
    }

    if table[index].offset_count as usize >= MAX_BREAKPOINTS_PER_PAGE {
        return Err(HookError::NoFreeSlot);
    }

    // A real 0xCC would trap again after the MTF single-step, so the guest
    // could never make progress.
    if read_gpa_byte(target.page_gpa + target.offset as u64)? == 0xCC {
        return Err(HookError::AlreadyExists);
    }

    if !table[index].add_breakpoint(target.offset) {
        return Err(HookError::AlreadyExists);
    }

    let slot = &mut table[index];
    if let Err(error) = rebuild_fake_page(&mut slot.entry, &mut slot.fake) {
        slot.remove_breakpoint(target.offset);
        return Err(error);
    }

    let generation = bump_generation();
    table[index].generation = generation;

    let installed_va = table[index].page_va + target.offset as u64;
    let breakpoints = table[index].offset_count;

    apply_pending_logged(cpu_id, vmx, table, generation);

    klog!(
        "EPT breakpoint installed: va={:#x} gpa={:#x} breakpoints={}",
        installed_va,
        target.page_gpa,
        breakpoints
    );

    Ok(())
}

/// Allocate a new slot and turn the resolved target into a live hook entry.
fn install_new_slot(
    vmx: &mut VmxVcpu,
    cpu_id: u32,
    target: &InstallTarget,
    kind: HookKind,
    handler: u64,
    trampoline: u64,
    table: &mut [HookSlot; MAX_HOOKS],
) -> Result<(), HookError> {
    // A cross-page detour takes over the second page's leaf too, so it cannot
    // coexist with an existing hook on either page.
    if target.page2_gpa != 0 && state::find_page(&table[..], target.page2_gpa).is_some() {
        return Err(HookError::AlreadyExists);
    }

    let index = state::find_free_slot(&table[..], target.page_gpa, target.page2_gpa)
        .ok_or(HookError::NoFreeSlot)?;

    // A retired slot can be reused; waits referring to its old page/offset must
    // not survive.
    WAITS.lock().cancel_slot(index);

    let reuse_original_pte = reusable_original_pte(&table[index], target);

    if kind != HookKind::Hide {
        ensure_page(&mut table[index].fake)?;
    }
    if kind.has_detour_trampoline() {
        ensure_page(&mut table[index].trampoline)?;
    }
    if target.page2_gpa != 0 {
        ensure_page(&mut table[index].fake2)?;
    }

    let original_pte =
        resolve_original_pte(vmx, table, index, target.page_gpa, reuse_original_pte)?;
    let original_pte2 = if target.page2_gpa == 0 {
        0
    } else {
        resolve_original_pte(vmx, table, index, target.page2_gpa, reuse_original_pte)?
    };

    let mut draft = new_entry(target, kind, &table[index], original_pte, original_pte2);

    if !matches!(kind, HookKind::Hide | HookKind::DetourTrampoline)
        && read_gpa_byte(target.page_gpa + target.offset as u64)? == 0xCC
    {
        return Err(HookError::AlreadyExists);
    }

    prepare_user_byte_patch(target, kind, &mut draft, &mut table[index])?;
    build_payload(
        target,
        kind,
        handler,
        trampoline,
        &mut draft,
        &mut table[index],
        index,
    )?;

    let generation = bump_generation();
    draft.active = true;
    draft.generation = generation;

    let detour_va = draft.detour_va;
    let stolen_len = draft.detour_stolen_len;
    let origin_va = draft.trampoline_va;

    let fake = table[index].fake;
    let fake2 = table[index].fake2;
    let trampoline_page = table[index].trampoline;

    table[index] = HookSlot {
        entry: draft,
        fake,
        fake2,
        trampoline: trampoline_page,
    };

    apply_pending_logged(cpu_id, vmx, table, generation);

    log_installed(
        kind,
        table,
        index,
        target,
        handler,
        origin_va,
        detour_va,
        stolen_len,
        target.page2_gpa,
    );

    Ok(())
}

fn reusable_original_pte(slot: &HookSlot, target: &InstallTarget) -> bool {
    !slot.entry.active
        && slot.entry.retired_generation != 0
        && slot.entry.page_gpa == target.page_gpa
        && slot.entry.page2_gpa == target.page2_gpa
        && slot.entry.original_pte != 0
        && (target.page2_gpa == 0 || slot.entry.original_pte2 != 0)
}

fn resolve_original_pte(
    vmx: &mut VmxVcpu,
    table: &[HookSlot; MAX_HOOKS],
    index: usize,
    page_gpa: u64,
    reuse: bool,
) -> Result<u64, HookError> {
    if reuse {
        let pte = if page_gpa == table[index].entry.page_gpa {
            table[index].entry.original_pte
        } else {
            table[index].entry.original_pte2
        };
        if pte != 0 {
            return Ok(pte);
        }
    }

    if let Some(pte) = existing_original_pte(&table[..], page_gpa) {
        return Ok(pte);
    }

    vmx.ept
        .split_pde(PhysAddr::new(page_gpa))
        .map_err(|_| HookError::NoMemory)?;

    vmx.ept
        .pte_raw(PhysAddr::new(page_gpa))
        .map_err(|_| HookError::NoMemory)?
        .ok_or(HookError::NoMemory)
}

#[allow(clippy::too_many_arguments)]
fn log_installed(
    kind: HookKind,
    table: &[HookSlot; MAX_HOOKS],
    index: usize,
    target: &InstallTarget,
    handler: u64,
    origin_va: u64,
    detour_va: u64,
    stolen_len: u16,
    page2_gpa: u64,
) {
    match kind {
        HookKind::Breakpoint => klog!(
            "EPT breakpoint installed: va={:#x} gpa={:#x}",
            table[index].page_va + target.offset as u64,
            target.page_gpa
        ),
        HookKind::UserBreakpoint => klog!(
            "user breakpoint installed: va={:#x} gpa={:#x} slot={index} orig_pfn={:#x} fake_pfn={:#x} \
             cr3={:#x} (byte patch; stops on INT3 and waits for debugger)",
            table[index].page_va + target.offset as u64,
            target.page_gpa,
            (table[index].original_pte >> 12) & 0xFFFF_FFFF,
            table[index].fake_pfn,
            target.cr3
        ),
        HookKind::DetourInternal => klog!(
            "EPT hook installed: va={:#x} gpa={:#x} gpa2={:#x} stolen={} bytes",
            detour_va,
            target.page_gpa,
            page2_gpa,
            stolen_len
        ),
        HookKind::DetourTrampoline => klog!(
            "EPT proxy hook installed: va={:#x} proxy={:#x} origin={:#x} stolen={} \
             gpa={:#x} gpa2={:#x}",
            detour_va,
            handler,
            origin_va,
            stolen_len,
            target.page_gpa,
            page2_gpa
        ),
        HookKind::Hide => klog!(
            "EPT hidden page installed: va={:#x} gpa={:#x}",
            table[index].page_va,
            target.page_gpa
        ),
    }
}

mod remove;
pub(crate) use remove::remove_breakpoint_offset;
pub use remove::{remove, remove_page};
