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

use stats::{snapshot, stats_as_bytes};

pub const ABI_VERSION: u16 = 1;

/// NPT 恒等映射是一整段连续范围，所以 `MEM_MAP` 只有一个区间。
pub const fn range_total() -> u64 {
    1
}

/// Write the single [0, DIRECT_MAP_SIZE) range; returns how many were written.
pub fn write_ranges(destination: u64, capacity: usize, cr3: u64) -> u64 {
    if capacity == 0 {
        return 0;
    }

    let mut raw = [0u8; 16];
    raw[0..8].copy_from_slice(&0u64.to_le_bytes());
    raw[8..16].copy_from_slice(&DIRECT_MAP_SIZE.to_le_bytes());

    if write_bytes(cr3, destination, &raw).is_ok() {
        1
    } else {
        0
    }
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

