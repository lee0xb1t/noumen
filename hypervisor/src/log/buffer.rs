//! Fixed 512-entry log ring, either supplied by the loader or owned by the
//! hypervisor.
//!
//! The ring lives in memory the hypervisor can address directly. The loader
//! normally allocates it and passes its kernel virtual address in
//! `KernelArgs.log_base`; when that field is `0` the ring is allocated from the
//! page pool instead (`internal_ring`), so logging works without loader
//! support. Either way `sequence` is a seqlock (odd while writing), so an
//! external reader can take a consistent snapshot without taking the lock.
//! `LoggerMsg` is the 152-byte guest ABI.

use core::{
    fmt::{self, Write},
    mem::{offset_of, size_of},
    ptr,
    sync::atomic::{AtomicBool, AtomicPtr, AtomicU64, Ordering},
};

use crate::arch::x86_64::{memory, sync};
use crate::klog_verbose;

/// Text capacity of one record, including the NUL.
pub const MAX_MSG_LENGTH: usize = 128;

/// Ring capacity.
pub const MAX_MSG_COUNT: usize = 512;

/// Guest-visible record size; keep in sync with user mode.
pub const LOGGER_MSG_SIZE: usize = 152;

const SIGNATURE: [u8; 16] = *b"noumen.hv.log\0\0\0";

/// One ring record, mirrored byte-for-byte by user mode:
///
/// ```c
/// typedef struct {
///     uint64_t id;
///     uint64_t tsc;
///     uint32_t aux;
///     char data[128];
/// } logger_msg;             /* sizeof == 152 */
/// ```
///
/// `id` starts at 1; gaps mean overwritten records. `data` is NUL-terminated.
/// `tsc` is the host TSC; `aux` is `IA32_TSC_AUX` (0 without RDTSCP).
#[repr(C)]
#[derive(Clone, Copy)]
pub struct LoggerMsg {
    pub id: u64,
    pub tsc: u64,
    pub aux: u32,
    pub data: [u8; MAX_MSG_LENGTH],
    /// Explicit tail padding; C gets the same four bytes from the compiler.
    pub reserved: [u8; 4],
}

impl LoggerMsg {
    /// The record as it is copied to the guest.
    pub fn as_bytes(&self) -> &[u8] {
        // SAFETY: `Self` is `repr(C)` and every field, including `reserved`, is
        // initialized, so the whole object is valid to read as bytes.
        unsafe { core::slice::from_raw_parts((self as *const Self).cast(), size_of::<Self>()) }
    }
}

const _: () = {
    assert!(size_of::<LoggerMsg>() == LOGGER_MSG_SIZE);
    assert!(offset_of!(LoggerMsg, id) == 0);
    assert!(offset_of!(LoggerMsg, tsc) == 8);
    assert!(offset_of!(LoggerMsg, aux) == 16);
    assert!(offset_of!(LoggerMsg, data) == 20);
    assert!(offset_of!(LoggerMsg, reserved) == 148);
};

/// Shared ring header and records. Either the loader supplies this memory and
/// reads it directly, or [`internal_ring`] allocates it from the page pool;
/// `sequence` makes an external read snapshot-safe.
#[repr(C)]
pub struct LoggerData {
    pub signature: [u8; 16],
    pub sequence: AtomicU64,
    pub msg_start: u32,
    pub msg_count: u32,
    pub total_msg_count: u64,
    pub dropped_msg_count: u64,
    pub msgs: [LoggerMsg; MAX_MSG_COUNT],
}

/// Bytes one ring needs: the minimum `KernelArgs.log_size` for a loader-supplied
/// ring, and the amount the internal ring takes from the page pool.
pub const LOGGER_BUFFER_SIZE: usize = size_of::<LoggerData>();

const _: () = {
    assert!(offset_of!(LoggerData, signature) == 0);
    assert!(offset_of!(LoggerData, sequence) == 16);
    assert!(offset_of!(LoggerData, msg_start) == 24);
    assert!(offset_of!(LoggerData, msg_count) == 28);
    assert!(offset_of!(LoggerData, total_msg_count) == 32);
    assert!(offset_of!(LoggerData, dropped_msg_count) == 40);
    assert!(offset_of!(LoggerData, msgs) == 48);
    assert!(size_of::<LoggerData>() == 77872);
};

static LOGGER_LOCK: sync::Mutex<()> = sync::Mutex::new(());

/// Pointer to the active ring (loader buffer or internal pool pages); null
/// disables buffered logs.
static LOGGER_DATA: AtomicPtr<LoggerData> = AtomicPtr::new(ptr::null_mut());

/// Set once by [`init`]; RDTSCP is not present on every VMX-capable CPU.
static HAS_RDTSCP: AtomicBool = AtomicBool::new(false);

/// Binds a ring and resets it. Call once at boot, before the first `klog!`.
///
/// `virtual_address` is the kernel virtual address of the loader-supplied ring;
/// `0` means "hypervisor-owned", in which case the ring comes from the page pool
/// and `size` is ignored. The page pool must be initialized first
/// (`memory::init`); if it is not, the internal ring cannot be allocated and
/// logging simply stays disabled instead of panicking during boot.
pub fn init(virtual_address: u64, size: u32) {
    let Some(logger) = ring_address(virtual_address, size) else {
        return;
    };

    {
        let _guard = LOGGER_LOCK.lock();
        unsafe {
            (*logger).signature = SIGNATURE;
            (*logger).sequence.store(0, Ordering::SeqCst);
            (*logger).msg_start = 0;
            (*logger).msg_count = 0;
            (*logger).total_msg_count = 0;
            (*logger).dropped_msg_count = 0;
        }
        LOGGER_DATA.store(logger, Ordering::Release);
    }

    let has_rdtscp = x86::cpuid::CpuId::new()
        .get_extended_processor_and_feature_identifiers()
        .is_some_and(|info| info.has_rdtscp());
    HAS_RDTSCP.store(has_rdtscp, Ordering::Relaxed);

    // The ring lock is free again, so this can go through the normal path.
    klog_verbose!(
        "log ring: va={:#x} internal={} size={}",
        virtual_address,
        virtual_address == 0,
        size
    );
}

/// Where the ring lives: `0` allocates an internal one, anything else is the
/// loader buffer and must pass the ABI checks (canonical kernel address, 8-byte
/// aligned, at least [`LOGGER_BUFFER_SIZE`] bytes).
fn ring_address(virtual_address: u64, size: u32) -> Option<*mut LoggerData> {
    if virtual_address == 0 {
        return internal_ring();
    }

    let upper = virtual_address >> 48;
    if (size as usize) < LOGGER_BUFFER_SIZE || virtual_address & 7 != 0 || upper != 0xffff {
        return None;
    }

    Some(virtual_address as *mut LoggerData)
}

/// Allocates the hypervisor-owned ring from the page pool.
///
/// `Page` only borrows pool memory and has no destructor; the bump allocator
/// never hands the same pages out twice, so the raw pointer stays valid for the
/// rest of the hypervisor's lifetime.
fn internal_ring() -> Option<*mut LoggerData> {
    if !memory::is_initialized() {
        return None;
    }

    let page = memory::alloc::<LoggerData>().ok()?;

    Some(page.virt().as_mut_ptr())
}

/// Formats on the stack, then appends under the ring lock.
pub fn write_fmt(args: fmt::Arguments<'_>) {
    let message = format_message(args);
    let (tsc, aux) = timestamp();

    let _guard = LOGGER_LOCK.lock();
    if let Some(logger) = data_mut() {
        push_locked(logger, message.as_bytes(), tsc, aux);
    }
}

/// Non-blocking variant for contexts that may already hold the ring lock.
pub fn try_write_fmt(args: fmt::Arguments<'_>) {
    let message = format_message(args);
    let (tsc, aux) = timestamp();

    if let Some(_guard) = LOGGER_LOCK.try_lock() {
        if let Some(logger) = data_mut() {
            push_locked(logger, message.as_bytes(), tsc, aux);
        }
    }
}

/// Ring counters.
#[derive(Debug, Clone, Copy)]
pub struct LoggerStats {
    pub total: u64,
    pub dropped: u64,
    pub buffered: u32,
}

/// Counter snapshot.
pub fn stats() -> LoggerStats {
    let _guard = LOGGER_LOCK.lock();
    let Some(logger) = data_mut() else {
        return LoggerStats {
            total: 0,
            dropped: 0,
            buffered: 0,
        };
    };

    LoggerStats {
        total: logger.total_msg_count,
        dropped: logger.dropped_msg_count,
        buffered: logger.msg_count.min(MAX_MSG_COUNT as u32),
    }
}

/// Copies oldest-first. On failure returns `None` without advancing the ring;
/// the ring lock is held for the whole copy.
pub fn flush<F>(capacity: u32, mut copy: F) -> Option<u32>
where
    F: FnMut(&LoggerMsg) -> bool,
{
    let _guard = LOGGER_LOCK.lock();
    let Some(logger) = data_mut() else {
        return Some(0);
    };

    // Defensive clamp: a stray write must not index past `msgs`.
    let count = capacity.min(logger.msg_count).min(MAX_MSG_COUNT as u32);
    if count == 0 {
        return Some(0);
    }

    let start = (logger.msg_start as usize) % MAX_MSG_COUNT;
    for i in 0..count as usize {
        let index = (start + i) % MAX_MSG_COUNT;
        if !copy(&logger.msgs[index]) {
            return None;
        }
    }

    logger.sequence.fetch_add(1, Ordering::SeqCst);
    logger.msg_start = ((start + count as usize) % MAX_MSG_COUNT) as u32;
    logger.msg_count -= count;
    logger.sequence.fetch_add(1, Ordering::SeqCst);

    Some(count)
}

fn timestamp() -> (u64, u32) {
    if HAS_RDTSCP.load(Ordering::Relaxed) {
        // SAFETY: RDTSCP was probe-checked; TSD only applies at CPL > 0.
        unsafe { x86::time::rdtscp() }
    } else {
        // SAFETY: as above.
        (unsafe { x86::time::rdtsc() }, 0)
    }
}

fn data_mut() -> Option<&'static mut LoggerData> {
    let logger = LOGGER_DATA.load(Ordering::Acquire);
    if logger.is_null() {
        None
    } else {
        // SAFETY: the pointer is set once by `init` and the caller holds the lock.
        unsafe { Some(&mut *logger) }
    }
}

fn push_locked(logger: &mut LoggerData, data: &[u8], tsc: u64, aux: u32) {
    let index = (logger.msg_start as usize + logger.msg_count as usize) % MAX_MSG_COUNT;

    logger.sequence.fetch_add(1, Ordering::SeqCst);

    if (logger.msg_count as usize) < MAX_MSG_COUNT {
        logger.msg_count += 1;
    } else {
        logger.msg_start = ((logger.msg_start as usize + 1) % MAX_MSG_COUNT) as u32;
        logger.dropped_msg_count = logger.dropped_msg_count.saturating_add(1);
    }

    logger.total_msg_count = logger.total_msg_count.saturating_add(1);
    let id = logger.total_msg_count;

    let message = &mut logger.msgs[index];
    message.id = id;
    message.tsc = tsc;
    message.aux = aux;
    message.data.fill(0);
    message.reserved = [0; 4];

    // Clamp again to keep the terminating NUL.
    let len = data.len().min(MAX_MSG_LENGTH - 1);
    message.data[..len].copy_from_slice(&data[..len]);

    logger.sequence.fetch_add(1, Ordering::SeqCst);
}

/// Fixed 128-byte formatter scratch; truncates at 127 bytes.
struct MessageBuffer {
    bytes: [u8; MAX_MSG_LENGTH],
    len: usize,
}

impl MessageBuffer {
    const fn new() -> Self {
        Self {
            bytes: [0; MAX_MSG_LENGTH],
            len: 0,
        }
    }

    fn as_bytes(&self) -> &[u8] {
        &self.bytes[..self.len]
    }
}

impl Write for MessageBuffer {
    fn write_str(&mut self, s: &str) -> fmt::Result {
        let capacity = MAX_MSG_LENGTH - 1 - self.len;
        let mut count = s.len().min(capacity);

        // Do not split a UTF-8 sequence.
        while count > 0 && !s.is_char_boundary(count) {
            count -= 1;
        }

        self.bytes[self.len..self.len + count].copy_from_slice(&s.as_bytes()[..count]);
        self.len += count;

        if count < s.len() {
            // Truncated: stop formatting.
            return Err(fmt::Error);
        }

        Ok(())
    }
}

fn format_message(args: fmt::Arguments<'_>) -> MessageBuffer {
    let mut message = MessageBuffer::new();
    let _ = message.write_fmt(args);
    message
}
