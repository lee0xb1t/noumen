use super::*;
use super::detour;
use crate::arch::x86_64::hook_events::push_event;

// 只接受 trampoline 上报代码范围内的 VMCALL。
pub fn on_hook_hit(hook_id: u64, argument: u64, cpu_id: u32) -> EmulationResult<()> {
    let rip = guest::Rip::read()?.as_raw();

    let mut table = HOOKS.lock();
    let Some(entry) = table.get_mut(hook_id as usize) else {
        return Ok(());
    };

    if !entry.active || entry.kind != HookKind::DetourInternal {
        return Ok(());
    }

    let trampoline_end = entry.trampoline_va + detour::REPORT_LEN as u64;
    if entry.trampoline_va == 0
        || rip < entry.trampoline_va
        || rip >= trampoline_end
    {
        return Ok(());
    }

    let target = entry.detour_va;

    entry.hits += 1;
    let hits = entry.hits;
    let tsc = unsafe { x86::time::rdtsc() };

    push_event(HookEvent {
        id: hits,
        tsc,
        cpu: cpu_id,
        kind: HookKind::DetourInternal as u32,
        va: target,
        rip,
        rax: 0,
        rcx: argument,
        rdx: 0,
        r8: 0,
        r9: 0,
    });
    let log = should_log(hits);
    let rsp = if log {
        guest::Rsp::read().map(|rsp| rsp.as_raw()).unwrap_or(0)
    } else {
        0
    };

    drop(table);

    if log {
        klog!(
            "EPT hook hit #{hits}: cpu={cpu_id} target={target:#x} rcx={argument:#x} \
             rip={rip:#x} rsp={rsp:#x}"
        );
    }

    Ok(())
}

pub(crate) fn should_log(hits: u64) -> bool {
    const EVERY: u64 = 64;

    hits == 1 || hits % EVERY == 0
}
