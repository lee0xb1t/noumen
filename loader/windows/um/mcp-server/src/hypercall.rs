//! Direct CPL3 hypercall path. No driver, IOCTL, or device object is used.
//!
//! ABI: `RAX = key`, `RCX = command`, `RDX = arg0`, `R8 = arg1`,
//! `R9 = arg2`; the hypervisor returns in `RAX`, and some operations also
//! return values in `RDX`/`R8`/`R9`.
//!
//! This module is deliberately the only place that knows hypercall numbers.
//! The HTTP/MCP layers expose named operations, never raw command numbers.

use std::io;
use std::sync::atomic::{AtomicU8, Ordering};

use windows_sys::Win32::System::Threading::{
    GetCurrentProcessorNumber, GetCurrentThread, SetThreadAffinityMask, Sleep,
};

static HYPERCALL_ISA: AtomicU8 = AtomicU8::new(0);

#[derive(Clone, Copy, Debug, Default)]
pub struct VmcallResult {
    pub rax: u64,
    pub rdx: u64,
    pub r8: u64,
    pub r9: u64,
}

fn is_amd() -> bool {
    match HYPERCALL_ISA.load(Ordering::Relaxed) {
        2 => return true,
        1 => return false,
        _ => {}
    }

    let leaf = core::arch::x86_64::__cpuid(0);
    let amd = leaf.ebx == 0x6874_7541 && leaf.edx == 0x6974_6E65 && leaf.ecx == 0x444D_4163;

    HYPERCALL_ISA.store(if amd { 2 } else { 1 }, Ordering::Relaxed);

    amd
}

/// Invoke one hypercall, optionally pinning the calling thread first.
pub fn call(
    key: u64,
    command: u64,
    arg0: u64,
    arg1: u64,
    arg2: u64,
    processor: Option<u32>,
) -> Result<VmcallResult, String> {
    if key == 0 {
        return Err("hypercall key is 0; the hypervisor will inject #UD".into());
    }

    let _pin = match processor {
        Some(cpu) => Some(PinnedThread::new(cpu).map_err(|error| {
            format!("could not pin calling thread to processor {cpu}: {error}")
        })?),
        None => None,
    };

    Ok(unsafe { vmcall3(key, command, arg0, arg1, arg2) })
}

#[cfg(target_arch = "x86_64")]
unsafe fn vmcall3(key: u64, command: u64, arg0: u64, arg1: u64, arg2: u64) -> VmcallResult {
    let mut result = VmcallResult {
        rax: key,
        rdx: arg0,
        r8: arg1,
        r9: arg2,
    };

    if is_amd() {
        unsafe {
            core::arch::asm!(
                "vmmcall",
                inlateout("rax") result.rax,
                in("rcx") command,
                inlateout("rdx") result.rdx,
                inlateout("r8") result.r8,
                inlateout("r9") result.r9,
                options(nostack),
            );
        }
    } else {
        unsafe {
            core::arch::asm!(
                "vmcall",
                inlateout("rax") result.rax,
                in("rcx") command,
                inlateout("rdx") result.rdx,
                inlateout("r8") result.r8,
                inlateout("r9") result.r9,
                options(nostack),
            );
        }
    }

    result
}

#[cfg(not(target_arch = "x86_64"))]
compile_error!("noumen hypercalls use the x86_64 VMCALL/VMMCALL ABI");

struct PinnedThread {
    previous: usize,
}

impl PinnedThread {
    fn new(processor: u32) -> io::Result<Self> {
        let mask = 1usize.checked_shl(processor).ok_or_else(|| {
            io::Error::new(
                io::ErrorKind::InvalidInput,
                format!("processor {processor} exceeds the 64-bit affinity mask"),
            )
        })?;

        let previous = unsafe { SetThreadAffinityMask(GetCurrentThread(), mask) };
        if previous == 0 {
            return Err(io::Error::last_os_error());
        }

        for _ in 0..1000 {
            if unsafe { GetCurrentProcessorNumber() } == processor {
                return Ok(Self { previous });
            }
            unsafe { Sleep(1) };
        }

        unsafe { SetThreadAffinityMask(GetCurrentThread(), previous) };

        Err(io::Error::new(
            io::ErrorKind::TimedOut,
            format!("thread did not migrate to processor {processor}"),
        ))
    }
}

impl Drop for PinnedThread {
    fn drop(&mut self) {
        unsafe {
            SetThreadAffinityMask(GetCurrentThread(), self.previous);
        }
    }
}
