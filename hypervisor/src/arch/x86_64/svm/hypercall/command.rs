use crate::arch::x86_64::{
    guest_context::GuestContext, hypercall::Hypercall, vcpu::Vcpu,
};

#[cfg(any(feature = "svm-npt", feature = "buffer-log"))]
use super::diag;
#[cfg(feature = "svm-npt")]
use super::{hook, mem, wait};

pub(super) fn dispatch(vcpu: &mut Vcpu, ctx: &mut GuestContext, command: Hypercall) {
    #[cfg(any(feature = "svm-npt", feature = "buffer-log"))]
    let svm = vcpu.backend.as_svm_mut();
    #[cfg(not(any(feature = "svm-npt", feature = "buffer-log")))]
    let _ = vcpu;

    match command {
        #[cfg(feature = "svm-npt")]
        Hypercall::HookInstall => hook::install(svm, ctx),
        #[cfg(feature = "svm-npt")]
        Hypercall::SvmHookInstall => hook::install_method(svm, ctx),
        #[cfg(feature = "svm-npt")]
        Hypercall::SvmHookRemove | Hypercall::HookRemove => hook::remove(svm, ctx),
        #[cfg(feature = "svm-npt")]
        Hypercall::SvmHookInstallNpt => hook::install_npt(svm, ctx),
        #[cfg(feature = "svm-npt")]
        Hypercall::HookRemovePage => hook::remove_page(svm, ctx),
        #[cfg(feature = "svm-npt")]
        Hypercall::UserBreakpointOneshot => hook::oneshot(ctx),
        #[cfg(feature = "svm-npt")]
        Hypercall::UserBreakpointCommit => hook::commit(ctx),
        #[cfg(feature = "svm-npt")]
        Hypercall::HookApply | Hypercall::HookHit => ctx.rax = 0,
        #[cfg(feature = "svm-npt")]
        Hypercall::HideSelf => diag::hide_self(svm, ctx),
        #[cfg(feature = "svm-npt")]
        Hypercall::ReadSelf => diag::read_self(svm, ctx, false),
        #[cfg(feature = "svm-npt")]
        Hypercall::ReadSelfEpt => diag::read_self(svm, ctx, true),
        #[cfg(feature = "svm-npt")]
        Hypercall::UserBreakpointWaitList
        | Hypercall::UserBreakpointWaitReply
        | Hypercall::UserBreakpointWaitTimeout
        | Hypercall::UserBreakpointWaitSetReg
        | Hypercall::UserBreakpointWaitFilter
        | Hypercall::UserBreakpointWaitCancel => wait::dispatch(svm, ctx, command),
        #[cfg(feature = "svm-npt")]
        Hypercall::MemQuery
        | Hypercall::MemBatch
        | Hypercall::MemMap
        | Hypercall::MemStats
        | Hypercall::MemScan => mem::dispatch(svm, ctx, command),
        #[cfg(feature = "buffer-log")]
        Hypercall::LoggerInfo => diag::logger_info(ctx),
        #[cfg(feature = "buffer-log")]
        Hypercall::FlushLogs => diag::flush_logs(svm, ctx),
        // TSC 虚拟化（0x200..0x206）：**SVM 暂不实现**，编号保留并返回 `3`。
        // SVM 侧没有任何拦截位/offset 被使用，guest 读到的就是物理 TSC。
        Hypercall::TscSetTargetPid
        | Hypercall::TscSetTargetName
        | Hypercall::TscSetTargetModule
        | Hypercall::TscSetWinOffsets
        | Hypercall::TscSetExitOverhead
        | Hypercall::TscSetDivisor
        | Hypercall::TscSetHitLog => ctx.rax = 3,
        _ => ctx.rax = 3,
    }
}
