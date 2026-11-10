//! Loader → hypervisor 的**内存层**启动描述符。
//!
//! v3 起彻底移除 OS 字段：操作系统语义（进程、模块、偏移、PE）完全由调用者
//! （调用方）负责，hypervisor 只消费内存相关字段。`version` 仍在偏移 `0x00`：
//! 读任何其它字段之前必须先校验它。
//!
//! 尺寸 112 字节；与驱动 `NOUMEN_KERNEL_ARGS` 逐字段对应。

use spin::Once;

use crate::error::{HvError, HvResult};

/// `KernelArgs` ABI 版本。字段增删必须 +1，并同步驱动侧 `NOUMEN_KARGS_VERSION`。
pub const KARGS_VERSION: u32 = 4;

/// 物理内存区间上限（`MmGetPhysicalMemoryRanges` 在常见机器上 10–40 项）。
pub const MAX_PHYS_RANGES: u32 = 256;

/// Windows 内核结构偏移；`build == 0` 表示未提供。选择性 TSC 移除后保留为 ABI 占位。
#[repr(C, packed(4))]
#[derive(Debug, Clone, Copy)]
pub struct WinOffsets {
    pub build: u32,
    pub list_blink: u32,
    pub eproc_link: u32,
    pub kproc_dtb: u32,
    pub eproc_pid: u32,
    pub eproc_name: u32,
    pub eproc_peb: u32,
    pub eproc_section_base: u32,
    pub eproc_exit_status: u32,
    pub eproc_thread_list: u32,
    pub eproc_wow64: u32,
    pub eproc_vad_root: u32,
    /// `_KTHREAD.ApcState.Process` 的偏移；当前进程 = `gs_base + 本字段`。
    pub kthread_process: u32,
    pub reserved5: u32,
}

#[repr(C, packed(8))]
#[derive(Debug, Clone, Copy)]
pub struct KernelArgs {
    /// 必须等于 [`KARGS_VERSION`]。
    pub version: u32,
    pub num_cpus: u8,
    pub reserved0: [u8; 3],
    /// `phys_map_base` 指向的区间个数（0 = 未提供）。
    pub phys_map_count: u32,
    pub reserved1: u32,
    /// nonpaged `PHYSICAL_MEMORY_RANGE[]` 的内核 VA，由驱动保活。
    /// 数组以全 0 项结尾；hypervisor 只用它实现 `MEM_MAP`。
    pub phys_map_base: usize,
    /// 内核半区可用的 CR3。
    pub system_cr3: usize,
    pub system_cr3_virt: usize,
    /// **noumen 镜像**基址（不是 ntoskrnl）。
    pub kernel_base: usize,
    pub sym_base: usize,
    pub str_base: usize,
    pub log_base: usize,
    pub kernel_size: u32,
    pub sym_size: u32,
    pub str_size: u32,
    pub log_size: u32,
    pub sym_count: u16,
    pub reserved2: u16,
    pub reserved3: u32,
    pub kernel_pfn_base: usize,
    pub kernel_pfn_count: u32,
    pub reserved4: u32,
    /// Windows 内核结构偏移表（用于进程检查）。`build == 0` 表示未提供。
    pub win_offsets: WinOffsets,
}

const _: () = assert!(core::mem::size_of::<WinOffsets>() == 56);
const _: () = assert!(core::mem::size_of::<KernelArgs>() == 168);
const _: () = assert!(core::mem::offset_of!(KernelArgs, phys_map_base) == 0x10);
const _: () = assert!(core::mem::offset_of!(KernelArgs, system_cr3) == 0x18);
const _: () = assert!(core::mem::offset_of!(KernelArgs, kernel_base) == 0x28);
const _: () = assert!(core::mem::offset_of!(KernelArgs, kernel_pfn_base) == 0x60);

impl KernelArgs {
    /// 参数级校验：**不访问 guest 内存**，可以在 serial/log 初始化之前调用。
    pub fn validate(&self) -> HvResult<()> {
        // 通过局部变量读 packed 字段，避免对未对齐字段取引用。
        if self.version != KARGS_VERSION {
            return Err(HvError::UnsupportedKargs);
        }

        let cr3 = self.system_cr3 as u64;

        if cr3 == 0 || cr3 & 0xFFF != 0 {
            return Err(HvError::InvalidPrameters);
        }

        let phys_count = self.phys_map_count;
        let phys_base = self.phys_map_base as u64;

        if phys_count > MAX_PHYS_RANGES {
            return Err(HvError::BadPhysMap);
        }

        if phys_count != 0 && phys_base == 0 {
            return Err(HvError::BadPhysMap);
        }

        if self.num_cpus == 0 {
            return Err(HvError::InvalidPrameters);
        }

        Ok(())
    }
}

#[derive(Debug)]
pub struct GlobalArgs {
    inner: Once<KernelArgs>,
}

impl GlobalArgs {
    pub const fn new() -> Self {
        Self { inner: Once::new() }
    }

    pub fn init(&self, args: &KernelArgs) {
        self.inner.call_once(|| *args);
    }

    pub fn get(&self) -> &KernelArgs {
        self.inner.get().expect("GlobalArgs not initialized")
    }
}
