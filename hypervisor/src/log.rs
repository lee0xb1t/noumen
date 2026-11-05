//! COM1 console and in-memory ring logging.
//!
//! The two sinks are independent. The serial lock keeps interrupts off while
//! held; `klog_try!` is the non-blocking variant for exception/panic paths,
//! which may already own a log lock on this CPU.

#[cfg(any(feature = "serial-console", feature = "buffer-log"))]
use core::fmt;

#[cfg(feature = "serial-console")]
use core::fmt::Write;

#[cfg(feature = "buffer-log")]
pub(crate) mod buffer;

#[cfg(feature = "buffer-log")]
#[allow(unused_imports)]
pub use buffer::{
    LOGGER_MSG_SIZE, LoggerMsg, LoggerStats, MAX_MSG_COUNT, MAX_MSG_LENGTH, flush, stats,
};

#[cfg(feature = "serial-console")]
use crate::arch::x86_64::{serial, sync};

/// Binds the log ring and probes RDTSCP. Call once at boot, after the page pool
/// is initialized.
///
/// `virtual_address` is the kernel virtual address of the loader-supplied ring.
/// Passing `0` selects the hypervisor-owned internal ring instead: `buffer-log`
/// then allocates it from the page pool, so a loader that provides no log area
/// (for example kdmapper) still gets buffered logs. `size` is the loader buffer
/// size and is ignored in that case.
pub fn init(virtual_address: u64, size: u32) {
    #[cfg(feature = "buffer-log")]
    buffer::init(virtual_address, size);

    #[cfg(not(feature = "buffer-log"))]
    let _ = (virtual_address, size);
}

/// Log to the console and/or ring; expands to nothing if both are disabled.
#[macro_export]
macro_rules! klog {
    ($($arg:tt)*) => {{
        #[cfg(any(feature = "serial-console", feature = "buffer-log"))]
        {
            $crate::log::_klog_fmt(format_args!($($arg)*));
        }
        #[cfg(not(any(feature = "serial-console", feature = "buffer-log")))]
        {
            if false {
                let _ = format_args!($($arg)*);
            }
        }
    }};
}

/// Debug-only `klog!`; compiled out unless `debug_assertions` is on.
#[macro_export]
macro_rules! klog_verbose {
    ($($arg:tt)*) => {{
        #[cfg(all(
            debug_assertions,
            any(feature = "serial-console", feature = "buffer-log")
        ))]
        {
            $crate::log::_klog_fmt(format_args!($($arg)*));
        }
        #[cfg(not(all(
            debug_assertions,
            any(feature = "serial-console", feature = "buffer-log")
        )))]
        {
            if false {
                let _ = format_args!($($arg)*);
            }
        }
    }};
}

/// Non-blocking `klog!`; drops a record if a sink lock is contended.
#[macro_export]
macro_rules! klog_try {
    ($($arg:tt)*) => {{
        #[cfg(any(feature = "serial-console", feature = "buffer-log"))]
        {
            $crate::log::_klog_try_fmt(format_args!($($arg)*));
        }
        #[cfg(not(any(feature = "serial-console", feature = "buffer-log")))]
        {
            if false {
                let _ = format_args!($($arg)*);
            }
        }
    }};
}

#[cfg(feature = "serial-console")]
static LOG_LOCK: sync::Mutex<()> = sync::Mutex::new(());

#[cfg(any(feature = "serial-console", feature = "buffer-log"))]
pub fn _klog_fmt(args: fmt::Arguments<'_>) {
    // Ring first, console second; never nest the two locks.
    #[cfg(feature = "buffer-log")]
    buffer::write_fmt(args);

    #[cfg(feature = "serial-console")]
    {
        let _guard = LOG_LOCK.lock();

        let mut writer = serial::SerialWriter;
        let _ = writeln!(&mut writer, "{}", args);
    }
}

#[cfg(any(feature = "serial-console", feature = "buffer-log"))]
pub fn _klog_try_fmt(args: fmt::Arguments<'_>) {
    #[cfg(feature = "buffer-log")]
    buffer::try_write_fmt(args);

    #[cfg(feature = "serial-console")]
    if let Some(_guard) = LOG_LOCK.try_lock() {
        let mut writer = serial::SerialWriter;
        let _ = writeln!(&mut writer, "{}", args);
    }
}

/// Best-effort panic record in the ring; never blocks.
#[cfg(feature = "buffer-log")]
pub fn log_panic(args: fmt::Arguments<'_>) {
    buffer::try_write_fmt(args);
}
