//! `RDTSC` / `RDTSCP` 的模拟与目标判定。
//!
//! 判定按 CPL 分工：
//!
//! - `CPL3`：只按进程（`KPCR → KTHREAD → EPROCESS → PID`，翻译用 `system_cr3`）；
//! - `CPL0`：只按内核模块（`RIP ∈ [base, base+size)`），线程键取 `[KPCR+0x188]`。
//!
//! 每次退出只判定一次（`dispatch_vm_exit`），结果存进 vCPU，两条 `RDTSC` 路径复用。

use crate::{
    arch::x86_64::{
        guest_context::GuestContext,
        resume::EmulationResult,
        vcpu::Vcpu,
        vmx::{state::selective_tsc, vmcs},
    },
    klog,
};

use super::skip_instruction;

const KERNEL_BASE: u64 = 0xffff_8000_0000_0000;

pub(super) fn emulate_rdtsc(vcpu: &mut Vcpu, context: &mut GuestContext) -> EmulationResult<()> {
    let physical = unsafe { x86::time::rdtsc() };
    let tsc = guest_visible_tsc(vcpu, physical);

    context.rax = tsc & 0xffff_ffff;
    context.rdx = (tsc >> 32) & 0xffff_ffff;

    skip_instruction()
}

pub(super) fn emulate_rdtscp(vcpu: &mut Vcpu, context: &mut GuestContext) -> EmulationResult<()> {
    let (physical, aux) = unsafe { x86::time::rdtscp() };
    let tsc = guest_visible_tsc(vcpu, physical);

    context.rax = tsc & 0xffff_ffff;
    context.rdx = (tsc >> 32) & 0xffff_ffff;
    context.rcx = aux as u64;

    skip_instruction()
}

/// 命中目标就返回隐藏后的值，否则原样返回物理 TSC。
fn guest_visible_tsc(vcpu: &mut Vcpu, physical: u64) -> u64 {
    let cpu_id = vcpu.cpu_id;
    let vmx = vcpu.backend.as_vmx_mut();

    let Some(matched) = vmx.selective_tsc.current_match() else {
        return physical;
    };

    if !matched.matched() {
        return physical;
    }

    // 时钟是全局一条曲线，拿不到 KTHREAD 也照常隐藏；`thread` 只是日志标签。
    let thread = matched.thread.unwrap_or(0);
    let (hidden, since, host_time) = vmx.selective_tsc.hide(physical, thread);

    // 命中日志自身要 12000~45000 tick，落在测量窗口里会抬高读数，可用 `0x206` 关掉。
    if !selective_tsc::hit_log() {
        return hidden;
    }

    let (exits, matching) = vmx.selective_tsc.exit_counters();

    log_match(
        cpu_id,
        if matched.by_pid { "pid" } else { "module" },
        &matched,
        physical,
        hidden,
        since,
        host_time,
        exits,
        matching,
        selective_tsc::exit_overhead(),
        vmx.selective_tsc.rewind_count(),
        vmx.selective_tsc.stale_count(),
        vmx.selective_tsc.thread_count(),
        vmx.selective_tsc.last_window,
        vmx.selective_tsc.guest_run,
    );

    hidden
}

/// 判定当前退出上下文是否属于隐藏目标；没有目标返回 `None`。
///
/// `exit_kpcr` 是**本核**这次退出的 `IA32_KERNEL_GS_BASE` 快照（CPL3 时是 KPCR），
/// 必须按 vCPU 传入：多核共用一个全局变量会互相覆盖，PID 直接判错。
pub(super) fn match_target(exit_kpcr: Option<u64>) -> Option<selective_tsc::TargetMatch> {
    let target = selective_tsc::snapshot();

    if !target.has_pid() && !target.has_module() {
        return None;
    }

    let cs = vmcs::guest::CsSelector::read().map_or(0, |cs| cs.as_raw());
    let cpl = guest_cpl().unwrap_or(0);
    let rip = vmcs::guest::Rip::read().map_or(0, |rip| rip.as_raw());

    let mut matched = selective_tsc::TargetMatch {
        by_pid: false,
        by_module: false,
        cpl,
        cs,
        rip,
        pid: None,
        thread: None,
    };

    if cpl == 3 {
        if target.has_pid() && target.allows_cpl(3) {
            // 快照优先，`rdmsr` 兜底：快照没写进来时不能静默失配。
            let kpcr = exit_kpcr
                .filter(|kpcr| *kpcr >= KERNEL_BASE)
                .or_else(|| {
                    let kpcr = unsafe { x86::msr::rdmsr(selective_tsc::IA32_KERNEL_GS_BASE) };

                    (kpcr >= KERNEL_BASE).then_some(kpcr)
                });
            // 内核半区必须用 system_cr3 翻译（KVA shadow 下用户 CR3 没映射）。
            let kernel_cr3 = crate::hv::GLOBAL_ARGS.get().system_cr3 as u64;
            let offsets = crate::hv::kargs_win_offsets();
            let pid =
                kpcr.and_then(|kpcr| selective_tsc::current_process_id(kernel_cr3, kpcr, &offsets));

            log_miss(pid, &target, exit_kpcr, kpcr, kernel_cr3, &offsets);

            matched.pid = pid;

            if pid == Some(target.pid) && target.pid != 0 {
                matched.by_pid = true;
                matched.thread = selective_tsc::current_kthread(3);
            }
        }
    } else if target.has_module() && target.allows_cpl(0) && target.contains_rip(rip) {
        matched.by_module = true;
        matched.thread = selective_tsc::current_kthread(0);
    }

    Some(matched)
}

/// 失配日志：限流 8 条，把链上每一跳都打出来，便于分辨偏移表错还是 CR3 错。
fn log_miss(
    pid: Option<u32>,
    target: &selective_tsc::TargetSnapshot,
    exit_kpcr: Option<u64>,
    kpcr: Option<u64>,
    kernel_cr3: u64,
    offsets: &crate::kargs::WinOffsets,
) {
    use core::sync::atomic::{AtomicU32, Ordering};

    static PID_LOG: AtomicU32 = AtomicU32::new(0);

    if pid == Some(target.pid) || PID_LOG.fetch_add(1, Ordering::Relaxed) >= 8 {
        return;
    }

    let probe = kpcr.map_or_else(selective_tsc::ChainProbe::default, |kpcr| {
        selective_tsc::probe_chain(kernel_cr3, kpcr, offsets)
    });

    // 152 字节一条，别用 `{:#x}`：四个 64 位地址的前缀会把最后的字段截断。
    klog!(
        "TSC: miss pid={:?} tgt={} kpcr={:x} snap={:x} c3={:x} kt={:x} ep={:x}",
        pid,
        target.pid,
        kpcr.unwrap_or(0),
        exit_kpcr.unwrap_or(0),
        kernel_cr3,
        probe.kthread.unwrap_or(0),
        probe.eprocess.unwrap_or(0)
    );
}

/// `CPL == CS.RPL`（64 位模式），CS 读不到退回 SS.RPL，都失败按 CPL0 处理。
fn guest_cpl() -> Option<u8> {
    if let Ok(cs) = vmcs::guest::CsSelector::read() {
        return Some((cs.as_raw() & 3) as u8);
    }

    vmcs::guest::SsSelector::read()
        .ok()
        .map(|ss| (ss.as_raw() & 3) as u8)
}

static MATCH_LOG_COUNT: core::sync::atomic::AtomicU32 = core::sync::atomic::AtomicU32::new(0);

#[allow(clippy::too_many_arguments)]
fn log_match(
    cpu_id: u32,
    reason: &str,
    matched: &selective_tsc::TargetMatch,
    physical: u64,
    hidden: u64,
    since: u64,
    host_time: u64,
    exits: u64,
    matched_exits: u64,
    overhead: u64,
    rewind: u64,
    stale: u64,
    threads: u32,
    root: u64,
    guest: u64,
) {
    let count = MATCH_LOG_COUNT.fetch_add(1, core::sync::atomic::Ordering::Relaxed);

    klog!(
        "TSC: hidden rdtsc since={} root={} guest={} cpl={} cs={:#x} pid={:?} cpu#{} by {} kthread={:#x} phys={:#x} -> {:#x} host_time={} exits={} matched={} overhead={} rewind={} stale={} threads={} rip={:#x} hits={}",
        since,
        root,
        guest,
        matched.cpl,
        matched.cs,
        matched.pid,
        cpu_id,
        reason,
        matched.thread.unwrap_or(0),
        physical,
        hidden,
        host_time,
        exits,
        matched_exits,
        overhead,
        rewind,
        stale,
        threads,
        matched.rip,
        count + 1
    );
}
