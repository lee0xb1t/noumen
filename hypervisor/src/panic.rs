//! Panic handler.
//!
//! The register snapshot, loader arguments and symbol-based backtrace live
//! behind the `panic-detail` feature (on by default). The feature is
//! independent of the sinks: the report goes to COM1 when `serial-console` is
//! enabled and into the loader's ring when `buffer-log` is, so a serial-free
//! image can keep the full report:
//! `--no-default-features --features panic-detail,buffer-log`.
//!
//! Release builds that are shipped to users can drop the whole block with
//! `--no-default-features`, keeping only the panic message and source location.

use core::fmt::Write;
use core::panic::PanicInfo;

#[cfg(feature = "panic-detail")]
use core::arch::asm;

#[cfg(feature = "serial-console")]
use crate::arch::x86_64::serial;
#[cfg(feature = "panic-detail")]
use crate::hv;

#[cfg(feature = "panic-detail")]
#[repr(C)]
#[derive(Debug, Clone, Copy)]
pub struct Elf64_Sym {
    pub st_name: u32,
    pub st_info: u8,
    pub st_other: u8,
    pub st_shndx: u16,
    pub st_value: u64,
    pub st_size: u64,
}

#[cfg(feature = "panic-detail")]
impl Elf64_Sym {
    #[inline]
    #[allow(dead_code)]
    pub fn binding(&self) -> u8 {
        self.st_info >> 4
    }

    #[inline]
    #[allow(dead_code)]
    pub fn type_(&self) -> u8 {
        self.st_info & 0x0F
    }
}

#[cfg(feature = "panic-detail")]
pub struct Symbols {
    syms: *const Elf64_Sym,
    sym_count: u16,
    strs: *const u8,
}

#[cfg(feature = "panic-detail")]
impl Symbols {
    pub fn new() -> Self {
        let kargs = hv::GLOBAL_ARGS.get();
        let symbols = kargs.sym_base as *const Elf64_Sym;
        let strs = kargs.str_base as *const u8;

        Self {
            syms: symbols,
            sym_count: kargs.sym_count,
            strs,
        }
    }

    #[inline]
    unsafe fn get_symbol_name<'a>(&self, offset: u32) -> &'a str {
        let ptr = unsafe { self.strs.add(offset as usize) };
        let mut len = 0;
        while unsafe { *ptr.add(len) } != 0 {
            len += 1;
        }
        unsafe { core::str::from_utf8_unchecked(core::slice::from_raw_parts(ptr, len)) }
    }

    pub unsafe fn walk_syms<F>(&self, mut callback: F)
    where
        F: FnMut(&str, usize, usize) -> bool,
    {
        unsafe {
            let kargs = hv::GLOBAL_ARGS.get();
            let kbase = kargs.kernel_base;

            let mut ptr = self.syms;
            let count = self.sym_count;

            for _ in 0..count {
                let sym = &*ptr;

                let name = if sym.st_name == 0 {
                    ""
                } else {
                    self.get_symbol_name(sym.st_name)
                };

                if callback(
                    name,
                    (sym.st_value as usize).wrapping_add(kbase),
                    sym.st_size as usize,
                ) {
                    break;
                }

                ptr = ptr.add(1);
            }
        }
    }
}

#[cfg(feature = "panic-detail")]
#[repr(C)]
struct Backtrace {
    prev_frame: *const Backtrace,
    ret: u64,
}

#[cfg(feature = "panic-detail")]
impl Backtrace {
    #[inline(always)]
    pub fn new() -> *const Self {
        let rbp = get_frame_pointer() as usize;
        rbp as *const Self
    }

    #[inline(always)]
    unsafe fn next(&self) -> *const Self {
        self.prev_frame
    }

    #[inline(always)]
    unsafe fn ret_address(&self) -> usize {
        self.ret as usize
    }

    /// Print the backtrace through `writer`.
    ///
    /// The writer is passed in instead of going through `klog!` on purpose: the
    /// panic may have interrupted a `klog!` call that still holds the console
    /// lock, and taking it here would deadlock before the report is printed.
    pub fn dump_trace<W: Write>(writer: &mut W) {
        let symbols = Symbols::new();
        let mut frame = Backtrace::new();

        let mut depth = 0;

        while !frame.is_null() && depth < 32 {
            unsafe {
                let rip = (*frame).ret_address();

                let mut best_name_ptr = core::ptr::null();
                let mut best_name_len = 0;
                let mut best_addr = 0;
                let mut best_size = 0;

                symbols.walk_syms(|name, sym_addr, sym_size| {
                    if sym_addr <= rip && sym_addr > best_addr {
                        best_name_ptr = name.as_ptr();
                        best_name_len = name.len();
                        best_addr = sym_addr;
                        best_size = sym_size;
                    }
                    false
                });

                if !best_name_ptr.is_null() {
                    let name_bytes = core::slice::from_raw_parts(best_name_ptr, best_name_len);
                    let name = core::str::from_utf8_unchecked(name_bytes);
                    let offset = rip - best_addr;
                    let in_range = best_size == 0 || offset < best_size;
                    let mark = if in_range { '+' } else { '?' };
                    let _ = writeln!(
                        writer,
                        "#{}  RIP = {:#018x}  {} {}{:#x}",
                        depth, rip, name, mark, offset
                    );
                } else {
                    let _ = writeln!(writer, "#{}  RIP = {:#018x}  <unknown>", depth, rip);
                }

                frame = (*frame).next();
                depth += 1;
            }
        }
    }
}

#[cfg(feature = "panic-detail")]
#[inline(never)]
fn get_stack_pointer() -> u64 {
    let rsp: u64;
    unsafe {
        asm!("mov {}, rsp", out(reg) rsp);
    }
    rsp
}

#[cfg(feature = "panic-detail")]
#[inline(never)]
fn get_frame_pointer() -> u64 {
    let rbp: u64;
    unsafe {
        asm!("mov {}, rbp", out(reg) rbp);
    }
    rbp
}

#[cfg(feature = "panic-detail")]
#[inline(never)]
fn read_general_registers() -> (u64, u64, u64, u64, u64, u64, u64) {
    let rax: u64;
    let rbx: u64;
    let rcx: u64;
    let rdx: u64;
    let rsi: u64;
    let rdi: u64;
    let rbp: u64;
    unsafe {
        asm!(
            "mov {}, rax",
            "mov {}, rbx",
            "mov {}, rcx",
            "mov {}, rdx",
            "mov {}, rsi",
            "mov {}, rdi",
            "mov {}, rbp",
            out(reg) rax,
            out(reg) rbx,
            out(reg) rcx,
            out(reg) rdx,
            out(reg) rsi,
            out(reg) rdi,
            out(reg) rbp,
        );
    }
    (rax, rbx, rcx, rdx, rsi, rdi, rbp)
}

/// Maximum payload of one ring record, excluding the NUL terminator.
#[cfg(feature = "buffer-log")]
const PANIC_LINE_MAX: usize = crate::log::MAX_MSG_LENGTH - 1;

/// Splits formatted panic output into ring records.
///
/// A record is flushed on every newline and whenever the next character would
/// not fit, so long lines reach the ring in pieces instead of being truncated.
/// [`crate::log::log_panic`] takes the ring with a try-lock: a panic that
/// interrupted a `klog!` owning the ring drops records instead of deadlocking.
#[cfg(feature = "buffer-log")]
struct PanicLines {
    bytes: [u8; PANIC_LINE_MAX],
    len: usize,
}

#[cfg(feature = "buffer-log")]
impl PanicLines {
    const fn new() -> Self {
        Self {
            bytes: [0; PANIC_LINE_MAX],
            len: 0,
        }
    }

    fn push(&mut self, text: &str) {
        for ch in text.chars() {
            if ch == '\n' {
                self.flush();
                continue;
            }

            let mut encoded = [0u8; 4];
            let encoded = ch.encode_utf8(&mut encoded).as_bytes();

            if self.len + encoded.len() > PANIC_LINE_MAX {
                self.flush();
            }

            self.bytes[self.len..self.len + encoded.len()].copy_from_slice(encoded);
            self.len += encoded.len();
        }
    }

    fn flush(&mut self) {
        if self.len == 0 {
            return;
        }

        // Only whole characters are appended, so this cannot fail.
        if let Ok(line) = core::str::from_utf8(&self.bytes[..self.len]) {
            crate::log::log_panic(format_args!("{}", line));
        }
        self.len = 0;
    }
}

/// Panic report sink.
///
/// Writes to every sink that is compiled in and to none otherwise, so the
/// panic handler needs no `cfg` of its own. Serial output bypasses
/// [`crate::log`]'s console lock on purpose: the panic may have interrupted a
/// `klog!` that still holds it.
struct PanicWriter {
    #[cfg(feature = "serial-console")]
    serial: serial::SerialWriter,
    #[cfg(feature = "buffer-log")]
    lines: PanicLines,
}

impl PanicWriter {
    const fn new() -> Self {
        Self {
            #[cfg(feature = "serial-console")]
            serial: serial::SerialWriter,
            #[cfg(feature = "buffer-log")]
            lines: PanicLines::new(),
        }
    }

    /// Flush a trailing partial line; the report normally ends with a newline.
    fn finish(&mut self) {
        #[cfg(feature = "buffer-log")]
        self.lines.flush();
    }
}

impl Write for PanicWriter {
    fn write_str(&mut self, text: &str) -> core::fmt::Result {
        #[cfg(feature = "serial-console")]
        self.serial.write_str(text)?;

        #[cfg(feature = "buffer-log")]
        self.lines.push(text);

        #[cfg(not(any(feature = "serial-console", feature = "buffer-log")))]
        let _ = text;

        Ok(())
    }
}

#[panic_handler]
fn panic(info: &PanicInfo) -> ! {
    // Direct sink writes: the panic may have interrupted a `klog!` holding a lock.
    let mut writer = PanicWriter::new();

    let _ = writeln!(writer, "\n========================================");
    let _ = writeln!(writer, "            KERNEL PANIC               ");
    let _ = writeln!(writer, "========================================");

    let _ = writeln!(writer, "Message: {}", info.message());

    if let Some(loc) = info.location() {
        let _ = writeln!(writer, "Location: {}:{}", loc.file(), loc.line());
        let _ = writeln!(writer, "Column:   {}", loc.column());
    } else {
        let _ = writeln!(writer, "Location: (unknown)");
    }

    #[cfg(feature = "panic-detail")]
    {
        let rsp = get_stack_pointer();
        let _ = writeln!(writer, "RSP:      0x{:016x}", rsp);

        let rbp = get_frame_pointer();
        let _ = writeln!(writer, "RBP:      0x{:016x}", rbp);

        let rip = panic as *const () as u64;
        let _ = writeln!(writer, "RIP (approx): 0x{:016x}", rip);

        let (rax, rbx, rcx, rdx, rsi, rdi, rbp) = read_general_registers();
        let _ = writeln!(writer, "\n--- General Registers ---");
        let _ = writeln!(writer, "RAX: 0x{:016x}", rax);
        let _ = writeln!(writer, "RBX: 0x{:016x}", rbx);
        let _ = writeln!(writer, "RCX: 0x{:016x}", rcx);
        let _ = writeln!(writer, "RDX: 0x{:016x}", rdx);
        let _ = writeln!(writer, "RSI: 0x{:016x}", rsi);
        let _ = writeln!(writer, "RDI: 0x{:016x}", rdi);
        let _ = writeln!(writer, "RBP: 0x{:016x}", rbp);

        let _ = writeln!(writer, "\n--- Initialize Args ---");
        let kargs = hv::GLOBAL_ARGS.get();
        let _ = writeln!(writer, "{:#?}", kargs);

        let _ = writeln!(writer, "\n--- Back trace ---");
        Backtrace::dump_trace(&mut writer);
    }

    let _ = writeln!(writer, "========================================");
    let _ = writeln!(writer, "System halted. No further output.\n");

    writer.finish();

    loop {
        core::hint::spin_loop();
    }
}
