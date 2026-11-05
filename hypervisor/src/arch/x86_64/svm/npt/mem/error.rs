use crate::arch::x86_64::guest_memory::MemoryFault;

/// Error codes are ABI (`MEM_BATCH` status and `RAX`); append only.
// ABI error codes; not every backend produces every variant.
#[allow(dead_code)]
#[repr(u16)]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum MemError {
    Ok = 0,
    NotPresent = 1,
    NonCanonical = 2,
    OutOfMap = 3,
    BadCr3 = 4,
    NotRam = 5,
    TooLarge = 6,
    BadEntry = 7,
    Busy = 8,
    HostFault = 9,
    Unsupported = 10,
    Hooked = 11,
    Unaligned = 12,
}

impl MemError {
    pub const fn as_u16(self) -> u16 {
        self as u16
    }

    pub const fn as_u64(self) -> u64 {
        self as u64
    }
}

impl From<MemoryFault> for MemError {
    fn from(fault: MemoryFault) -> Self {
        match fault {
            MemoryFault::NonCanonical => MemError::NonCanonical,
            MemoryFault::Page(_) => MemError::NotPresent,
        }
    }
}

