use core::arch::asm;

pub struct HostExceptionInfo {
    pub has_occurred: bool,
    pub vector: u64,
    pub error: u64,
}

impl HostExceptionInfo {
    pub const fn new() -> Self {
        Self {
            has_occurred: false,
            vector: 0,
            error: 0,
        }
    }
}

pub fn wrmsr_safe(exception_info: &mut HostExceptionInfo, msr: u32, value: u64) -> bool {
    let low = value as u32;
    let high = (value >> 32) as u32;

    let out: u64;

    unsafe {
        asm!(
            "
            lea r10, [rip+2f]
            mov r11, r8
            
            wrmsr

            mov r9, 1
            jmp 3f
        2:
            # exception hander
            xor r9, r9
        
        3:
            # successfully
            ",
            in("r8") exception_info,
            in("ecx") msr,
            in("eax") low,
            in("edx") high,
            out("r9") out,
            out("r10") _,
            out("r11") _,
        );
    }

    out == 1
}

pub fn rdmsr_safe(exception_info: &mut HostExceptionInfo, msr: u32) -> Option<u64> {
    let low: u32;
    let high: u32;

    let out: u64;

    unsafe {
        asm!(
            "
            lea r10, [rip+2f]
            mov r11, r8 
            
            rdmsr

            mov r9, 1
            jmp 3f
        2:
            # exception hander
            xor r9, r9
        
        3:
            # successfully
            ",
            in("r8") exception_info,
            in("ecx") msr,
            out("eax") low,
            out("edx") high,
            out("r9") out,
            out("r10") _,
            out("r11") _,
        );
    }

    if out == 1 {
        Some(((high as u64) << 32) | low as u64)
    } else {
        None
    }
}

pub fn xsetbv_safe(exception_info: &HostExceptionInfo, idx: u32, value: u64) -> bool {
    let low = value as u32;
    let high = (value >> 32) as u32;

    let out: u64;

    unsafe {
        asm!(
            "
            lea r10, [rip+2f]
            mov r11, r8
            
            xsetbv

            mov r9, 1
            jmp 3f
        2:
            # exception hander
            xor r9, r9
        
        3:
            # successfully
            ",
            in("r8") exception_info,
            in("ecx") idx,
            in("eax") low,
            in("edx") high,
            out("r9") out,
            out("r10") _,
            out("r11") _,
        );
    }

    out == 1
}

/// `memcpy` that returns `false` on a host fault instead of faulting the
/// hypervisor. A partial prefix may be written.
///
/// `exception_info` is raw because the exception handler creates a `&mut` to it
/// while this function is suspended.
///
/// Not gated on `buffer-log`: guest-memory copies must also fault-safe when the
/// log buffer is compiled out.
pub fn memcpy_safe(
    exception_info: *mut HostExceptionInfo,
    dst: *mut u8,
    src: *const u8,
    len: usize,
) -> bool {
    let out: u64;

    unsafe {
        asm!(
            "
            cld
            lea r10, [rip+2f]
            mov r11, r8

            rep movsb

            mov r9, 1
            jmp 3f
        2:
            # exception handler
            xor r9, r9

        3:
            # success
            ",
            in("r8") exception_info,
            inout("rdi") dst => _,
            inout("rsi") src => _,
            inout("rcx") len => _,
            out("r9") out,
            out("r10") _,
            out("r11") _,
        );
    }

    out == 1
}
