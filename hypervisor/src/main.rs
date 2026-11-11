#![no_std]
#![no_main]

use error::HvError;

use crate::arch::x86_64::{memory, serial};

#[cfg(feature = "vmx-ept")]
use crate::arch::x86_64::mtrr;

mod arch;
mod error;
mod hv;
mod kargs;
mod log;
mod panic;

#[unsafe(no_mangle)]
extern "C" fn _start(args_ptr: *const kargs::KernelArgs, hypercall_key: u64) -> u32 {
    let args = unsafe {
        assert!(!args_ptr.is_null());
        &*args_ptr
    };
    hv::GLOBAL_ARGS.init(args);
    hv::set_hypercall_key(hypercall_key);
    hv::set_system_cr3(args.system_cr3 as u64);

    // 布局版本必须先校验：v1 与 v2 的字段语义不同，错位读会得到垃圾。
    // 这里只做纯参数检查，不访问 guest 内存，因此可以在 serial 初始化之前。
    if let Err(e) = args.validate() {
        return e.as_u32();
    }

    serial::serial_reset();
    if !serial::serial_init() {
        return HvError::SerialInitFail.as_u32();
    }

    // 页池必须先初始化：`buffer-log` 在 `log_base == 0` 时从这里分配内部日志环，
    // 之后的 host 页表也来自同一个池。失败留到日志可用之后再报。
    let pool_result = memory::init();

    // RDTSCP probe + ring signature; must precede the first klog!.
    log::init(args.log_base as u64, args.log_size);

    if let Err(e) = pool_result {
        klog!("{}", e);
        return e.as_u32();
    }

    {
        let a = hv::GLOBAL_ARGS.get();
        let (ver, cpus) = (a.version, a.num_cpus);
        let (cr3, kernel, ksize) = (a.system_cr3, a.kernel_base, a.kernel_size);
        let (sym, syms, str_base, strs) = (a.sym_base, a.sym_count, a.str_base, a.str_size);
        let (log, lsize) = (a.log_base, a.log_size);
        let (pfn, pfn_n) = (a.kernel_pfn_base, a.kernel_pfn_count);
        let (pmap, pmap_n) = (a.phys_map_base, a.phys_map_count);

        klog!(
            "KernelArgs v{ver} cpus={cpus} cr3={cr3:#x} kernel={kernel:#x}+{ksize:#x} syms={sym:#x}/{syms} strs={str_base:#x}/{strs} log={log:#x}/{lsize} pfn={pfn:#x}+{pfn_n} phys_map={pmap:#x}+{pmap_n}"
        );
    }

    #[cfg(feature = "vmx-ept")]
    klog!("MTRR: {:?}", mtrr::MtrrData::read());

    #[cfg(not(feature = "vmx-ept"))]
    klog!("memory virtualization: disabled");

    if let Err(_) = hv::check_features() {
        klog!("{}", HvError::UnsupportedCpu);
        return HvError::UnsupportedCpu.as_u32();
    }

    if hv::is_virtualization_enabled() {
        klog!("{}", HvError::VmxIsAlreadyUse);
        return HvError::VmxIsAlreadyUse.as_u32();
    }

    if let Err(e) = hv::init_host_page(hv::GLOBAL_ARGS.get()) {
        klog!("{}", e);
        return e.as_u32();
    }

    klog_verbose!("host cr3={:#x}", hv::get_host_cr3());

    klog!("Initialized successfully!");

    0
}

/// 入口：切到 noumen 自己的初始化栈再干活。
///
/// loader 线程的内核栈只有十几 KiB，`init_body` 的调用链（VMCS 配置 + EPT
/// 建表 + 日志格式化）会把它用到底；`init_stack.S` 负责保存/恢复 loader 的
/// rsp，所以这里对调用方完全透明。
#[unsafe(no_mangle)]
extern "C" fn _init(cpu_id: u8) -> u32 {
    // SAFETY: `run_on_init_stack` 只切栈、调用 `init_body`、再恢复 rsp。
    unsafe { crate::arch::x86_64::init_stack::run_on_init_stack(cpu_id) }
}

#[unsafe(no_mangle)]
extern "C" fn init_body(cpu_id: u8) -> u32 {
    let i = cpu_id as u32;

    klog!("Initializing core #{}", i);

    if let Err(e) = hv::GLOBAL_VCPU.init(i) {
        klog!("Failed to initialize core: {}", e);
        return e.as_u32();
    }

    if let Err(e) = hv::GLOBAL_VCPU.virtualize_core(i) {
        klog!("Failed to virtualize core: {}", e);
        return e.as_u32();
    }

    0
}
