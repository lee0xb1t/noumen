use super::*;
use super::support::*;
use super::super::state;
use crate::arch::x86_64::vmx::instruction::{Invept, Invvpid, InvvpidType};

//
// ============================ 关键：换页 ≠ 换指令 ============================
//
// 切 EPT leaf 只改变“取指/读写走哪一页”，**不会**让 CPU 已经译码过的指令
// 失效。目标进程在装 hook 之前就把这段代码跑热了（典型：死循环调用 test），
// 真机上 CPU 可能继续执行解码缓存里的旧指令，表现有两种：
//
//   * 装好断点却“命中后直接跑过去”——执行的是缓存里那条真实指令；
//   * Continue 之后一直等不到 MTF 退出——执行的是缓存里那个 `0xCC`。
//
// 嵌套虚拟化时上层 hypervisor 对 VM exit/影子 EPT 的处理会顺手把这类残留
// 盖掉，所以现象是“虚拟机次次正常、真机时好时坏”。
//
// 解决（三条一起用，缺一条都会退化）：
//
// 1. **用户断点不再依赖翻译**：真实页上本来就是 `0xCC`（字节补丁），陷入来自
//    guest 自己执行到的 `INT3`；数据视图指向 clean 副本（见
//    `state::HookEntry::byte_mode` 与 `PteState::CopyDataOnly`）。
// 2. **每次 leaf 切换后清 EPT/VPID 缓存**：`INVEPT(single)` + `INVEPT(all)` +
//    `INVVPID(all)`，见 [`invalidate_all_caches`]。
// 3. **每次切换后对真实页和副本页的断点字节做同值写**（SMC poke），触发
//    CPU 的自修改代码检测来作废 uop/解码缓存，见
//    `support::poke_breakpoint_bytes`。
//
// ============================================================================
//

/// 页状态切换后的强制失效：`INVEPT(single + all)` + `INVVPID(all)`。
///
/// 只做 `INVEPT` 时，TLB 里按 **VPID** 标记的条目可能还保留上一个视图的
/// 翻译；字节补丁页要在“clean 副本（数据视图）”和“真实页（执行视图）”之间
/// 来回切，任何一次残留都会让目标取指看到错的字节。所以每次切完都补一条
/// `INVVPID(all)`，把当前逻辑处理器上所有 VPID 的线性→物理翻译一并清掉。
fn invalidate_all_caches(vmx: &VmxVcpu) -> bool {
    let eptp = vmx.ept.eptp().as_raw();

    let ept_ok = Invept::single(eptp) || Invept::all(eptp);

    // 单上下文失败时已经退化成全局；这里再无条件补一次全局，正确性优先。
    Invept::all(eptp);

    let vpid_ok = Invvpid::new(InvvpidType::AllContext, 0, 0).invvpid();

    if !vpid_ok {
        klog!("diag: INVVPID(all) failed (byte-patched page may be cached)");
    }

    ept_ok && vpid_ok
}


/// 诊断：页状态写入次数（前若干次全打，之后抽样），避免刷屏。
/// 每次页状态切换成功后都要调用的统一入口：清缓存（见文件头注释）。
fn after_page_change(vmx: &VmxVcpu) {
    let _ = invalidate_all_caches(vmx);
}


/// RFLAGS 的 IF 位。
const RFLAGS_IF: u64 = 1 << 9;

/// 准备单步：切回真实页 + 打开 MTF，并**暂时屏蔽外部中断**。
///
/// MTF 只保证“执行一条指令后退出”，但如果在 VM entry 之后、这条指令之前先来了
/// 一个外部中断，CPU 会先进中断处理程序：第一条retire的指令变成内核 ISR 的指令，
/// MTF 在那里退出，**原来的用户指令根本没执行**。日志里表现为
/// `MTF 完成 ... guest rip=0xfffff8...`（内核地址），目标随后又回到同一个 INT3，
/// 调试器就得重复点 Continue。
///
/// 因此这里把 guest 的 `RFLAGS.IF` 清掉，单步这一条指令期间不接受外部中断；
/// MTF 退出时再把 IF 恢复成进入前的值（只合并 IF 位，保留指令自己的标志位结果）。
///
/// `single_step == true` 表示这次是调试器的 `SingleStep`：MTF 退出后要在“下一条
/// 指令”处补一个一次性断点，让目标执行完这一条就停下来。
pub(super) fn begin_single_step(
    vmx: &mut VmxVcpu,
    page_gpa: u64,
    single_step: bool,
    hide: bool,
    thread: u64,
) -> EmulationResult<()> {
    let rflags = guest::Rflags::read()
        .map_err(|_| EmulationError::Terminate(Terminate::internal()))?
        .as_raw();
    let rip = guest::Rip::read()
        .map_err(|_| EmulationError::Terminate(Terminate::internal()))?
        .as_raw();

    if rflags & RFLAGS_IF != 0 {
        guest::Rflags::from_raw(rflags & !RFLAGS_IF)
            .write()
            .map_err(|_| EmulationError::Terminate(Terminate::internal()))?;
    }

    set_monitor_trap_flag(true)?;

    vmx.hook_pending = Some(HookPending {
        page_gpa,
        rflags,
        rip,
        single_step,
        thread,
        hide,
    });

    Ok(())
}

/// 把 guest 的 IF 恢复成单步之前的值（其它标志位保持指令执行后的结果）。
pub(super) fn restore_interrupt_flag(saved: u64) -> EmulationResult<()> {
    let current = guest::Rflags::read()
        .map_err(|_| EmulationError::Terminate(Terminate::internal()))?
        .as_raw();
    let desired = (current & !RFLAGS_IF) | (saved & RFLAGS_IF);

    if desired != current {
        guest::Rflags::from_raw(desired)
            .write()
            .map_err(|_| EmulationError::Terminate(Terminate::internal()))?;
    }

    Ok(())
}

/// 恢复真实页并用 MTF 单步一条指令；MTF 退出后 `settle_page` 重新设为 `RealDataOnly`。
///
/// **关键**：单步要求目标重新取指执行真实指令。实测发现只换 EPT leaf + INVEPT
/// 还不够：CPU 有时仍会执行同一条线性地址上旧的 `0xCC`（旧的翻译/解码结果），
/// 于是 `Continue` 永远等不到 MTF 退出。这里先把 fake page 上这个字节也换成
/// 真实指令，再做一次全局 EPT 失效，即使取指仍落在旧页上也能正确执行。
pub(super) fn single_step_original(
    vmx: &mut VmxVcpu,
    entry: &HookEntry,
    fake: &mut Page<[u8; PAGE_SIZE]>,
    page_gpa: u64,
    single_step: bool,
    thread: u64,
) -> EmulationResult<()> {
    let rip = guest::Rip::read()
        .map_err(|_| EmulationError::Terminate(Terminate::internal()))?
        .as_raw();
    let offset = (rip & PAGE_MASK) as u16;

    // 字节补丁模式：把真实页的原字节写回去，MTF 单步执行完这一条指令后
    // `settle_page` 再把 `0xCC` 写回来。全程不动 EPT 翻译。
    if entry.byte_mode {
        let target = entry.page_gpa + entry.patched_offset as u64;

        write_gpa_byte(target, entry.saved_byte)
            .map_err(|_| EmulationError::Terminate(Terminate::internal()))?;

        begin_single_step(vmx, page_gpa, single_step, false, thread)?;

        return Ok(());
    }

    if entry.kind.is_breakpoint() && entry.has_breakpoint(offset) {
        if let Err(error) = patch_fake_byte_from_real(entry, fake, offset) {
            klog!("breakpoint wait: single-step fake-page fix failed: {error:?}");
        }
    }

    write_page_state(vmx, entry, 0, PteState::RealPassthrough)
        .map_err(|_| EmulationError::Terminate(Terminate::internal()))?;

    // 单上下文 INVEPT 之后再补一次 all-contexts：这一步是正确性问题，
    // 过期翻译会让目标一直执行 fake page 上的 0xCC。
    let eptp = vmx.ept.eptp().as_raw();

    if !Invept::all(eptp) {
        klog!("diag: INVEPT(all) failed eptp={eptp:#x}");
    }

    begin_single_step(vmx, page_gpa, single_step, false, thread)?;

    Ok(())
}

pub(super) fn settle_page(
    vmx: &mut VmxVcpu,
    entry: &HookEntry,
    fake: &mut Page<[u8; PAGE_SIZE]>,
    cpu_id: u32,
) -> EmulationResult<()> {
    // 字节补丁模式：单步结束，把 `0xCC` 写回真实页，并把 leaf 切回 clean 副本
    // （数据视图：读到原字节、取指会因 X=0 触发 violation）。
    if entry.active && entry.byte_mode {
        let target = entry.page_gpa + entry.patched_offset as u64;

        write_gpa_byte(target, 0xCC)
            .map_err(|_| EmulationError::Terminate(Terminate::internal()))?;

        write_page_state(vmx, entry, 0, PteState::CopyDataOnly)
            .map_err(|_| EmulationError::Terminate(Terminate::internal()))?;

        return Ok(());
    }

    // 单步期间 fake page 上的 `0xCC` 被换成了真实指令（见
    // `single_step_original`），这里把它放回去，下一次命中才能再次停住。
    if entry.active && entry.kind.is_breakpoint() {
        if let Err(error) = rebuild_fake_page(entry, fake) {
            klog!("breakpoint wait: settle failed to rebuild fake page: {error:?}");
        }
    }

    if !entry.active {
        write_page_state(vmx, entry, 0, PteState::RealPassthrough)
            .map_err(|_| EmulationError::Terminate(Terminate::internal()))?;

        if entry.page_count > 1 {
            write_page_state(vmx, entry, 1, PteState::RealPassthrough)
                .map_err(|_| EmulationError::Terminate(Terminate::internal()))?;
        }

        return Ok(());
    }

    match entry.kind {
        HookKind::Hide => {
            write_page_state(vmx, entry, 0, PteState::HideData)
                .map_err(|_| EmulationError::Terminate(Terminate::internal()))?;
        }
        HookKind::Breakpoint | HookKind::UserBreakpoint => {
            let state = if breakpoint_owner(cpu_id) {
                PteState::RealDataOnly
            } else {
                PteState::RealPassthrough
            };

            write_page_state(vmx, entry, 0, state)
                .map_err(|_| EmulationError::Terminate(Terminate::internal()))?;
        }
        HookKind::DetourInternal | HookKind::DetourTrampoline => {
            write_page_state(vmx, entry, 0, PteState::RealDataOnly)
                .map_err(|_| EmulationError::Terminate(Terminate::internal()))?;

            if entry.page_count > 1 {
                write_page_state(vmx, entry, 1, PteState::CopyExecOnly)
                    .map_err(|_| EmulationError::Terminate(Terminate::internal()))?;
            }
        }
    }

    Ok(())
}

/// 把某个 hooked 页的 EPT leaf 写成指定状态。
///
/// `RealPassthrough` 只改 leaf 本身（保留当前的内存类型位）；
/// `RealDataOnly`/`CopyExecOnly`/`CopyDataOnly`/Hide 系需要先把 2 MiB 大页拆成 4 KiB 再写。
pub(super) fn write_page_state(
    vmx: &mut VmxVcpu,
    entry: &HookEntry,
    page: usize,
    state: PteState,
) -> Result<(), HookError> {
    let (gpa, original_pte, real_data_only_pte, copy_exec_only_pte) = match page {
        0 => (
            entry.page_gpa,
            entry.original_pte,
            entry.real_data_only_pte(),
            entry.copy_exec_only_pte(),
        ),
        1 => (
            entry.page2_gpa,
            entry.original_pte2,
            entry.real_data_only_pte2(),
            entry.copy_exec_only_pte2(),
        ),
        _ => return Err(HookError::InvalidAddress),
    };

    let phys = PhysAddr::new(gpa);

    if state == PteState::RealPassthrough {
        let Some(current) = vmx.ept.pte_raw(phys).map_err(|_| HookError::NoMemory)? else {
            return Ok(());
        };

        let desired = state::keep_memory_type(original_pte, current);
        if desired != current {
            vmx.ept
                .set_pte(phys, desired)
                .map_err(|_| HookError::NoMemory)?;
            after_page_change(vmx);
            poke_breakpoint_bytes(entry);
        }

        return Ok(());
    }

    let desired = match state {
        PteState::RealDataOnly => real_data_only_pte,
        PteState::CopyExecOnly => copy_exec_only_pte,
        PteState::CopyDataOnly => entry.copy_data_only_pte(),
        PteState::HideData => {
            vmx.hide_zero.fill(0);
            entry.hide_data_pte(vmx.hide_zero.pfn())
        }
        PteState::HideExec => entry.hide_exec_pte(),
        PteState::HideShadow => entry.hide_shadow_pte(vmx.hide_shadow.pfn()),
        PteState::RealPassthrough => unreachable!(),
    };

    vmx.ept.split_pde(phys).map_err(|_| HookError::NoMemory)?;

    let current = vmx
        .ept
        .pte_raw(phys)
        .map_err(|_| HookError::NoMemory)?
        .ok_or(HookError::NoMemory)?;

    let desired = state::keep_memory_type(desired, current);

    vmx.ept
        .set_pte(phys, desired)
        .map_err(|_| HookError::NoMemory)?;

    after_page_change(vmx);
    poke_breakpoint_bytes(entry);

    Ok(())
}

