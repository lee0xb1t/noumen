#[cfg(feature = "svm-npt")]
use crate::arch::x86_64::{
    svm::{
        exit_info::{ExitInfo, SvmExitCode},
        hide_self, hook,
        npt::NptView,
    },
    vcpu::Vcpu,
};

/// Bring this vCPU's NPT views up to date before handling the exit.
#[cfg(feature = "svm-npt")]
pub(super) fn sync_views(vcpu: &mut Vcpu, exit: &ExitInfo) {
    // Lazily apply HideSelf to this vCPU (another core may have hidden the
    // image since our last exit).
    {
        let svm = vcpu.backend.as_svm_mut();
        hide_self::sync(svm);
    }

    // Apply hook views installed/removed by other vCPUs.
    {
        let svm = vcpu.backend.as_svm_mut();
        hook::sync(svm);
    }

    // 上一次取指把 hook 页切到了执行视图；数据与取指共用一个 leaf，所以
    // 恢复前这一页对数据读也可见补丁。每次 VM exit 开头切回 ShadowNx。
    {
        let svm = vcpu.backend.as_svm_mut();

        if let Some((page, data_pfn)) = svm.npt_shadow_view.take()
            && hook::is_active_npt_page(page)
        {
            let _ = svm.set_npt_view(page, NptView::ShadowNx, data_pfn);
        }
    }

    // 单步停在临时断点上时保持数据视图干净；取到临时字节的那次 fetch 会
    // 经 #NPF 切到 OriginalExec。
    if exit.code.0 != SvmExitCode::EXCEPTION_BASE + 3 {
        if let Some(page) = hook::stop_after_page() {
            if let Some(clean_pfn) = hook::page_clean_pfn(page) {
                let svm = vcpu.backend.as_svm_mut();
                let _ = svm.set_npt_view(page, NptView::ShadowNx, clean_pfn);
            }
        }
    }

    // Hide hook 执行过的那一页只在这一次执行窗口内指向真实页；下一次 exit
    // 把零页数据视图放回去。
    {
        let svm = vcpu.backend.as_svm_mut();
        if let Some(page) = svm.hide_exec.take() {
            if let Some(zero_pfn) = hook::hide_zero_pfn(page) {
                let _ = svm.set_npt_view(page, NptView::ShadowNx, zero_pfn);
            }
        }
    }

    // Any non-#DB exit during a single step invalidates it: the instruction we
    // wanted to execute may not retire, so re-arm the breakpoint and drop TF.
    {
        let svm = vcpu.backend.as_svm_mut();
        if hook::pending(svm) && exit.code.0 != SvmExitCode::EXCEPTION_BASE + 1 {
            hook::abort_pending(svm);
        }
    }
}

#[cfg(not(feature = "svm-npt"))]
use crate::arch::x86_64::{svm::exit_info::ExitInfo, vcpu::Vcpu};

#[cfg(not(feature = "svm-npt"))]
pub(super) fn sync_views(_: &mut Vcpu, _: &ExitInfo) {}
