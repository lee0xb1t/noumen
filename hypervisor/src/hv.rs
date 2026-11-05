use core::sync::atomic::{AtomicBool, AtomicU32, AtomicU64, Ordering::SeqCst};

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

/// 运行时下发的偏移表覆盖（hypercall `0x203`）。
///
/// `KernelArgs` 在 `_start` 时只读一次；有了这份覆盖，loader 可以在**虚拟化之后**
/// 换偏移表（换目标/换了 build 的系统时不用重启）。VM-exit 路径读它，所以必须是
/// 无锁的原子量：一个 `ACTIVE` 标志 + 逐字段的 `AtomicU32`。
static WIN_OFFSETS_OVERRIDE: [AtomicU32; 14] = [
    AtomicU32::new(0),
    AtomicU32::new(0),
    AtomicU32::new(0),
    AtomicU32::new(0),
    AtomicU32::new(0),
    AtomicU32::new(0),
    AtomicU32::new(0),
    AtomicU32::new(0),
    AtomicU32::new(0),
    AtomicU32::new(0),
    AtomicU32::new(0),
    AtomicU32::new(0),
    AtomicU32::new(0),
    AtomicU32::new(0),
];
static WIN_OFFSETS_OVERRIDE_ACTIVE: AtomicBool = AtomicBool::new(false);

/// 下发一份新的偏移表并启用覆盖；`build == 0` 表示清除覆盖。
pub fn set_win_offsets_override(offsets: &kargs::WinOffsets) {
    if offsets.build == 0 {
        WIN_OFFSETS_OVERRIDE_ACTIVE.store(false, SeqCst);
        return;
    }

    // 先写字段再置标志：读者看到 ACTIVE 时字段一定已经就绪。
    for (slot, value) in WIN_OFFSETS_OVERRIDE.iter().zip(fields_of(offsets)) {
        slot.store(value, SeqCst);
    }

    WIN_OFFSETS_OVERRIDE_ACTIVE.store(true, SeqCst);
}

pub fn win_offsets_override_active() -> bool {
    WIN_OFFSETS_OVERRIDE_ACTIVE.load(SeqCst)
}

fn fields_of(offsets: &kargs::WinOffsets) -> [u32; 14] {
    [
        offsets.build,
        offsets.list_blink,
        offsets.eproc_link,
        offsets.kproc_dtb,
        offsets.eproc_pid,
        offsets.eproc_name,
        offsets.eproc_peb,
        offsets.eproc_section_base,
        offsets.eproc_exit_status,
        offsets.eproc_thread_list,
        offsets.eproc_wow64,
        offsets.eproc_vad_root,
        offsets.kthread_process,
        offsets.reserved5,
    ]
}

/// 当前生效的 Windows 内核结构偏移表。
///
/// 运行时覆盖优先，否则取 `KernelArgs.win_offsets`。`KernelArgs` 那份在日志
/// 初始化之前就固定了，之后只读；VM-exit 里必须无锁，所以每次取一份值拷贝。
#[inline]
pub fn kargs_win_offsets() -> kargs::WinOffsets {
    if win_offsets_override_active() {
        let override_ = kargs::WinOffsets {
            build: WIN_OFFSETS_OVERRIDE[0].load(SeqCst),
            list_blink: WIN_OFFSETS_OVERRIDE[1].load(SeqCst),
            eproc_link: WIN_OFFSETS_OVERRIDE[2].load(SeqCst),
            kproc_dtb: WIN_OFFSETS_OVERRIDE[3].load(SeqCst),
            eproc_pid: WIN_OFFSETS_OVERRIDE[4].load(SeqCst),
            eproc_name: WIN_OFFSETS_OVERRIDE[5].load(SeqCst),
            eproc_peb: WIN_OFFSETS_OVERRIDE[6].load(SeqCst),
            eproc_section_base: WIN_OFFSETS_OVERRIDE[7].load(SeqCst),
            eproc_exit_status: WIN_OFFSETS_OVERRIDE[8].load(SeqCst),
            eproc_thread_list: WIN_OFFSETS_OVERRIDE[9].load(SeqCst),
            eproc_wow64: WIN_OFFSETS_OVERRIDE[10].load(SeqCst),
            eproc_vad_root: WIN_OFFSETS_OVERRIDE[11].load(SeqCst),
            kthread_process: WIN_OFFSETS_OVERRIDE[12].load(SeqCst),
            reserved5: WIN_OFFSETS_OVERRIDE[13].load(SeqCst),
        };

        return override_;
    }

    let packed = GLOBAL_ARGS.get().win_offsets;

    kargs::WinOffsets {
        build: packed.build,
        list_blink: packed.list_blink,
        eproc_link: packed.eproc_link,
        kproc_dtb: packed.kproc_dtb,
        eproc_pid: packed.eproc_pid,
        eproc_name: packed.eproc_name,
        eproc_peb: packed.eproc_peb,
        eproc_section_base: packed.eproc_section_base,
        eproc_exit_status: packed.eproc_exit_status,
        eproc_thread_list: packed.eproc_thread_list,
        eproc_wow64: packed.eproc_wow64,
        eproc_vad_root: packed.eproc_vad_root,
        kthread_process: packed.kthread_process,
        reserved5: packed.reserved5,
    }
}

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
