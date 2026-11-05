//! 隐蔽性自测：只看 CPU 自己暴露的痕迹，不依赖 hypercall。
//!
//! 覆盖三类常见检测：
//!
//! - `CPUID.1:ECX[31]` hypervisor present 位；
//! - `CPUID.1:ECX[5]` VMX 支持位（被 VT 接管后仍然可见就露馅了）；
//! - leaf `0x40000000..` 的 hypervisor 厂商签名；
//! - `rdtsc; cpuid; rdtsc` 的差值（VM-exit 开销）。

use core::arch::x86_64::{__cpuid, _rdtsc};

/// `CPUID` 结果里所有可疑位。
pub fn cpuid_report() -> Vec<String> {
    let mut lines = Vec::new();
    let leaf1 = __cpuid(1);

    lines.push(format!(
        "CPUID.1:ECX[31] hypervisor present = {}",
        (leaf1.ecx >> 31) & 1
    ));
    lines.push(format!(
        "CPUID.1:ECX[5]  VMX = {}",
        (leaf1.ecx >> 5) & 1
    ));
    lines.push(format!(
        "CPUID.1:ECX[6]  SMX = {}",
        (leaf1.ecx >> 6) & 1
    ));

    let leaf = __cpuid(0x4000_0000);
    let mut vendor = Vec::new();

    for reg in [leaf.ebx, leaf.ecx, leaf.edx] {
        for byte in reg.to_le_bytes() {
            if byte.is_ascii_graphic() {
                vendor.push(byte as char);
            }
        }
    }

    lines.push(format!(
        "CPUID.0x40000000 max_leaf = {:#x} vendor = \"{}\"",
        leaf.eax,
        vendor.into_iter().collect::<String>()
    ));

    let leaf0 = __cpuid(0);
    lines.push(format!(
        "CPUID.0 vendor = {}{}{}{}{}{}{}{}{}{}{}{}",
        (leaf0.ebx & 0xff) as u8 as char,
        ((leaf0.ebx >> 8) & 0xff) as u8 as char,
        ((leaf0.ebx >> 16) & 0xff) as u8 as char,
        ((leaf0.ebx >> 24) & 0xff) as u8 as char,
        (leaf0.edx & 0xff) as u8 as char,
        ((leaf0.edx >> 8) & 0xff) as u8 as char,
        ((leaf0.edx >> 16) & 0xff) as u8 as char,
        ((leaf0.edx >> 24) & 0xff) as u8 as char,
        (leaf0.ecx & 0xff) as u8 as char,
        ((leaf0.ecx >> 8) & 0xff) as u8 as char,
        ((leaf0.ecx >> 16) & 0xff) as u8 as char,
        ((leaf0.ecx >> 24) & 0xff) as u8 as char,
    ));

    lines
}

/// `rdtsc; cpuid; rdtsc` 的差值：取 8 次，报告最小/平均。
pub fn rdtsc_report() -> Vec<String> {
    let mut samples = Vec::new();

    for _ in 0..8 {
        let t0 = unsafe { _rdtsc() };

        __cpuid(0);

        let t1 = unsafe { _rdtsc() };

        samples.push(t1.wrapping_sub(t0));
    }

    let min = samples.iter().copied().min().unwrap_or(0);
    let max = samples.iter().copied().max().unwrap_or(0);
    let avg = samples.iter().map(|v| *v as u128).sum::<u128>() / samples.len() as u128;

    vec![
        format!("rdtsc;cpuid;rdtsc avg = {avg} min = {min} max = {max}"),
        format!("samples = {samples:?}"),
        String::from("裸机通常 < 1000；几万说明每次 cpuid 都在 VM-exit"),
    ]
}
