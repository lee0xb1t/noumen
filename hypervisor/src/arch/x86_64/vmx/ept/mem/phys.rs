//! 客户机物理内存（GPA）的读写与物理区间。
//!
//! 全部走 host 页表的 identity 直接映射：`paging::phys_to_virt`。所有访问都带
//! `DIRECT_MAP_SIZE` 边界检查，并且用 `memcpy_safe` 把宿主 fault 变成 `HostFault`，
//! 因此**不会**把 MMIO 空洞或越界地址变成蓝屏。

use core::mem::size_of;

use crate::arch::x86_64::{
    addr::PhysAddr,
    paging::{self, DIRECT_MAP_SIZE},
    safe_routine::{self, HostExceptionInfo},
    sync,
    vmx::ept::mem::error::MemError,
};
use crate::kargs::MAX_PHYS_RANGES;

/// 一个物理内存区间（来自驱动的 `MmGetPhysicalMemoryRanges` 副本）。
#[repr(C)]
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct PhysRange {
    pub start: u64,
    pub size: u64,
}

impl PhysRange {
    pub const EMPTY: Self = Self { start: 0, size: 0 };

    pub const fn end(&self) -> u64 {
        self.start + self.size
    }
}

const EMPTY_RANGE: PhysRange = PhysRange::EMPTY;

struct PhysMap {
    /// 已尝试过从 `KernelArgs` 装载（失败也算，避免每次访问都重试）。
    loaded: bool,
    count: u32,
    entries: [PhysRange; MAX_PHYS_RANGES as usize],
}

impl PhysMap {
    const fn new() -> Self {
        Self {
            loaded: false,
            count: 0,
            entries: [EMPTY_RANGE; MAX_PHYS_RANGES as usize],
        }
    }
}

static PHYS_MAP: sync::Mutex<PhysMap> = sync::Mutex::new(PhysMap::new());

/// 把驱动提供的区间表读进静态缓存。
///
/// 表本身是 nonpaged 内核内存，用 `system_cr3` 读；读失败就停在已读到的条数上，
/// 后续按“无区间信息”处理。
fn ensure_loaded(map: &mut PhysMap) {
    if map.loaded {
        return;
    }

    map.loaded = true;

    let args = crate::hv::GLOBAL_ARGS.get();
    let count = args.phys_map_count.min(MAX_PHYS_RANGES);
    let base = args.phys_map_base as u64;

    if count == 0 || base == 0 {
        return;
    }

    let cr3 = args.system_cr3 as u64;

    for index in 0..count as usize {
        let offset = match (index as u64).checked_mul(size_of::<PhysRange>() as u64) {
            Some(offset) => offset,
            None => break,
        };
        let va = match base.checked_add(offset) {
            Some(va) => va,
            None => break,
        };

        let mut raw = [0u8; size_of::<PhysRange>()];
        if super::virt::read(cr3, va, &mut raw).is_err() {
            break;
        }

        let start = u64::from_le_bytes(raw[0..8].try_into().unwrap());
        let size = u64::from_le_bytes(raw[8..16].try_into().unwrap());

        // 结束项：全 0。驱动保证数组以全 0 项结尾。
        if size == 0 {
            break;
        }

        map.entries[map.count as usize] = PhysRange { start, size };
        map.count += 1;
    }
}

/// 把一个 GPA 区间映射进宿主可访问的窗口；越界返回 `OutOfMap`。
#[inline]
fn window(pa: u64, len: usize) -> Result<*mut u8, MemError> {
    let end = pa
        .checked_add(len as u64)
        .ok_or(MemError::OutOfMap)?;

    if end > DIRECT_MAP_SIZE {
        return Err(MemError::OutOfMap);
    }

    // 前置检查保证 `phys_to_virt` 的 assert 不会触发。
    Ok(paging::phys_to_virt(PhysAddr::new(pa)).as_mut_ptr::<u8>())
}

/// 从 GPA 读一段到本地切片。
pub fn read(pa: u64, out: &mut [u8]) -> Result<(), MemError> {
    if out.is_empty() {
        return Ok(());
    }

    let src = window(pa, out.len())?;
    let mut info = HostExceptionInfo::new();

    if safe_routine::memcpy_safe(&mut info, out.as_mut_ptr(), src, out.len()) {
        Ok(())
    } else {
        Err(MemError::HostFault)
    }
}

/// 把本地切片写到 GPA。
pub fn write(pa: u64, data: &[u8]) -> Result<(), MemError> {
    if data.is_empty() {
        return Ok(());
    }

    let dst = window(pa, data.len())?;
    let mut info = HostExceptionInfo::new();

    if safe_routine::memcpy_safe(&mut info, dst, data.as_ptr(), data.len()) {
        Ok(())
    } else {
        Err(MemError::HostFault)
    }
}

/// GPA → GPA 的拷贝。两侧都必须在直接映射内。
pub fn copy(src_pa: u64, dst_pa: u64, len: usize) -> Result<(), MemError> {
    if len == 0 {
        return Ok(());
    }

    let src = window(src_pa, len)?;
    let dst = window(dst_pa, len)?;
    let mut info = HostExceptionInfo::new();

    if safe_routine::memcpy_safe(&mut info, dst, src, len) {
        Ok(())
    } else {
        Err(MemError::HostFault)
    }
}

/// 从 `pa` 起的下一个 RAM 区间 `(start, end)`：
/// 落在区间内时 `start = pa`，落在空洞里时返回后面最近的区间。
pub fn next_range(pa: u64) -> Option<(u64, u64)> {
    let mut map = PHYS_MAP.lock();
    ensure_loaded(&mut map);

    if map.count == 0 {
        return (pa < DIRECT_MAP_SIZE).then_some((pa, DIRECT_MAP_SIZE));
    }

    let mut best: Option<(u64, u64)> = None;

    for range in &map.entries[..map.count as usize] {
        let end = range.end();

        if end <= pa {
            continue;
        }

        let start = range.start.max(pa);

        if best.is_none_or(|(best_start, _)| start < best_start) {
            best = Some((start, end));
        }
    }

    best
}

/// 区间总数（未装载或未提供时为 0）。
pub fn range_total() -> usize {
    let mut map = PHYS_MAP.lock();
    ensure_loaded(&mut map);
    map.count as usize
}

/// 按索引取区间。
pub fn range_at(index: usize) -> Option<PhysRange> {
    let mut map = PHYS_MAP.lock();
    ensure_loaded(&mut map);

    if index >= map.count as usize {
        return None;
    }

    Some(map.entries[index])
}
