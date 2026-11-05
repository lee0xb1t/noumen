use crate::arch::x86_64::addr::VirtAddr;

// VMXON Region
bitfield::bitfield! {
    #[repr(transparent)]
    #[derive(Clone, Copy, Debug, Default)]
    pub struct VmxonHeader(u32);

    pub revision_id, set_revision_id: 30, 0;
    pub must_be_zero, set_must_be_zero: 31, 31;
}

impl VmxonHeader {
    pub const fn from_raw(raw: u32) -> Self {
        Self(raw)
    }

    pub const fn as_raw(&self) -> u32 {
        self.0
    }

    pub const fn from_revision_id(revision_id: u32) -> Self {
        Self(revision_id & 0x7FFFFFFF)
    }
}

// NOTE: neither the VMXON nor the VMCS region may be `Clone`/`Copy`. Both are
// addressed by physical address once handed to the hardware, so a by-value copy
// would only edit a snapshot that the CPU never looks at.
#[repr(C, align(4096))]
pub struct Vmxon {
    pub header: VmxonHeader,
    pub data: [u8; 4092],
}

impl Default for Vmxon {
    fn default() -> Self {
        Self::new()
    }
}

impl Vmxon {
    /// All-zero region, ready for the revision ID to be written by `VMXON`.
    pub const fn new() -> Self {
        Self {
            header: VmxonHeader::from_raw(0),
            data: [0; 4092],
        }
    }

    pub fn from_revision_id(revision_id: u32) -> Self {
        Self {
            header: VmxonHeader::from_revision_id(revision_id),
            data: [0u8; 4092],
        }
    }

    pub fn from_virt(addr: VirtAddr) -> *const Self {
        assert_eq!(
            addr.as_u64() % 0x1000,
            0,
            "The VMXON region must be aligned to an 8-byte boundary."
        );
        addr.as_ptr() as *const Self
    }

    pub fn from_virt_mut(addr: VirtAddr) -> *mut Self {
        assert_eq!(
            addr.as_u64() % 0x1000,
            0,
            "The VMXON region must be aligned to an 8-byte boundary."
        );
        addr.as_mut_ptr() as *mut Self
    }

    pub fn as_ptr(&self) -> *const u8 {
        self as *const Self as *const u8
    }

    pub fn as_mut_ptr(&mut self) -> *mut u8 {
        self as *mut Self as *mut u8
    }

    pub fn as_usize(&self) -> usize {
        self as *const Self as usize
    }

    pub fn as_mut_usize(&mut self) -> usize {
        self as *mut Self as usize
    }
}

// VMCS Region
bitfield::bitfield! {
    #[derive(Clone, Copy, Debug)]
    pub struct VmcsHeader(u32);

    pub revision_id, set_revision_id: 30, 0;
    pub shadow_vmcs_indicator, set_shadow_vmcs_indicator: 31, 31;
}

impl VmcsHeader {
    pub const fn from_raw(raw: u32) -> Self {
        Self(raw)
    }

    pub const fn as_raw(&self) -> u32 {
        self.0
    }

    pub const fn from_revision_id(revision_id: u32) -> Self {
        Self(revision_id & 0x7FFFFFFF)
    }
}

#[repr(C, align(4096))]
pub struct Vmcs {
    pub header: VmcsHeader,
    pub abort_indicator: u32,
    pub data: [u8; 4088],
}

impl Default for Vmcs {
    fn default() -> Self {
        Self::new()
    }
}

impl Vmcs {
    /// All-zero region, ready for the revision ID to be written by `VMCLEAR`.
    pub const fn new() -> Self {
        Self {
            header: VmcsHeader::from_raw(0),
            abort_indicator: 0,
            data: [0; 4088],
        }
    }

    pub fn from_virt(addr: VirtAddr) -> *const Self {
        assert_eq!(
            addr.as_u64() % 0x1000,
            0,
            "The VMCS region must be aligned to an 8-byte boundary."
        );

        addr.as_ptr() as *const Self
    }

    pub fn from_virt_mut(addr: VirtAddr) -> *mut Self {
        assert_eq!(
            addr.as_u64() % 0x1000,
            0,
            "The VMCS region must be aligned to an 8-byte boundary."
        );
        addr.as_mut_ptr() as *mut Self
    }

    pub fn as_ptr(&self) -> *const u8 {
        self as *const Self as *const u8
    }

    pub fn as_mut_ptr(&mut self) -> *mut u8 {
        self as *mut Self as *mut u8
    }

    pub fn as_usize(&self) -> usize {
        self as *const Self as usize
    }

    pub fn as_mut_usize(&mut self) -> usize {
        self as *mut Self as usize
    }
}

// Vmx Msr Bitmap
const MSR_ID_LOW_MIN: usize = 0x00000000;
const MSR_ID_LOW_MAX: usize = 0x00001FFF;
const MSR_ID_HIGH_MIN: usize = 0xC0000000;
const MSR_ID_HIGH_MAX: usize = 0xC0001FFF;

#[repr(C, align(4096))]
pub struct VmxMsrBitmap {
    pub rdmsr_low: [u8; 1024],
    pub rdmsr_high: [u8; 1024],
    pub wrmsr_low: [u8; 1024],
    pub wrmsr_high: [u8; 1024],
}

impl VmxMsrBitmap {
    /// An all-zero bitmap: every MSR access is handled by the guest without a
    /// VM exit. Bits are set later for the MSRs that must be intercepted.
    pub const fn new() -> Self {
        Self {
            rdmsr_low: [0; 1024],
            rdmsr_high: [0; 1024],
            wrmsr_low: [0; 1024],
            wrmsr_high: [0; 1024],
        }
    }

    pub fn from_virt(addr: usize) -> *const Self {
        addr as *const Self
    }

    pub fn from_virt_mut(addr: usize) -> *mut Self {
        addr as *mut Self
    }

    pub fn enable_exit_for_rdmsr(&mut self, msr: u32) {
        let (bitmap, index) = msr_slot(&mut self.rdmsr_low, &mut self.rdmsr_high, msr);
        bitmap[index / 8] |= 1u8 << (index % 8);
    }

    pub fn enable_exit_for_wrmsr(&mut self, msr: u32) {
        let (bitmap, index) = msr_slot(&mut self.wrmsr_low, &mut self.wrmsr_high, msr);
        bitmap[index / 8] |= 1u8 << (index % 8);
    }
}

fn msr_slot<'a>(
    low: &'a mut [u8; 1024],
    high: &'a mut [u8; 1024],
    msr: u32,
) -> (&'a mut [u8; 1024], usize) {
    let msr = msr as usize;

    if (MSR_ID_HIGH_MIN..=MSR_ID_HIGH_MAX).contains(&msr) {
        (high, msr - MSR_ID_HIGH_MIN)
    } else {
        assert!(
            (MSR_ID_LOW_MIN..=MSR_ID_LOW_MAX).contains(&msr),
            "MSR {:#x} is outside the MSR bitmap",
            msr
        );

        (low, msr)
    }
}
