//! 用户断点（CPL3 INT3 自旋等待）实现存档。
//!
//! 与 EPT 硬件断点分开存放：`wait` 是等待项状态机（ABI、超时、ACK、取消），
//! 本模块放用户断点特有的动作应用（写回寄存器、单步临时断点、一次性断点退役）。
//! EPT 页状态机仍在 `super::breakpoint`。
//!
//! `wait` 刻意不引用 crate 内其它模块，因此可以在宿主机上单独编译测试。

pub mod wait;

use core::sync::atomic::Ordering;

use crate::{
    arch::x86_64::{
        guest_context::{GeneralPurposeRegister, GuestContext},
        resume::{EmulationError, EmulationResult, Terminate},
        vmx::{VmxVcpu, vmcs::guest},
    },
    klog,
};

use self::wait::{HookWait, WAIT_ACTION_SINGLE_STEP};
use super::{
    HookSlot, MAX_HOOKS, PAGE_MASK,
    breakpoint::{APPLY_AWAITS_MTF, APPLY_SEQ},
    install::remove_breakpoint_offset,
    page::single_step_original,
    support::rebuild_fake_page,
};

/// 把应答/超时的动作落到目标线程所在的 vCPU 上。等待项已由调用方清除。
///
/// 应用之前先把等待项里缓存的上下文写回目标线程：调试器可以用
/// `HYPERCALL_USER_BREAKPOINT_WAIT_SET_REG` 改寄存器，`Continue` 时一次性生效。
///
/// 临时单步 hook（跨页单步装的）在成功执行这一条指令后**整个删除**：
/// 它只服务“把目标停在这一行”这一次，之后不再参与命中。
#[allow(clippy::too_many_arguments)]
pub(super) fn apply_wait_action(
    vmx: &mut VmxVcpu,
    context: &mut GuestContext,
    table: &mut [HookSlot; MAX_HOOKS],
    index: usize,
    action: u32,
    cpu_id: u32,
    va: u64,
    thread: u64,
    wait: Option<&HookWait>,
    acked: bool,
) -> EmulationResult<()> {
    let slot = &mut table[index];
    let entry = &mut slot.entry;
    let fake = &mut slot.fake;
    let rip = guest::Rip::read()?.as_raw();
    let seq = wait.map(|wait| wait.seq).unwrap_or(0);
    let single_step = action == WAIT_ACTION_SINGLE_STEP;

    // 应用动作是控制路径（一次点击/一次超时一条），不节流：排查“点了 Continue
    // 没生效”时，它和 `断点等待: ACK` 一起决定动作有没有落到目标 vCPU 上。
    klog!(
        "breakpoint wait: apply seq={seq} {} slot={index} cpu={cpu_id} thread={thread:#x} \
         va={va:#x} rip={rip:#x} ({})",
        if single_step { "SingleStep" } else { "Continue" },
        if acked { "acked" } else { "timeout" }
    );

    if let Some(wait) = wait {
        write_wait_regs(context, wait)?;
    }

    // 一次性断点只停一次：如果这次就是停在单步补出来的断点上，先把它摘掉并
    // 重建 fake page，之后 Continue / 再单步都按普通断点处理。
    let offset = (va & PAGE_MASK) as u16;
    if entry.is_temp_breakpoint(offset) {
        entry.take_temp_breakpoint();

        if let Err(error) = rebuild_fake_page(entry, fake) {
            klog!("breakpoint wait: one-shot fake-page rebuild failed: {error:?}");
        } else {
            klog!("breakpoint wait: single-step one-shot 0x{va:#x} removed");
        }
    }

    // 先决定要不要“回收/禁用”这个条目（之后 `entry` 就不能再借了）：
    // * 一次性断点：第一次命中被继续（或超时）后**禁用**——页恢复原状，之后
    //   执行到这里不再停；`SingleStep` 不禁用（调试器还在交互）；
    // * 临时单步 hook：目标已经停在它上面，调试器一继续，整个删除；
    // * 临时断点摘掉后本页已没有任何断点：退役。
    let step_hook_retire = entry.step_hook;
    let one_shot_disable = entry.one_shot && entry.enabled && !single_step;
    let empty_retire = entry.active && entry.offset_count == 0;

    single_step_original(
        vmx,
        entry,
        fake,
        entry.page_gpa,
        single_step,
        wait.map(|wait| wait.thread).unwrap_or(0),
    )?;

    if step_hook_retire || empty_retire {
        remove_breakpoint_offset(vmx, cpu_id, table, index, offset);
    } else if one_shot_disable {
        klog!("breakpoint one-shot: va={va:#x} first hit continued; disabled (will not stop here again)");

        table[index].entry.enabled = false;
        remove_breakpoint_offset(vmx, cpu_id, table, index, offset);
    }

    // 记下“等 MTF”：下一条指令执行完才会有 MTF 退出；如果没有等到就又命中，
    // 说明原指令没执行（被打断/页状态没切回）。
    APPLY_SEQ.store(seq, Ordering::Release);
    APPLY_AWAITS_MTF.store(true, Ordering::Release);

    klog!(
        "breakpoint wait: {} switched back to real page and enabled MTF; waiting for one instruction",
        if single_step { "SingleStep" } else { "Continue" }
    );

    Ok(())
}

/// 在同一条目的 fake page 上装/复用一个一次性断点（单步“停在下一条指令”用）。
///
/// `thread` 是这个一次性断点的 owner：只有它命中才停，别的线程只单步放行。
/// 返回是否装好（偏移已满、页重建失败时返回 false）。
pub(super) fn arm_temp_breakpoint(
    slot: &mut HookSlot,
    offset: u16,
    va: u64,
    thread: u64,
) -> bool {
    // 先摘掉上一次单步留下的一次性偏移（如果有）。
    slot.entry.take_temp_breakpoint();

    if slot.entry.has_breakpoint(offset) {
        klog!("breakpoint wait: single-step: next instruction {va:#x} already has a breakpoint; stop directly");

        return true;
    }

    if !slot.entry.set_temp_breakpoint(offset) {
        klog!("breakpoint wait: single-step: {va:#x} cannot install (page offsets full or duplicate)");

        return false;
    }

    slot.entry.temp_thread = thread;

    match rebuild_fake_page(&mut slot.entry, &mut slot.fake) {
        Ok(()) => {
            klog!("breakpoint wait: single-step: next instruction {va:#x} one-shot installed (thread=0x{thread:X})");

            true
        }
        Err(error) => {
            slot.entry.take_temp_breakpoint();
            klog!("breakpoint wait: single-step fake-page rebuild failed: {error:?}");

            false
        }
    }
}

/// 把等待项缓存的 16 个通用寄存器写回 guest。
///
/// `rsp` 通过 `write_gpr` 写进 `GUEST_RSP` VMCS 字段，其余直接落到
/// `GuestContext`；没改过的寄存器写回的是命中时的原值，等于空操作。
pub(super) fn write_wait_regs(
    context: &mut GuestContext,
    wait: &HookWait,
) -> EmulationResult<()> {
    for index in 0..=15u8 {
        let Some(value) = wait.gpr(index) else {
            return Err(EmulationError::Terminate(Terminate::internal()));
        };

        context.write_gpr(GeneralPurposeRegister::from_raw(index), value)?;
    }

    Ok(())
}
