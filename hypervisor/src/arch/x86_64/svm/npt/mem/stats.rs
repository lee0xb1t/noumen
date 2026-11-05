use core::sync::atomic::{AtomicU64, Ordering};

use crate::arch::x86_64::vcpu::MAX_CPUS;

/// `MEM_STATS` record (48 bytes), same ABI as the VMX backend.
#[repr(C)]
#[derive(Clone, Copy, Debug, Default)]
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

static READS: [AtomicU64; MAX_CPUS] = [const { AtomicU64::new(0) }; MAX_CPUS];
static WRITES: [AtomicU64; MAX_CPUS] = [const { AtomicU64::new(0) }; MAX_CPUS];
static FAULTS: [AtomicU64; MAX_CPUS] = [const { AtomicU64::new(0) }; MAX_CPUS];
static SCANNED: [AtomicU64; MAX_CPUS] = [const { AtomicU64::new(0) }; MAX_CPUS];
static ROOT_TSC: [AtomicU64; MAX_CPUS] = [const { AtomicU64::new(0) }; MAX_CPUS];

fn counter(counters: &[AtomicU64; MAX_CPUS], cpu: usize) -> &AtomicU64 {
    &counters[cpu.min(MAX_CPUS - 1)]
}

pub(super) fn record(cpu: usize, reads: u64, writes: u64, faults: u64) {
    if reads != 0 {
        counter(&READS, cpu).fetch_add(reads, Ordering::Relaxed);
    }

    if writes != 0 {
        counter(&WRITES, cpu).fetch_add(writes, Ordering::Relaxed);
    }

    if faults != 0 {
        counter(&FAULTS, cpu).fetch_add(faults, Ordering::Relaxed);
    }
}

/// Record a `MEM_SCAN` run: `bytes` goes to `scanned`, `tsc` to `root_tsc`.
pub fn record_scan(cpu: usize, bytes: u64, tsc: u64) {
    if bytes != 0 {
        counter(&SCANNED, cpu).fetch_add(bytes, Ordering::Relaxed);
    }

    if tsc != 0 {
        counter(&ROOT_TSC, cpu).fetch_add(tsc, Ordering::Relaxed);
    }
}

pub(super) fn snapshot(cpu: usize) -> MemStats {
    MemStats {
        cpu: cpu as u32,
        reserved: 0,
        reads: counter(&READS, cpu).load(Ordering::Relaxed),
        writes: counter(&WRITES, cpu).load(Ordering::Relaxed),
        faults: counter(&FAULTS, cpu).load(Ordering::Relaxed),
        scanned: counter(&SCANNED, cpu).load(Ordering::Relaxed),
        root_tsc: counter(&ROOT_TSC, cpu).load(Ordering::Relaxed),
    }
}


pub(super) fn stats_as_bytes(stats: &MemStats) -> [u8; 48] {
    let mut raw = [0u8; 48];
    raw[0..4].copy_from_slice(&stats.cpu.to_le_bytes());
    raw[4..8].copy_from_slice(&stats.reserved.to_le_bytes());
    raw[8..16].copy_from_slice(&stats.reads.to_le_bytes());
    raw[16..24].copy_from_slice(&stats.writes.to_le_bytes());
    raw[24..32].copy_from_slice(&stats.faults.to_le_bytes());
    raw[32..40].copy_from_slice(&stats.scanned.to_le_bytes());
    raw[40..48].copy_from_slice(&stats.root_tsc.to_le_bytes());
    raw
}

