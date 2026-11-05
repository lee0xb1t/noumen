//! EPT hook removal and page-level retirement.

use crate::{
    arch::x86_64::{
        guest_memory,
        vmx::{VmxVcpu, vmcs::guest},
    },
    klog,
};

use super::super::{
    HOOKS, HookSlot, WAITS,
    state::{self, HookError, MAX_HOOKS, PAGE_MASK},
    support::{rebuild_fake_page, write_gpa_byte},
};
use super::sync::{apply_pending_logged, bump_generation};

pub fn remove(vmx: &mut VmxVcpu, cpu_id: u32, va: u64) -> Result<(), HookError> {
    if !guest_memory::is_canonical(va) {
        return Err(HookError::InvalidAddress);
    }

    let cr3 = guest::Cr3::read()
        .map_err(|_| HookError::NotMapped)?
        .as_raw();

    let mut table = HOOKS.lock();

    let index = state::find_by_va(&table[..], va, cr3).ok_or(HookError::NotFound)?;

    let page_gpa = table[index].page_gpa;

    if table[index].kind.is_breakpoint() {
        let offset = (va & PAGE_MASK) as u16;

        if !table[index].remove_breakpoint(offset) {
            return Err(HookError::NotFound);
        }

        // Byte-patch mode: restore the original byte before switching the leaf
        // back to RealPassthrough. The reverse order leaves a short window where
        // the real page still contains 0xCC.
        if table[index].byte_mode {
            if !table[index].pending_patch {
                let target = page_gpa + table[index].patched_offset as u64;

                match write_gpa_byte(target, table[index].saved_byte) {
                    Ok(()) => klog!(
                        "user breakpoint: restored real page byte gpa={target:#x} (original {:#x})",
                        table[index].saved_byte
                    ),
                    Err(error) => {
                        klog!("user breakpoint: restore real page byte failed gpa={target:#x}: {error:?}")
                    }
                }
            }

            table[index].byte_mode = false;
            table[index].pending_patch = false;
        }

        // An in-flight wait would leave the target thread parked on INT3; also
        // clear the thread filter so a new breakpoint at this VA starts clean.
        {
            let mut waits = WAITS.lock();

            waits.cancel_va(va);
            waits.clear_filter_va(va);
        }

        if table[index].offset_count == 0 {
            let generation = bump_generation();
            WAITS.lock().cancel_slot(index);
            state::retire(&mut table[index].entry, generation);
            apply_pending_logged(cpu_id, vmx, &mut table, generation);
        } else {
            let slot = &mut table[index];
            if let Err(error) = rebuild_fake_page(&mut slot.entry, &mut slot.fake) {
                slot.add_breakpoint(offset);
                return Err(error);
            }

            let generation = bump_generation();
            slot.generation = generation;
            apply_pending_logged(cpu_id, vmx, &mut table, generation);
        }
    } else {
        let generation = bump_generation();
        state::retire(&mut table[index].entry, generation);
        apply_pending_logged(cpu_id, vmx, &mut table, generation);
    }

    klog!(
        "EPT hook removed: va={:#x} gpa={:#x} breakpoints left={}",
        va,
        page_gpa,
        table[index].offset_count
    );

    Ok(())
}

/// Remove one breakpoint offset; retire the slot when it was the last one.
///
/// Callers must hold the `HOOKS` lock.
pub(crate) fn remove_breakpoint_offset(
    vmx: &mut VmxVcpu,
    cpu_id: u32,
    table: &mut [HookSlot; MAX_HOOKS],
    index: usize,
    offset: u16,
) {
    // Byte-patch mode restores the real page byte regardless of how many
    // offsets remain.
    if table[index].byte_mode {
        if !table[index].pending_patch {
            let target = table[index].page_gpa + table[index].patched_offset as u64;

            let _ = write_gpa_byte(target, table[index].saved_byte);
        }

        table[index].byte_mode = false;
        table[index].pending_patch = false;
    }

    if table[index].offset_count > 1 {
        let slot = &mut table[index];

        slot.remove_breakpoint(offset);

        // The clean copy in byte-patch mode contains the original byte, so it
        // must not be rebuilt from the fake page.
        if slot.byte_mode {
            klog!(
                "breakpoint: removed offset 0x{offset:#x}; {} breakpoints left on page (byte patch)",
                slot.offset_count
            );

            return;
        }

        match rebuild_fake_page(&slot.entry, &mut slot.fake) {
            Ok(()) => klog!(
                "breakpoint: removed offset 0x{offset:#x}; {} breakpoints left on page",
                slot.offset_count
            ),
            Err(error) => klog!("breakpoint: rebuild fake page failed: {error:?}"),
        }

        return;
    }

    let generation = bump_generation();

    WAITS.lock().cancel_slot(index);
    state::retire(&mut table[index].entry, generation);
    apply_pending_logged(cpu_id, vmx, table, generation);

    klog!("breakpoint: slot {index} has no breakpoints; retired (page restored)");
}

pub fn remove_page(vmx: &mut VmxVcpu, cpu_id: u32, page_gpa: u64) -> Result<(), HookError> {
    let page_gpa = page_gpa & !PAGE_MASK;

    let mut table = HOOKS.lock();
    let index = state::find_page(&table[..], page_gpa).ok_or(HookError::NotFound)?;

    WAITS.lock().cancel_slot(index);

    if table[index].byte_mode {
        if !table[index].pending_patch {
            let target = table[index].page_gpa + table[index].patched_offset as u64;

            let _ = write_gpa_byte(target, table[index].saved_byte);
        }

        table[index].byte_mode = false;
        table[index].pending_patch = false;
    }

    let generation = bump_generation();
    state::retire(&mut table[index].entry, generation);
    apply_pending_logged(cpu_id, vmx, &mut table, generation);

    klog!("EPT hook removed by page: gpa={:#x}", page_gpa);

    Ok(())
}
