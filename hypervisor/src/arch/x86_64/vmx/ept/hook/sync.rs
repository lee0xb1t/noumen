use super::*;
use super::page::write_page_state;
use super::super::state;

#[inline]
pub fn sync(cpu_id: u32, vmx: &mut VmxVcpu) {
    let generation = GENERATION.load(Ordering::Acquire);
    let breakpoint_generation = BREAKPOINT_GENERATION.load(Ordering::Acquire);
    let Some(applied) = APPLIED.get(cpu_id as usize) else {
        return;
    };

    let hooks_stale = applied.load(Ordering::Acquire) != generation;
    let breakpoints_stale = vmx.breakpoint_generation != breakpoint_generation;

    if !hooks_stale && !breakpoints_stale {
        return;
    }

    sync_slow(
        cpu_id,
        vmx,
        generation,
        breakpoint_generation,
        hooks_stale,
        breakpoints_stale,
    );
}

#[cold]
fn sync_slow(
    cpu_id: u32,
    vmx: &mut VmxVcpu,
    generation: u32,
    breakpoint_generation: u32,
    hooks_stale: bool,
    breakpoints_stale: bool,
) {
    let mut table = HOOKS.lock();

    if hooks_stale {
        apply_pending_logged(cpu_id, vmx, &mut table, generation);
    }

    if breakpoints_stale {
        apply_breakpoint_owner(cpu_id, vmx, &mut table, breakpoint_generation);
    }
}

fn apply_breakpoint_owner(
    cpu_id: u32,
    vmx: &mut VmxVcpu,
    table: &mut [HookSlot; MAX_HOOKS],
    generation: u32,
) {
    let pending_page = vmx.hook_pending.map(|pending| pending.page_gpa);

    // 只重写 active 断点，detour/hide 不动。
    for slot in table.iter() {
        let entry = &slot.entry;

        if !entry.active || !entry.kind.is_breakpoint() {
            continue;
        }

        if Some(entry.page_gpa) == pending_page {
            continue;
        }

        let state = if breakpoint_owner(cpu_id) {
            PteState::RealDataOnly
        } else {
            PteState::RealPassthrough
        };

        if let Err(error) = write_page_state(vmx, entry, 0, state) {
            klog!(
                "EPT breakpoints: vCPU#{} cannot apply owner generation {} (will retry): {:?}",
                cpu_id,
                generation,
                error
            );
            return;
        }
    }

    vmx.breakpoint_generation = generation;
}

pub(crate) fn bump_generation() -> u32 {
    let generation = GENERATION.fetch_add(1, Ordering::AcqRel) + 1;
    crate::arch::x86_64::vmx::state::bump_exit_sync_generation();
    generation
}

/// 某个 vCPU 应用某一代时，每个槽位/页只打一次失败日志，避免刷屏。
static LAST_LOGGED: [AtomicU32; MAX_CPUS] = [const { AtomicU32::new(0) }; MAX_CPUS];

/// 记录一次 `apply_pending` 的页状态写入失败（每个 vCPU 每代只打一次）。
fn note_apply_failure(
    cpu_id: u32,
    generation: u32,
    index: usize,
    page: usize,
    entry: &HookEntry,
    state: PteState,
    error: HookError,
) {
    let Some(logged) = LAST_LOGGED.get(cpu_id as usize) else {
        return;
    };

    if logged.swap(generation, Ordering::AcqRel) == generation {
        return;
    }

    klog!(
        "EPT: vCPU#{cpu_id} gen={generation} page write failed slot={index} page={page} \
         kind={:?} gpa={:#x} state={state:?} err={error:?} (page skipped)",
        entry.kind,
        if page == 0 { entry.page_gpa } else { entry.page2_gpa }
    );
}

fn apply_pending(
    cpu_id: u32,
    vmx: &mut VmxVcpu,
    table: &mut [HookSlot; MAX_HOOKS],
    generation: u32,
) -> Result<(), HookError> {
    let Some(applied) = APPLIED.get(cpu_id as usize) else {
        return Err(HookError::InvalidAddress);
    };

    let applied_value = applied.load(Ordering::Acquire);

    if applied_value >= generation {
        return Ok(());
    }

    let pending_page = vmx.hook_pending.map(|pending| pending.page_gpa);

    // 先 restore retired，再 arm active；同一页可能同时有旧/新记录。
    //
    // 单个页写失败不能让整个 vCPU 永远卡在“应用不了这一代”上（那会让每个
    // VM exit 都重试并刷日志）。这里逐页容错：失败的页跳过并记录一次，其余
    // 页照常应用，最后仍然把 `APPLIED` 推进到这一代。
    for (index, slot) in table.iter().enumerate() {
        let entry = &slot.entry;

        if state::sync_action(entry, applied_value, pending_page) == SyncAction::Restore {
            if let Err(error) = write_page_state(vmx, entry, 0, PteState::RealPassthrough) {
                note_apply_failure(cpu_id, generation, index, 0, entry, PteState::RealPassthrough, error);
            }

            if entry.page_count > 1 {
                if let Err(error) = write_page_state(vmx, entry, 1, PteState::RealPassthrough) {
                    note_apply_failure(cpu_id, generation, index, 1, entry, PteState::RealPassthrough, error);
                }
            }
        }
    }

    for (index, slot) in table.iter().enumerate() {
        let entry = &slot.entry;

        if state::sync_action(entry, applied_value, pending_page) != SyncAction::Arm {
            continue;
        }

        let (first_state, second_state) = if entry.kind == HookKind::Hide {
            (PteState::HideData, PteState::HideData)
        } else if entry.byte_mode {
            // 字节补丁模式：每个核把自己的 leaf 指向 clean 副本（数据视图）。
            (PteState::CopyDataOnly, PteState::RealPassthrough)
        } else if entry.kind.is_breakpoint() && !breakpoint_owner(cpu_id) {
            (PteState::RealPassthrough, PteState::RealPassthrough)
        } else {
            (PteState::RealDataOnly, PteState::CopyExecOnly)
        };

        if let Err(error) = write_page_state(vmx, entry, 0, first_state) {
            note_apply_failure(cpu_id, generation, index, 0, entry, first_state, error);
        }

        if entry.page_count > 1 {
            if let Err(error) = write_page_state(vmx, entry, 1, second_state) {
                note_apply_failure(cpu_id, generation, index, 1, entry, second_state, error);
            }
        }
    }

    applied.store(generation, Ordering::Release);

    Ok(())
}

pub(crate) fn apply_pending_logged(
    cpu_id: u32,
    vmx: &mut VmxVcpu,
    table: &mut [HookSlot; MAX_HOOKS],
    generation: u32,
) {
    if let Err(error) = apply_pending(cpu_id, vmx, table, generation) {
        klog!(
            "EPT hooks: vCPU#{} cannot apply generation {} (will retry): {:?}",
            cpu_id,
            generation,
            error
        );
    }
}
