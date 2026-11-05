//! 客户机内存访问（hypervisor 侧）。
//!
//! 只提供**通用原语**：虚拟/物理内存的读、写、翻译、分段扫描、批量环、物理区间。
//! 不做任何操作系统解析——进程、模块、偏移、PE 全在 调用方。
//!
//! 放在 `ept` 下的理由：这里拥有 guest 物理地址空间的语义（identity EPT、
//! `DIRECT_MAP_SIZE` 上限、页池），并且 `hook` 也在同一子树，写路径要避让被 hook 的页。
//!
//! 两条硬约束（调用方必须满足）：
//!
//! * 只能在 **VM-exit 上下文**调用（host 页表的直接映射生效）；`_start` 阶段只能
//!   做参数校验，见 [`available`]。
//! * 输入校验失败返回 [`MemError`]，绝不对 guest 注入异常。

pub mod error;
pub mod pattern;
pub mod phys;
pub mod ring;
pub mod scan;
pub mod virt;

pub use error::MemError;
#[allow(unused_imports)]
pub use pattern::MemPattern;
#[allow(unused_imports)]
pub use phys::PhysRange;
#[allow(unused_imports)]
pub use ring::{
    MemOp, MemRingHeader, DATA_OFFSET, MAX_BATCH_BYTES, MAX_BATCH_OPS, MAX_OP_BYTES,
    MAX_RING_BYTES, MIN_RING_BYTES, OP_READ_PHYS, OP_READ_VIRT, OP_TRANSLATE, OP_WRITE_PHYS,
    OP_WRITE_VIRT, RING_ABI, RING_MAGIC,
};
#[allow(unused_imports)]
pub use scan::{
    scan, MemScanRequest, SCAN_DEFAULT_BYTES, SCAN_DEFAULT_US, SCAN_FLAG_MATCH_ALIGNED,
    SCAN_FLAG_SKIP_INACCESSIBLE, SCAN_FLAG_STOP_AFTER_FIRST, SCAN_MAX_BYTES, SCAN_MAX_US,
    SPACE_PHYS, SPACE_VIRT,
};

/// mem ABI 版本（`MEM_QUERY` 的返回值）。
pub const ABI_VERSION: u16 = 1;

/// 当前上下文能否安全访问 guest 物理内存。
///
/// 直接映射只存在于 host 页表；`_start` 阶段用的是系统 CR3，此时访问会命中错误映射。
/// 所有 hypercall 入口都先过这一关。
#[inline]
pub fn available() -> bool {
    unsafe { x86::controlregs::cr3() == crate::hv::get_host_cr3() }
}

/// 每次 `MEM_BATCH` 的载荷上限：`MAX_BATCH_OPS × MAX_OP_BYTES`。
#[inline]
pub const fn max_batch_bytes() -> u32 {
    MAX_BATCH_BYTES
}

/// `MEM_STATS` 的条目（48 字节）。每 CPU 一条。
#[repr(C)]
#[derive(Debug, Clone, Copy, Default)]
pub struct MemStats {
    pub cpu: u32,
    pub reserved: u32,
    pub reads: u64,
    pub writes: u64,
    pub faults: u64,
    pub scanned: u64,
    pub root_tsc: u64,
}

const _: () = assert!(core::mem::size_of::<MemStats>() == 48);

/// 按 CPU 拆分的计数器。
///
/// 全部是原子的：`MEM_BATCH` 可以由不同 CPU 并发执行，各自累加，读取时按 CPU 汇总。
pub mod stats {
    use core::sync::atomic::{AtomicU64, Ordering};

    use super::MemStats;
    use crate::arch::x86_64::vcpu::MAX_CPUS;

    pub struct MemCounters {
        reads: AtomicU64,
        writes: AtomicU64,
        faults: AtomicU64,
        scanned: AtomicU64,
        root_tsc: AtomicU64,
    }

    impl MemCounters {
        pub const fn new() -> Self {
            Self {
                reads: AtomicU64::new(0),
                writes: AtomicU64::new(0),
                faults: AtomicU64::new(0),
                scanned: AtomicU64::new(0),
                root_tsc: AtomicU64::new(0),
            }
        }
    }

    static COUNTERS: [MemCounters; MAX_CPUS] = [const { MemCounters::new() }; MAX_CPUS];

    #[inline]
    fn slot(cpu: usize) -> &'static MemCounters {
        &COUNTERS[cpu.min(MAX_CPUS - 1)]
    }

    /// 批量环累加读写与失败次数。
    ///
    /// **不要把载荷字节算进 `scanned`**：`scanned` 只表示 `MEM_SCAN` 扫过的字节，
    /// 两者混在一起会让 `MEM_STATS` 失去意义。
    pub fn record(cpu: usize, reads: u64, writes: u64, faults: u64) {
        let counters = slot(cpu);

        if reads != 0 {
            counters.reads.fetch_add(reads, Ordering::Relaxed);
        }

        if writes != 0 {
            counters.writes.fetch_add(writes, Ordering::Relaxed);
        }

        if faults != 0 {
            counters.faults.fetch_add(faults, Ordering::Relaxed);
        }
    }

    /// 扫描单独计：`bytes` 计入 `scanned`，`tsc` 计入 `root_tsc`。
    pub fn record_scan(cpu: usize, bytes: u64, tsc: u64) {
        let counters = slot(cpu);

        if bytes != 0 {
            counters.scanned.fetch_add(bytes, Ordering::Relaxed);
        }

        if tsc != 0 {
            counters.root_tsc.fetch_add(tsc, Ordering::Relaxed);
        }
    }

    /// 取某个 CPU 的快照；超出范围返回 `None`。
    pub fn snapshot(index: usize) -> Option<MemStats> {
        if index >= MAX_CPUS {
            return None;
        }

        let counters = &COUNTERS[index];

        Some(MemStats {
            cpu: index as u32,
            reserved: 0,
            reads: counters.reads.load(Ordering::Relaxed),
            writes: counters.writes.load(Ordering::Relaxed),
            faults: counters.faults.load(Ordering::Relaxed),
            scanned: counters.scanned.load(Ordering::Relaxed),
            root_tsc: counters.root_tsc.load(Ordering::Relaxed),
        })
    }

    /// 可用 CPU 槽位（`MEM_STATS` 的 `total`）。
    pub const fn capacity() -> usize {
        MAX_CPUS
    }
}
