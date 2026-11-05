use super::*;
use super::super::state;
use super::install::remove_breakpoint_offset;
use super::page::{restore_interrupt_flag, settle_page, single_step_original, write_page_state};
use super::sync::{apply_pending_logged, bump_generation};
use super::support::*;
use super::user_breakpoint::wait;
use super::user_breakpoint::{apply_wait_action, arm_temp_breakpoint};
use crate::arch::x86_64::hook_events::push_event;
use core::sync::atomic::{AtomicBool, AtomicU32, AtomicU64, Ordering};
/// 上一次 `Continue` 是否已经等到“原指令执行完”的 MTF 退出。
///
/// `Continue` 的实现是“切回真实页 + 打开 MTF 单步一条指令”。这条指令执行完会
/// 产生一次 MTF 退出（`handle_monitor_trap`）；如果它先被 NMI/异常打断，
/// `abort_pending` 会撤销单步，MTF 退出不会到来，目标立刻又回到同一个 INT3。
/// 下次命中时这个标记还是 set 的，就说明上一次 Continue 实际没执行原指令——
/// 点 Continue 却像没生效，看这一行。
pub(super) static APPLY_AWAITS_MTF: AtomicBool = AtomicBool::new(false);
pub(super) static APPLY_SEQ: AtomicU64 = AtomicU64::new(0);

/// 诊断：`#BP` 没有被当作我们断点处理的原因（每种只提示一次）。
static MISS_SEEN: AtomicU32 = AtomicU32::new(0);

fn note_breakpoint_miss(reason: u32, rip: u64, cr3: u64, user: bool, page_gpa: u64) {
    const REASONS: [&str; 4] = [
        "translate RIP failed (page not mapped)",
        "no hook at this GPA",
        "hit page kind is not breakpoint",
        "real page is already 0xCC (guest breakpoint)",
    ];
    let bit = 1 << reason;

    if MISS_SEEN.fetch_or(bit, Ordering::AcqRel) & bit != 0 {
        return;
    }

    klog!(
        "diag: #BP unhandled rip={rip:#x} cr3={cr3:#x} user={user} gpa={page_gpa:#x} reason={}",
        REASONS.get(reason as usize).copied().unwrap_or("?")
    );
}

//
// 用户断点（CPL3 + 低半区）在等待模式打开时走自旋路径：第一次命中只建等待项、
// 上报一次事件，不恢复原页、不打开 MTF、不动 RIP，guest 回去再执行 INT3；之后
// 每次 #BP 只返回 guest，直到 ACK 或超时。INT3 是 trap：VMCS RIP 仍指向 INT3。
pub fn handle_breakpoint(
    vmx: &mut VmxVcpu,
    context: &mut GuestContext,
    cpu_id: u32,
) -> EmulationResult<bool> {
    if vmx.hook_pending.is_some() {
        klog!(
            "diag: #BP while a single-step is pending (page={:#x}); hit ignored",
            vmx.hook_pending.map(|pending| pending.page_gpa).unwrap_or(0)
        );

        return Ok(false);
    }

    let rip = guest::Rip::read()?.as_raw();
    let cr3 = guest::Cr3::read()?.as_raw();
    let user = guest_is_user().map_err(|_| EmulationError::Terminate(Terminate::internal()))?;

    let phys = match guest_memory::translate_guest(cr3, rip, user, false, false) {
        Ok(phys) => phys,
        Err(_) => {
            note_breakpoint_miss(0, rip, cr3, user, 0);
            return Ok(false);
        }
    };

    let page_gpa = phys & !PAGE_MASK;
    let offset = (phys & PAGE_MASK) as u16;

    let mut table = HOOKS.lock();
    let Some(index) = state::find_page_or_retired(&table[..], page_gpa) else {
        note_breakpoint_miss(1, rip, cr3, user, page_gpa);
        return Ok(false);
    };

    let entry = &mut table[index];

    if !entry.kind.is_breakpoint() {
        note_breakpoint_miss(2, rip, cr3, user, page_gpa);
        return Ok(false);
    }

    // 真实页本身就是 0xCC，说明是 guest 自己的断点；字节补丁模式下这个
    // `0xCC` 是我们自己写进去的，必须照常处理。
    let patched_by_us = entry.byte_mode && entry.has_breakpoint(offset);

    match read_gpa_byte(page_gpa + offset as u64) {
        Ok(0xCC) if !patched_by_us => {
            note_breakpoint_miss(3, rip, cr3, user, page_gpa);
            return Ok(false);
        }
        Err(_) => {
            note_breakpoint_miss(3, rip, cr3, user, page_gpa);
            return Ok(false);
        }
        Ok(_) => {}
    }

    // 真机上排查“命中没停”用：页此刻指向谁、真实页字节是什么。

    // 真实页不是 0xCC，却收到了 #BP：只可能是 fake page 里的隐藏断点。如果这个
    // offset 已经不在表里（断点被移除，目标线程还停在 INT3 上），就取消等待项并
    // 单步执行原指令，不要把伪造的 #BP 注入 guest。
    if !entry.has_breakpoint(offset) {
        let removed_va = entry.page_va + offset as u64;

        if WAITS.lock().cancel_va(removed_va) != 0 {
            klog!("breakpoint wait: cancel va={removed_va:#x} slot={index} (breakpoint removed)");
        }

        single_step_original(vmx, &entry.entry, &mut entry.fake, page_gpa, false, 0)?;

        return Ok(true);
    }

    let va = entry.page_va + offset as u64;
    let rearm = entry.active && breakpoint_owner(cpu_id);

    // 记录“这个断点第一次在哪触发”：只记一次，后续命中不再改（`hits` 照常递增）。
    if entry.kind.is_breakpoint() && entry.first_hit.is_none() {
        let tsc = unsafe { x86::time::rdtsc() };

        entry.first_hit = Some(state::FirstHit {
            va,
            rip,
            thread: 0,
            cpu: cpu_id,
            tsc,
        });

        klog!("EPT breakpoint first hit: slot={index} va={va:#x} rip={rip:#x} cpu={cpu_id}");
    }

    if !rearm {
        // 非 owner 核的页在 sync 时已经恢复成真实页，正常不会再命中；能走到这里
        // 说明页状态还没跟上（例如 owner 刚变化）。用户断点上记一条，否则这条
        // 路径完全没有日志。
        if entry.kind.is_user_breakpoint() {
            log_wait_note(entry, WaitNote::NotOwner, cpu_id, va, rip);
        }

        single_step_original(vmx, &entry.entry, &mut entry.fake, page_gpa, false, 0)?;

        return Ok(true);
    }

    // 只有 USER_BREAKPOINT 会等待；内核态命中的用户页回退到普通 MTF 单步。
    if entry.kind.is_user_breakpoint() && user && is_user_address(rip) {
        let rsp = guest::Rsp::read().map(|rsp| rsp.as_raw()).unwrap_or(0);
        let now = unsafe { x86::time::rdtsc() };

        // 等待项**只和 ETHREAD 绑定**：不看 vCPU、也不看 RSP。每次命中都读一次
        // 当前线程；线程被迁移到别的核仍然能对上，别的线程在同一核/同一栈上
        // 执行也不会误匹配。
        let thread = current_guest_thread().unwrap_or(0);

        // 用户断点才有线程身份：补进首次命中记录（普通断点保持 0）。
        if thread != 0 {
            if let Some(first) = entry.first_hit.as_mut() {
                if first.thread == 0 {
                    first.thread = thread;
                }
            }
        }

        let held = WAITS.lock().lookup(index).copied();

        // 条目里有待处理的命中（Pending）或已应答待应用（Applying）；Idle 表示
        // 上一次命中已经走完，这次是**新命中**，原地更新同一个条目。
        let busy = held.is_some_and(|wait| wait.state != wait::WAIT_STATE_IDLE);

        if busy {
            let wait = held.expect("busy implies a wait entry");

            // 其它线程命中同一地址（或同页的另一个断点）：按普通路径单步放行，
            // 既不进自旋，也不动等待项。
            if !wait.owned_by(va, thread) {
                // 拿不到 ETHREAD（`thread == 0`）且命中的就是等待项那个地址时，
                // 不要单步放行：那会把 owner 悄悄放走、让等待项永远等不到应用。
                // 按 owner 处理，继续自旋（日志只提示一次）。
                if thread == 0 && wait.va == va {
                    log_wait_note(entry, WaitNote::NoThread, cpu_id, va, rip);

                    return Ok(true);
                }

                if wait.va == va {
                    if thread == 0 {
                        log_wait_note(entry, WaitNote::NoThread, cpu_id, va, rip);
                    } else {
                        log_wait_note(entry, WaitNote::OtherThread, cpu_id, va, rip);
                    }
                }

                single_step_original(vmx, &entry.entry, &mut entry.fake, page_gpa, false, 0)?;

                return Ok(true);
            }

            // 取出动作和上下文快照。`complete` 只把条目退回 Idle（**不删除**），
            // 同一个 seq 会留给下一次命中。
            let due = {
                let mut waits = WAITS.lock();
                let acked =
                    waits.lookup(index).is_some_and(|wait| wait.state == wait::WAIT_STATE_APPLYING);
                let action = waits.take_due(index, now);

                action.map(|action| (action, acked, waits.complete(index)))
            };

            // 没有动作就继续停在 INT3：不恢复原页、不打开 MTF、不计数、
            // 不写日志。
            if let Some((action, acked, snapshot)) = due {
                apply_wait_action(
                    vmx,
                    context,
                    &mut *table,
                    index,
                    action,
                    cpu_id,
                    va,
                    thread,
                    snapshot.as_ref(),
                    acked,
                )?;
            }

            return Ok(true);
        }

        {
            // `SingleStep` 留下的一次性断点只对“被单步的线程”生效：
            // * `temp_thread == 0`：等待项已经被取消/owner 变化，这个 0xCC 已经没有
            //   主人，直接摘掉并单步放行（否则它会变成永久断点，目标也停不下来）；
            // * 别的线程命中：单步放行它，但保留断点给 owner（不能把 owner 的停止点
            //   抢走，也不能删掉它）。
            if entry.is_temp_breakpoint(offset) {
                let owner = entry.temp_thread;

                // 跨页单步的临时 hook 没有经过驱动锁页（hypervisor 自己装不上
                // `MmProbeAndLockPages`）。如果这个 GPA 已经被别的进程/映射复用，
                // 它的 CR3 就和装 hook 时不同：**不能停**，摘掉这个临时断点并放行。
                if entry.cr3 != 0 && cr3 != entry.cr3 {
                    klog!(
                        "breakpoint wait: single-step temporary breakpoint 0x{va:#x} page reused by another CR3 ({cr3:#x}); \
                         remove and single-step (temporary hook holds no page lock)"
                    );

                    single_step_original(vmx, &entry.entry, &mut entry.fake, page_gpa, false, 0)?;
                    remove_breakpoint_offset(vmx, cpu_id, &mut *table, index, offset);

                    return Ok(true);
                }

                if owner == 0 {
                    klog!(
                        "breakpoint wait: single-step one-shot 0x{va:#x} expired; remove and single-step"
                    );

                    single_step_original(vmx, &entry.entry, &mut entry.fake, page_gpa, false, 0)?;
                    // 这个偏移可能只属于单步（跨页单步装的临时 hook 只有一个偏移）：
                    // 交给退役逻辑处理，避免留下一个“0 偏移但仍 active”的页。
                    remove_breakpoint_offset(vmx, cpu_id, &mut *table, index, offset);

                    return Ok(true);
                }

                if thread != 0 && thread != owner {
                    log_wait_note(entry, WaitNote::TempOtherThread, cpu_id, va, rip);

                    single_step_original(vmx, &entry.entry, &mut entry.fake, page_gpa, false, 0)?;

                    return Ok(true);
                }
            }

            let timeout = wait::timeout_ticks(
                wait_timeout_ms(),
                crate::arch::x86_64::tsc::tsc_hz(),
            );
            let request = WaitContext {
                thread,
                cr3,
                va,
                rip,
                rsp,
                rax: context.rax,
                rcx: context.rcx,
                rdx: context.rdx,
                rbx: context.rbx,
                rbp: context.rbp,
                rsi: context.rsi,
                rdi: context.rdi,
                r8: context.r8,
                r9: context.r9,
                r10: context.r10,
                r11: context.r11,
                r12: context.r12,
                r13: context.r13,
                r14: context.r14,
                r15: context.r15,
            };

            // 记录命中。它要求命中线程的 ETHREAD 已知、并且通过该地址的线程
            // 过滤；任何一条不满足都退回普通单步。
            let begun = WAITS.lock().hit(index, cpu_id, request, now, timeout);

            let seq = match begun {
                Ok(seq) => seq,
                Err(wait::WaitError::NoThread) => {
                    log_wait_note(entry, WaitNote::NoThread, cpu_id, va, rip);

                    single_step_original(vmx, &entry.entry, &mut entry.fake, page_gpa, false, 0)?;

                    return Ok(true);
                }
                Err(wait::WaitError::Filtered) => {
                    log_wait_note(entry, WaitNote::Filtered, cpu_id, va, rip);

                    single_step_original(vmx, &entry.entry, &mut entry.fake, page_gpa, false, 0)?;

                    return Ok(true);
                }
                Err(_) => {
                    single_step_original(vmx, &entry.entry, &mut entry.fake, page_gpa, false, 0)?;

                    return Ok(true);
                }
            };

            // 上一次 Continue 的单步如果被 NMI/异常打断，这里会看到它没有
            // 等到 MTF 退出——说明那次点击没有执行原指令，目标只是又回到了
            // 同一个 INT3。
            if APPLY_AWAITS_MTF.swap(false, Ordering::AcqRel) {
                klog!(
                    "breakpoint wait: warning: Continue seq={} saw no MTF exit (instruction not executed); \
                     new hit seq={seq}",
                    APPLY_SEQ.load(Ordering::Acquire)
                );
            }

            entry.hits += 1;
            let hits = entry.hits;
            let temp_breakpoint = entry.is_temp_breakpoint(offset);

            push_event(HookEvent {
                id: hits,
                tsc: now,
                cpu: cpu_id,
                kind: entry.kind as u32,
                va,
                rip,
                rax: context.rax,
                rcx: context.rcx,
                rdx: context.rdx,
                r8: context.r8,
                r9: context.r9,
            });

            // 关键：确保下一次取指一定再次陷入 INT3。
            //
            // 这次 #BP 有时来自“过期的翻译/解码结果”，此刻页可能仍指向真实页
            // （`leaf=orig`）。如果只是返回 guest，它会直接执行真实指令跑过去，
            // 等待项就成了孤儿——表现就是“断点命中后目标线程没停”。所以每次
            // 建等待项时都把页重新 arm 成 CopyExecOnly，并保证 fake page 上有 0xCC。
            //
            // 字节补丁模式不需要：真实页上已经是 `0xCC`，目标原地重执行就会
            // 再次陷入。
            if !entry.byte_mode {
                if let Err(error) = rebuild_fake_page(&entry.entry, &mut entry.fake) {
                    klog!("breakpoint wait: rebuild fake page while re-arming failed: {error:?}");
                } else if let Err(error) = write_page_state(vmx, &entry.entry, 0, PteState::CopyExecOnly) {
                    klog!("breakpoint wait: re-arm page failed: {error:?}");
                }
            }

            drop(table);

            // 建等待项/更新命中是控制路径（一次点击/一次调用一条），不节流。
            klog!(
                "EPT breakpoint wait #{hits}: seq={seq} slot={index} cpu={cpu_id} \
                 thread={thread:#x} va={va:#x} rip={rip:#x} rsp={rsp:#x} timeout={}ms{}",
                wait_timeout_ms(),
                if temp_breakpoint { " (single-step one-shot)" } else { "" }
            );

            // IMPORTANT: 这里**不**恢复原页、不打开 MTF，也不改 RIP。
            return Ok(true);
        }
    } else if entry.kind.is_user_breakpoint() {
        // 用户断点这次没进等待：CPL0 执行用户页（例如 KVA shadow 之外的路径）。
        log_wait_note(entry, WaitNote::Cpl0, cpu_id, va, rip);
    }

    entry.hits += 1;
    let hits = entry.hits;
    let tsc = unsafe { x86::time::rdtsc() };

    push_event(HookEvent {
        id: hits,
        tsc,
        cpu: cpu_id,
        kind: entry.kind as u32,
        va,
        rip,
        rax: context.rax,
        rcx: context.rcx,
        rdx: context.rdx,
        r8: context.r8,
        r9: context.r9,
    });

    // 命中日志只在这个断点第一次触发时打：自旋/热函数下这里每秒会走到百万次。
    let log = hits == 1;
    let rsp = if log {
        guest::Rsp::read().map(|rsp| rsp.as_raw()).unwrap_or(0)
    } else {
        0
    };

    single_step_original(vmx, &entry.entry, &mut entry.fake, page_gpa, false, 0)?;

    drop(table);

    if log {
        klog!(
            "EPT breakpoint hit #{hits}: cpu={cpu_id} va={va:#x} rip={rip:#x} rsp={rsp:#x} \
             rax={:#x} rcx={:#x} rdx={:#x} r8={:#x} r9={:#x}",
            context.rax,
            context.rcx,
            context.rdx,
            context.r8,
            context.r9
        );
    }

    Ok(true)
}

/// 低半区（bit 47 = 0）才是用户地址；内核 VA 一律不走等待。
fn is_user_address(va: u64) -> bool {
    (va >> 47) & 1 == 0
}

// 异常/NMI 打断 MTF 时必须撤销 pending，否则该 CPU 的断点会永久失效。
pub fn abort_pending(vmx: &mut VmxVcpu, cpu_id: u32) -> EmulationResult<()> {
    let Some(pending) = vmx.hook_pending else {
        return Ok(());
    };

    set_monitor_trap_flag(false)?;

    let mut table = HOOKS.lock();

    if let Some(index) = state::find_page_or_retired(&table[..], pending.page_gpa) {
        let slot = &mut table[index];

        settle_page(vmx, &slot.entry, &mut slot.fake, cpu_id)?;
    }

    drop(table);

    vmx.hook_pending = None;
    restore_interrupt_flag(pending.rflags)?;

    Ok(())
}

pub fn handle_monitor_trap(vmx: &mut VmxVcpu, cpu_id: u32) -> EmulationResult<()> {
    let Some(pending) = vmx.hook_pending.take() else {
        return Err(EmulationError::Terminate(Terminate::internal()));
    };

    set_monitor_trap_flag(false)?;

    let mut table = HOOKS.lock();
    let mut user_breakpoint = false;
    // 跨页单步：下一条指令（分支/跳转目标）在另一个 4 KiB 页，装不了同页
    // 一次性偏移，退出后要给它临时装一个 hook（见函数末尾）。
    let mut cross_page = None;

    if let Some(index) = state::find_page_or_retired(&table[..], pending.page_gpa) {
        user_breakpoint = table[index].entry.kind.is_user_breakpoint();

        // `SingleStep`：MTF 已经在“下一条指令”处退出，先给它装一个一次性断点，
        // 再 settle 回 RealDataOnly；目标下一次取指就会停在这条指令上。
        //
        // “页”必须是 **GPA 页**（hook 表按物理页索引）：用当前 guest CR3 把
        // `next_rip` 翻译一次，同页判断和偏移都基于翻译结果，VA 别名也正确。
        if pending.single_step && table[index].entry.kind.is_breakpoint() {
            let next_rip = guest::Rip::read().map(|rip| rip.as_raw()).unwrap_or(0);
            // 用 pending 里记录的被单步线程，而不是去查等待项：等待项可能已经
            // 因为“临时 hook 退役”被取消了，查出来会是 0。
            let thread = pending.thread;
            let translated = guest::Cr3::read().ok().and_then(|cr3| {
                let user = guest_is_user().unwrap_or(false);

                guest_memory::translate_guest(cr3.as_raw(), next_rip, user, false, false).ok()
            });

            match translated {
                Some(next_gpa) => {
                    let next_page = next_gpa & !PAGE_MASK;
                    let next_offset = (next_gpa & PAGE_MASK) as u16;

                    if next_page == table[index].entry.page_gpa {
                        arm_temp_breakpoint(&mut table[index], next_offset, next_rip, thread);
                    } else if let Some(other) = state::find_page(&table[..], next_page) {
                        // 目标页已经有 hook：在那个条目上加一次性偏移就够了。
                        arm_temp_breakpoint(&mut table[other], next_offset, next_rip, thread);
                    } else {
                        // 目标页没有 hook：退出时用 `install` 临时装一个（只能本核装）。
                        cross_page = Some((next_rip, thread));

                        klog!(
                            "breakpoint wait: single-step: next instruction {next_rip:#x} (gpa={next_gpa:#x}) is on \
                             another page; install cross-page temporary breakpoint after exit"
                        );
                    }
                }
                None => klog!(
                    "breakpoint wait: single-step: translate next instruction {next_rip:#x} failed; falling back to continue"
                ),
            }
        }

        // 从“跨页临时 hook”上再单步时，`arm_temp_breakpoint` 会先摘掉旧偏移；
        // 如果这页没有别的断点了，必须退役，否则它每次取指都会陷入却没人处理。
        if table[index].entry.active && table[index].entry.offset_count == 0 {
            let generation = bump_generation();

            WAITS.lock().cancel_slot(index);
            state::retire(&mut table[index].entry, generation);
            apply_pending_logged(cpu_id, vmx, &mut table, generation);

            klog!("breakpoint wait: single-step temporary hook slot {index} has no breakpoints; retired");
        }

        let slot = &mut table[index];

        settle_page(vmx, &slot.entry, &mut slot.fake, cpu_id)?;
    }

    drop(table);

    // 跨页单步：给下一条指令临时装一个用户断点，命中即停；调试器一继续，
    // 这个临时 hook 整个删除（见 `apply_wait_action` 的 `step_hook_retire`）。
    if let Some((next_rip, thread)) = cross_page {
        match install(vmx, cpu_id, next_rip, HookKind::UserBreakpoint, 0, 0) {
            Ok(()) => {
                let cr3 = guest::Cr3::read().map(|cr3| cr3.as_raw()).unwrap_or(0);
                let mut table = HOOKS.lock();

                if let Some(index) = state::find_by_va(&table[..], next_rip, cr3) {
                    let slot = &mut table[index];

                    slot.entry.temp_offset = (next_rip & PAGE_MASK) as u16;
                    slot.entry.temp_thread = thread;
                    slot.entry.step_hook = true;
                }

                klog!(
                    "breakpoint wait: single-step: installed cross-page one-shot on next instruction {next_rip:#x} \
                     (thread=0x{thread:X})"
                );
            }
            Err(error) => klog!(
                "breakpoint wait: single-step: cross-page temporary breakpoint failed {error:?}; falling back to continue (target will not stop at \
                 {next_rip:#x})"
            ),
        }
    }

    restore_interrupt_flag(pending.rflags)?;

    let rip_now = guest::Rip::read().map(|rip| rip.as_raw()).unwrap_or(0);

    // 单步的原指令真的执行完了：这是 `Continue` 生效的正面证据。顺便打出执行后
    // 的 guest RIP（已经前进），可以直接和断点地址对照。
    if user_breakpoint && APPLY_AWAITS_MTF.swap(false, Ordering::AcqRel) {
        klog!(
            "breakpoint wait: MTF complete seq={}; original instruction executed (guest rip={rip_now:#x}); page restored to RealDataOnly (real page)",
            APPLY_SEQ.load(Ordering::Acquire)
        );

        // 只有“可证明被劫持”的情况才报警：
        // * RIP 落到内核（bit 47 = 1）：用户态的这条指令不可能跳到内核，说明是
        //   中断/异常的处理程序抢了单步；
        // * RIP 仍等于断点地址：这条指令根本没 retire。
        //
        // 不能用 `rip + 指令长度` 当期望值：`test` 在 Debug/增量链接下常常是
        // `jmp [rip+disp]` 跳转桩，执行完 RIP 会变成真实函数体的地址
        // （例如 0x…110e → 0x…22d0），这是**正确**执行，不该报警。
        let hijacked = (rip_now >> 47) & 1 == 1 || rip_now == pending.rip;

        if hijacked {
            klog!(
                "breakpoint wait: warning: single-step did not execute instruction at breakpoint (breakpoint rip={:#x}, actual={rip_now:#x}, \
                 {}); this Continue was intercepted by interrupt/exception; continue again",
                pending.rip,
                if rip_now == pending.rip {
                    "RIP did not advance"
                } else {
                    "landed in kernel address"
                }
            );
        }
    }

    Ok(())
}
