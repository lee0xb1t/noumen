//! Guest memory service for the SVM backend: `MEM_QUERY` + `MEM_BATCH`.

pub mod pattern;
pub mod scan;

mod error;
mod phys;
mod ring;
mod stats;
mod virt;

pub use error::MemError;
pub use ring::*;

pub(crate) use phys::read_gpa;
pub(crate) use stats::record_scan;
pub(crate) use virt::{read_bytes, read_struct, write_bytes, write_struct};

use crate::arch::x86_64::{paging::DIRECT_MAP_SIZE, vcpu::MAX_CPUS};
use crate::kargs::MAX_PHYS_RANGES;

use stats::{snapshot, stats_as_bytes};

pub const ABI_VERSION: u16 = 1;

#[derive(Clone, Copy, Default)]
struct PhysRange {
    start: u64,
    size: u64,
}

struct PhysRangeMap {
    count: u32,
    entries: [PhysRange; MAX_PHYS_RANGES as usize],
}

impl PhysRangeMap {
    const fn new() -> Self {
        Self {
            count: 0,
            entries: [PhysRange { start: 0, size: 0 }; MAX_PHYS_RANGES as usize],
        }
    }
}

static PHYS_RANGES: spin::Mutex<PhysRangeMap> = spin::Mutex::new(PhysRangeMap::new());

/// NPT maps one contiguous [0, DIRECT_MAP_SIZE) window unless the loader
/// replaces it through `SetPhysMemoryRange`.
pub fn range_total() -> u64 {
    let ranges = PHYS_RANGES.lock();
    if ranges.count == 0 {
        1
    } else {
        ranges.count as u64
    }
}

/// Replace the SVM physical-range table from a guest `PHYSICAL_MEMORY_RANGE[]`.
pub fn set_ranges(cr3: u64, base: u64, count: u32) -> u32 {
    let mut ranges = PHYS_RANGES.lock();
    ranges.count = 0;

    if count == 0 || base == 0 {
        return 0;
    }

    let count = count.min(MAX_PHYS_RANGES);

    for index in 0..count as usize {
        let offset = match (index as u64).checked_mul(16) {
            Some(offset) => offset,
            None => break,
        };
        let va = match base.checked_add(offset) {
            Some(va) => va,
            None => break,
        };

        let mut raw = [0u8; 16];
        if read_bytes(cr3, va, &mut raw).is_err() {
            break;
        }

        let start = u64::from_le_bytes(raw[0..8].try_into().unwrap());
        let size = u64::from_le_bytes(raw[8..16].try_into().unwrap());

        if size == 0 {
            break;
        }

        let entry = ranges.count as usize;
        ranges.entries[entry] = PhysRange { start, size };
        ranges.count += 1;
    }

    ranges.count
}

/// Write the physical ranges; returns how many were written.
pub fn write_ranges(destination: u64, capacity: usize, cr3: u64) -> u64 {
    let ranges = PHYS_RANGES.lock();
    let total = if ranges.count == 0 {
        1
    } else {
        ranges.count as usize
    };
    let mut written = 0usize;

    while written < capacity && written < total {
        let (start, size) = if ranges.count == 0 {
            (0, DIRECT_MAP_SIZE)
        } else {
            let range = ranges.entries[written];
            (range.start, range.size)
        };

        let Some(address) = destination.checked_add((written * 16) as u64) else {
            break;
        };

        let mut raw = [0u8; 16];
        raw[0..8].copy_from_slice(&start.to_le_bytes());
        raw[8..16].copy_from_slice(&size.to_le_bytes());

        if write_bytes(cr3, address, &raw).is_err() {
            break;
        }

        written += 1;
    }

    written as u64
}

/// Write one 48-byte `MemStats` per CPU (up to `capacity`).
pub fn write_stats(destination: u64, capacity: usize, cr3: u64) -> u64 {
    let total = (crate::hv::GLOBAL_ARGS.get().num_cpus as usize).min(MAX_CPUS);
    let mut written = 0usize;

    while written < capacity && written < total {
        let stats = snapshot(written);
        let Some(address) = destination.checked_add((written * 48) as u64) else {
            break;
        };

        if write_bytes(cr3, address, &stats_as_bytes(&stats)).is_err() {
            break;
        }

        written += 1;
    }

    written as u64
}

