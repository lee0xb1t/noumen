//! TSC frequency estimation, shared by both backends.
//!
//! Breakpoint waits use an absolute TSC deadline (`RDTSC` + `deadline`), so
//! milliseconds must be converted to ticks. There is no PIT calibration in the
//! hypervisor; CPUID is enough:
//!
//! * `CPUID.15H`: `ECX` = crystal Hz, `EBX`/`EAX` = TSC/crystal ratio;
//! * `CPUID.16H`: `EAX` = base frequency in MHz;
//! * otherwise the fallback only makes timeouts inaccurate, it never leaves a
//!   thread parked on `INT3`.

use core::sync::atomic::{AtomicU64, Ordering};

use x86::cpuid::native_cpuid::cpuid_count;

const FALLBACK_TSC_HZ: u64 = 3_000_000_000;

/// This CPU's TSC frequency in Hz, cached after the first call.
pub fn tsc_hz() -> u64 {
    static HZ: AtomicU64 = AtomicU64::new(0);

    let cached = HZ.load(Ordering::Acquire);

    if cached != 0 {
        return cached;
    }

    let detected = detect_tsc_hz();
    HZ.store(detected, Ordering::Release);

    detected
}

fn detect_tsc_hz() -> u64 {
    let max_leaf = cpuid_count(0, 0).eax;

    if max_leaf >= 0x15 {
        let leaf = cpuid_count(0x15, 0);
        let denominator = leaf.eax as u64;
        let numerator = leaf.ebx as u64;
        let crystal_hz = leaf.ecx as u64;

        if denominator != 0 && numerator != 0 && crystal_hz != 0 {
            return crystal_hz.saturating_mul(numerator) / denominator;
        }
    }

    if max_leaf >= 0x16 {
        let base_mhz = cpuid_count(0x16, 0).eax as u64;

        if base_mhz != 0 {
            return base_mhz.saturating_mul(1_000_000);
        }
    }

    FALLBACK_TSC_HZ
}
