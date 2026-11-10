//! VMX hypercall dispatch.

use crate::arch::x86_64::{
    guest_context::GuestContext,
    hypercall::Hypercall,
    resume::{EmulationError, EmulationResult},
    vcpu::Vcpu,
};

mod diag;
#[cfg(feature = "vmx-ept")]
mod hook;
#[cfg(all(feature = "mem", feature = "vmx-ept"))]
mod mem;
#[cfg(feature = "vmx-ept")]
mod wait;

#[allow(deprecated, unreachable_patterns)]
pub fn dispatch(
    context: &mut GuestContext,
    key: u64,
    vcpu: &mut Vcpu,
) -> EmulationResult<()> {
    #[cfg(not(feature = "vmx-ept"))]
    let _ = vcpu;

    if key == 0 || context.rax != key {
        return Err(EmulationError::ud());
    }

    let Some(command) = Hypercall::from_raw(context.rcx) else {
        return Err(EmulationError::ud());
    };

    match command {
        Hypercall::Ping => diag::ping(context),
        Hypercall::Stop => {
            // 退出虚拟化前把字节补丁恢复，别把 `0xCC` 留在 guest 的代码页里。
            #[cfg(feature = "vmx-ept")]
            {
                let restored = crate::arch::x86_64::vmx::ept::hook::restore_patched_bytes();

                if restored != 0 {
                    crate::klog!("stop: restored {restored} user-breakpoint byte patches");
                }
            }

            Err(EmulationError::Stop)
        }
        #[cfg(feature = "buffer-log")]
        Hypercall::FlushLogs => diag::flush_logs(context),
        #[cfg(feature = "buffer-log")]
        Hypercall::LoggerInfo => diag::logger_info(context),
        #[cfg(feature = "vmx-ept")]
        Hypercall::HookInstall => hook::install(context, vcpu),
        #[cfg(feature = "vmx-ept")]
        Hypercall::HookRemove => hook::remove(context, vcpu),
        #[cfg(feature = "vmx-ept")]
        Hypercall::HookRemovePage => hook::remove_page(context, vcpu),
        #[cfg(feature = "vmx-ept")]
        Hypercall::HideSelf => diag::hide_self(context, vcpu),
        #[cfg(feature = "vmx-ept")]
        Hypercall::ReadSelf => diag::read_self(context, vcpu, false),
        #[cfg(feature = "vmx-ept")]
        Hypercall::ReadSelfEpt => diag::read_self(context, vcpu, true),
        #[cfg(feature = "vmx-ept")]
        Hypercall::UserBreakpointWaitList => wait::list(context, vcpu),
        #[cfg(feature = "vmx-ept")]
        Hypercall::UserBreakpointWaitReply => wait::reply(context, vcpu),
        #[cfg(feature = "vmx-ept")]
        Hypercall::UserBreakpointWaitTimeout => wait::timeout(context, vcpu),
        #[cfg(feature = "vmx-ept")]
        Hypercall::UserBreakpointWaitSetReg => wait::set_reg(context, vcpu),
        #[cfg(feature = "vmx-ept")]
        Hypercall::UserBreakpointWaitFilter => wait::filter(context, vcpu),
        #[cfg(feature = "vmx-ept")]
        Hypercall::UserBreakpointWaitCancel => wait::cancel(context, vcpu),
        #[cfg(feature = "vmx-ept")]
        Hypercall::UserBreakpointOneshot => hook::oneshot(context, vcpu),
        #[cfg(feature = "vmx-ept")]
        Hypercall::HookApply => hook::apply(context, vcpu),
        #[cfg(feature = "vmx-ept")]
        Hypercall::HookRetired => hook::retired(context),
        #[cfg(feature = "vmx-ept")]
        Hypercall::UserBreakpointCommit => hook::commit(context, vcpu),
        #[cfg(feature = "vmx-ept")]
        Hypercall::HookHit => hook::hit(context, vcpu),
        // 内存服务：只在 VM-exit 上下文可用，入口自己再检查一次。
        #[cfg(all(feature = "mem", feature = "vmx-ept"))]
        Hypercall::MemQuery => mem::query(context),
        #[cfg(all(feature = "mem", feature = "vmx-ept"))]
        Hypercall::MemBatch => mem::batch(context, vcpu),
        #[cfg(all(feature = "mem", feature = "vmx-ept"))]
        Hypercall::MemScan => mem::scan(context, vcpu),
        #[cfg(all(feature = "mem", feature = "vmx-ept"))]
        Hypercall::MemMap => mem::map(context),
        #[cfg(all(feature = "mem", feature = "vmx-ept"))]
        Hypercall::MemStats => mem::stats(context),
        // 选择性 TSC 已移除：编号保留，返回“不支持”，避免旧 loader/uapp 的
        // TSC 面板调用时收到 #UD。
        Hypercall::TscSetTargetPid
        | Hypercall::TscSetTargetName
        | Hypercall::TscSetTargetModule
        | Hypercall::TscSetWinOffsets
        | Hypercall::TscSetExitOverhead
        | Hypercall::TscSetDivisor
        | Hypercall::TscSetHitLog => {
            context.rax = 3;
            Ok(())
        }
        // 本构建未实现，保留编号。通过 RAX 报告“不支持”，避免内核调用方
        // 收到 #UD。
        #[cfg(not(feature = "vmx-ept"))]
        Hypercall::HookInstall
        | Hypercall::HookRemove
        | Hypercall::HookRemovePage
        | Hypercall::HideSelf
        | Hypercall::ReadSelf
        | Hypercall::ReadSelfEpt
        | Hypercall::UserBreakpointWaitList
        | Hypercall::UserBreakpointWaitReply
        | Hypercall::UserBreakpointWaitTimeout
        | Hypercall::UserBreakpointWaitSetReg
        | Hypercall::UserBreakpointWaitFilter
        | Hypercall::UserBreakpointWaitCancel
        | Hypercall::UserBreakpointOneshot
        | Hypercall::HookApply
        | Hypercall::UserBreakpointCommit
        | Hypercall::HookRetired
        | Hypercall::MemQuery
        | Hypercall::MemBatch
        | Hypercall::MemScan
        | Hypercall::MemMap
        | Hypercall::MemStats => {
            context.rax = 3;
            Ok(())
        }
        // 有 EPT 但没编 `mem`：编号保留，报告不支持而不是 #UD。
        #[cfg(all(feature = "vmx-ept", not(feature = "mem")))]
        Hypercall::MemQuery
        | Hypercall::MemBatch
        | Hypercall::MemScan
        | Hypercall::MemMap
        | Hypercall::MemStats => {
            context.rax = 3;
            Ok(())
        }
        // SVM-only 编号（NPT 函数 hook、SVM 等待表、DR 计数）。VMX 后端没有这些
        // 能力；返回“不支持”而不是 #UD —— 内核调用方收到 #UD 是 bugcheck，不是
        // 一个能显示出来的错误。
        Hypercall::SvmHookInstall
        | Hypercall::SvmHookInstallNpt
        | Hypercall::SvmHookRemove
        | Hypercall::SvmTfStep
        | Hypercall::SvmUserBreakpointWaitList
        | Hypercall::SvmUserBreakpointWaitReply
        | Hypercall::SvmUserBreakpointWaitTimeout
        | Hypercall::SvmUserBreakpointWaitSetReg
        | Hypercall::SvmUserBreakpointWaitFilter
        | Hypercall::SvmUserBreakpointWaitCancel
        | Hypercall::SvmHwDrStatus => {
            context.rax = 3;
            Ok(())
        }
        // 例如关了 `buffer-log` 时的 1/2：保留编号，但这里没有实现。
        _ => Err(EmulationError::ud()),
    }
}
