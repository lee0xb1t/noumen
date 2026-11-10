use core::sync::atomic::{AtomicU64, Ordering::SeqCst};

use crate::{
    arch::x86_64::{addr::VirtAddr, paging, sync, svm, vcpu, vendor::Vendor, vmx},
    error::{HvError, HvResult},
    kargs,
};

pub static GLOBAL_VCPU: vcpu::GlobalVcpu = vcpu::GlobalVcpu::new();

pub static GLOBAL_HOST_PAGE: sync::Mutex<paging::HostPageTable> =
    sync::Mutex::new(paging::HostPageTable::new());
pub static GLOBAL_HOST_CR3: AtomicU64 = AtomicU64::new(0);

pub static GLOBAL_ARGS: kargs::GlobalArgs = kargs::GlobalArgs::new();

/// 是否使用 1 GiB EPT 页，以及恒等映射覆盖的字节数。
#[cfg(feature = "vmx-ept")]
pub static EPT_1GB_PAGES: core::sync::atomic::AtomicBool =
    core::sync::atomic::AtomicBool::new(false);
#[cfg(feature = "vmx-ept")]
pub static EPT_COVERAGE: AtomicU64 = AtomicU64::new(0);

#[cfg(feature = "vmx-ept")]
#[inline]
pub fn ept_coverage() -> u64 {
    EPT_COVERAGE.load(SeqCst)
}

#[cfg(feature = "vmx-ept")]
#[inline]
pub fn set_ept_coverage(bytes: u64) {
    EPT_COVERAGE.store(bytes, SeqCst);
}

#[cfg(feature = "vmx-ept")]
#[inline]
pub fn ept_1gb_pages() -> bool {
    EPT_1GB_PAGES.load(SeqCst)
}

#[cfg(feature = "vmx-ept")]
#[inline]
pub fn set_ept_1gb_pages(use_1gb: bool) {
    EPT_1GB_PAGES.store(use_1gb, SeqCst);
}

/// Loader-supplied VMCALL key; `0` disables hypercalls.
pub static HYPERCALL_KEY: AtomicU64 = AtomicU64::new(0);

pub fn set_hypercall_key(key: u64) {
    HYPERCALL_KEY.store(key, SeqCst);
}

pub fn hypercall_key() -> u64 {
    HYPERCALL_KEY.load(SeqCst)
}

pub fn check_features() -> HvResult<()> {
    match Vendor::detect() {
        Vendor::Intel => vmx::check_vmx_features(),
        Vendor::Amd => svm::check_svm_features(),
        Vendor::Other => Err(HvError::UnsupportedCpu),
    }
}

#[inline]
pub fn is_virtualization_enabled() -> bool {
    match Vendor::detect() {
        Vendor::Intel => vmx::is_enabled(),
        Vendor::Amd => svm::is_enabled(),
        Vendor::Other => false,
    }
}

pub fn init_host_page(kargs: &kargs::KernelArgs) -> HvResult<()> {
    let mut page_table = GLOBAL_HOST_PAGE.lock();

    page_table.init()?;
    page_table.map_identity();
    page_table.map_kernel(VirtAddr::new(kargs.system_cr3_virt as u64));

    GLOBAL_HOST_CR3.store(page_table.get_cr3(), SeqCst);

    Ok(())
}

pub fn get_host_cr3() -> u64 {
    GLOBAL_HOST_CR3.load(SeqCst)
}
