//! Construction of hook entries, fake pages, detours and byte patches.

use crate::arch::x86_64::memory::{PAGE_SIZE, Page};

use super::super::{
    HookSlot, detour,
    state::{HookEntry, HookError, HookKind, MAX_BREAKPOINTS_PER_PAGE, PAGE_MASK},
    support::{copy_from_gpa, poke_range, read_gpa_byte, rebuild_fake_page, write_gpa},
};
use super::validate::InstallTarget;

pub(super) fn new_entry(
    target: &InstallTarget,
    kind: HookKind,
    slot: &HookSlot,
    original_pte: u64,
    original_pte2: u64,
) -> HookEntry {
    let mut entry = HookEntry {
        active: false,
        kind,
        page_gpa: target.page_gpa,
        page_va: target.va & !PAGE_MASK,
        cr3: target.cr3,
        offsets: [0; MAX_BREAKPOINTS_PER_PAGE],
        offset_count: if kind == HookKind::Hide { 0 } else { 1 },
        fake_pfn: if kind == HookKind::Hide {
            0
        } else {
            slot.fake.pfn()
        },
        original_pte,
        page2_gpa: target.page2_gpa,
        fake_pfn2: if target.page2_gpa == 0 {
            0
        } else {
            slot.fake2.pfn()
        },
        original_pte2,
        page_count: if target.page2_gpa == 0 { 1 } else { 2 },
        detour_va: 0,
        handler_va: 0,
        detour_stolen_len: 0,
        trampoline_va: 0,
        hits: 0,
        one_shot: false,
        enabled: true,
        step_hook: false,
        first_hit: None,
        temp_offset: u16::MAX,
        temp_thread: 0,
        noted: 0,
        generation: 0,
        retired_generation: 0,
        byte_mode: false,
        patched_offset: u16::MAX,
        saved_byte: 0,
        pending_patch: false,
    };

    entry.offsets[0] = target.offset;
    entry
}

/// User breakpoints use byte-patch mode when the driver supplied the target
/// physical page. The real byte is patched only after every core has switched
/// to the clean view (`USER_BREAKPOINT_COMMIT`).
pub(super) fn prepare_user_byte_patch(
    target: &InstallTarget,
    kind: HookKind,
    entry: &mut HookEntry,
    slot: &mut HookSlot,
) -> Result<(), HookError> {
    if kind != HookKind::UserBreakpoint || target.page_hint == 0 {
        return Ok(());
    }

    let target_gpa = target.page_gpa + target.offset as u64;
    let saved_byte = read_gpa_byte(target_gpa)?;

    // The clean copy must contain the page as it was before the patch, so the
    // data view reads the original byte.
    copy_from_gpa(
        target.page_gpa,
        slot.fake.as_mut_ptr() as *mut u8,
        PAGE_SIZE,
    )?;
    slot.fake[target.offset as usize] = saved_byte;
    entry.fake_pfn = slot.fake.pfn();

    entry.byte_mode = true;
    entry.patched_offset = target.offset;
    entry.saved_byte = saved_byte;
    entry.pending_patch = true;

    Ok(())
}

pub(super) fn build_payload(
    target: &InstallTarget,
    kind: HookKind,
    handler: u64,
    trampoline: u64,
    entry: &mut HookEntry,
    slot: &mut HookSlot,
    index: usize,
) -> Result<(), HookError> {
    match kind {
        HookKind::Breakpoint => rebuild_fake_page(entry, &mut slot.fake)?,
        HookKind::UserBreakpoint => {
            // Byte-patch mode does not need a fake page; only a cross-page
            // single-step temporary hook uses it.
            if !entry.byte_mode {
                rebuild_fake_page(entry, &mut slot.fake)?;
            }
        }
        HookKind::DetourInternal => build_detour(
            entry,
            &mut slot.fake,
            &mut slot.fake2,
            &mut slot.trampoline,
            index,
        )?,
        HookKind::DetourTrampoline => {
            build_trampoline_detour(
                target,
                handler,
                trampoline,
                entry,
                &mut slot.fake,
                &mut slot.fake2,
                target.trampoline_phys,
            )?;
        }
        HookKind::Hide => {}
    }

    Ok(())
}

fn build_trampoline_detour(
    target: &InstallTarget,
    handler: u64,
    trampoline_va: u64,
    entry: &mut HookEntry,
    fake: &mut Page<[u8; PAGE_SIZE]>,
    fake2: &mut Page<[u8; PAGE_SIZE]>,
    trampoline_phys: u64,
) -> Result<(), HookError> {
    copy_from_gpa(target.page_gpa, fake.as_mut_ptr() as *mut u8, PAGE_SIZE)?;

    if target.page2_gpa != 0 {
        copy_from_gpa(target.page2_gpa, fake2.as_mut_ptr() as *mut u8, PAGE_SIZE)?;
    }

    let secondary = if target.page2_gpa != 0 {
        Some(&mut **fake2)
    } else {
        None
    };

    // The origin lives in the driver's NonPagedPool page: stolen instructions
    // followed by a jump back to `va + stolen`. The driver proxy enters it with
    // `call`, so the original `ret` returns to the proxy.
    let mut origin = [0u8; PAGE_SIZE];
    let stolen = detour::build_origin_span(
        &mut **fake,
        secondary,
        target.offset as usize,
        target.va,
        handler,
        &mut origin,
    )
    .map_err(map_detour_error)?;

    let origin_len = stolen + detour::ABSOLUTE_JUMP_LEN;

    write_gpa(trampoline_phys, &origin[..origin_len])?;
    poke_range(trampoline_phys, origin_len);

    entry.detour_va = target.va;
    entry.handler_va = handler;
    entry.trampoline_va = trampoline_va;
    entry.detour_stolen_len = stolen as u16;

    Ok(())
}

fn build_detour(
    entry: &mut HookEntry,
    fake: &mut Page<[u8; PAGE_SIZE]>,
    fake2: &mut Page<[u8; PAGE_SIZE]>,
    trampoline: &mut Page<[u8; PAGE_SIZE]>,
    slot: usize,
) -> Result<(), HookError> {
    let offset = entry.offsets[0] as usize;
    let target_va = entry.page_va + offset as u64;
    let page_va = trampoline.virt().as_u64();

    if page_va == 0 {
        return Err(HookError::HypercallsDisabled);
    }

    let trampoline_va = page_va + detour::TRAMPOLINE_OFFSET as u64;

    copy_from_gpa(entry.page_gpa, fake.as_mut_ptr() as *mut u8, PAGE_SIZE)?;

    if entry.page_count > 1 {
        copy_from_gpa(entry.page2_gpa, fake2.as_mut_ptr() as *mut u8, PAGE_SIZE)?;
    }

    let secondary = if entry.page_count > 1 {
        Some(&mut **fake2)
    } else {
        None
    };

    let stolen = detour::build_span(
        &mut **fake,
        secondary,
        offset,
        target_va,
        trampoline_va,
        crate::hv::hypercall_key(),
        slot as u64,
        &mut **trampoline,
    )
    .map_err(map_detour_error)?;

    entry.detour_va = target_va;
    entry.detour_stolen_len = stolen as u16;
    entry.trampoline_va = trampoline_va;

    Ok(())
}

fn map_detour_error(error: detour::DetourError) -> HookError {
    match error {
        detour::DetourError::HypercallsDisabled => HookError::HypercallsDisabled,
        detour::DetourError::PatchOutOfRange | detour::DetourError::InvalidTarget => {
            HookError::InvalidAddress
        }
        detour::DetourError::TooLong | detour::DetourError::UnsupportedInstruction => {
            HookError::Unsupported
        }
    }
}
