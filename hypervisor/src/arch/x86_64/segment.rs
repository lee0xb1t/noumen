// Complete x86 descriptor/selector helpers; constructors are kept for the
// architectural layout even when only some are used by the current backends.
#![allow(dead_code)]

// GDT use x86_64 lib

use core::arch::asm;
use core::fmt::Debug;
use x86_64::structures::gdt;

pub const HOST_CS_SELECTOR: gdt::SegmentSelector =
    gdt::SegmentSelector::new(1, x86_64::PrivilegeLevel::Ring0);

pub const HOST_SS_SELECTOR: gdt::SegmentSelector =
    gdt::SegmentSelector::new(2, x86_64::PrivilegeLevel::Ring0);

pub const HOST_TR_SELECTOR: gdt::SegmentSelector =
    gdt::SegmentSelector::new(3, x86_64::PrivilegeLevel::Ring0);

pub const MAX_GDT_ENTRY: usize = 5;

pub const SEGMENT_DESCRIPTOR_TYPE_SYSTEM: u8 = 0;
pub const SEGMENT_DESCRIPTOR_TYPE_CODE_OR_DATA: u8 = 1;

#[repr(C, packed(2))]
#[derive(Debug)]
pub struct Gdtr {
    pub limit: u16,
    pub base_address: u64,
}

impl Gdtr {
    #[inline]
    pub fn new() -> Self {
        Self {
            limit: 0,
            base_address: 0,
        }
    }

    #[inline]
    pub fn sgdt() -> Self {
        let sgdt = x86_64::instructions::tables::sgdt();
        Self {
            limit: sgdt.limit,
            base_address: sgdt.base.as_u64(),
        }
    }
}

#[repr(C, packed)]
pub struct Idtr {
    pub limit: u16,
    pub base_address: u64,
}

impl Idtr {
    #[inline]
    pub fn new() -> Self {
        Self {
            limit: 0,
            base_address: 0,
        }
    }

    #[inline]
    pub fn sidt() -> Self {
        let sidt = x86_64::instructions::tables::sidt();
        Self {
            limit: sidt.limit,
            base_address: sidt.base.as_u64(),
        }
    }
}

#[repr(C, packed)]
pub struct SegmentSelector(u16);

impl SegmentSelector {
    pub fn new(sel: u16) -> Self {
        Self(sel)
    }

    pub fn index(&self) -> u16 {
        self.0 >> 3
    }

    /// Returns the TI (Table Indicator) bit (bit 2).
    pub fn ti(&self) -> u8 {
        ((self.0 >> 2) & 1) as u8
    }

    /// Returns the RPL (Requested Privilege Level) bits (bits 1:0).
    pub fn rpl(&self) -> u8 {
        (self.0 & 0b11) as u8
    }

    /// Check if this is a null selector.
    pub fn is_null(&self) -> bool {
        self.0 == 0
    }

    /// Check if this is a null selector.
    pub fn raw(&self) -> u16 {
        self.0
    }

    pub fn read_cs() -> Self {
        Self(x86::segmentation::cs().bits())
    }

    /// Read the current Data Segment (DS) selector.
    pub fn read_ds() -> Self {
        Self(x86::segmentation::ds().bits())
    }

    /// Read the current Stack Segment (SS) selector.
    pub fn read_ss() -> Self {
        Self(x86::segmentation::ss().bits())
    }

    /// Read the current Extra Segment (ES) selector.
    pub fn read_es() -> Self {
        Self(x86::segmentation::es().bits())
    }

    /// Read the current Task Register (TR) selector.
    pub fn read_tr() -> Self {
        Self(unsafe { x86::task::tr().bits() })
    }

    /// Read the current Local Descriptor Table Register (LDTR) selector.
    pub fn read_ldtr() -> Self {
        let result: u16;
        unsafe {
            asm!("sldt ax", out("ax") result, options(nomem, nostack));
        }
        Self(result)
    }

    pub fn read_fs() -> Self {
        Self(x86::segmentation::fs().bits())
    }

    pub fn read_gs() -> Self {
        Self(x86::segmentation::gs().bits())
    }
}

bitfield::bitfield! {
    #[repr(C)]
    #[derive(Clone, Copy, Debug)]
    pub struct SegmentDescriptorFlags(u32);

    /// Bits 7:0 – Base address middle (bits 23:16)
    pub base_address_middle, set_base_address_middle: 7, 0;

    /// Bits 11:8 – Segment type
    pub type_, set_type: 11, 8;

    /// Bit 12 – Descriptor type (S): 0 = system, 1 = code/data
    pub descriptor_type, set_descriptor_type: 12, 12;

    /// Bits 14:13 – Descriptor privilege level (DPL)
    pub descriptor_privilege_level, set_descriptor_privilege_level: 14, 13;

    /// Bit 15 – Present (P)
    pub present, set_present: 15, 15;

    /// Bits 19:16 – Segment limit high (bits 19:16)
    pub segment_limit_high, set_segment_limit_high: 19, 16;

    /// Bit 20 – Available for system software (AVL)
    pub available_bit, set_available_bit: 20, 20;

    /// Bit 21 – Long mode (L) – for code segments only
    pub long_mode, set_long_mode: 21, 21;

    /// Bit 22 – Default operation size / stack pointer size (D/B)
    pub default_big, set_default_big: 22, 22;

    /// Bit 23 – Granularity (G)
    pub granularity, set_granularity: 23, 23;

    /// Bits 31:24 – Base address high (bits 31:24)
    pub base_address_high, set_base_address_high: 31, 24;
}

impl SegmentDescriptorFlags {
    pub fn bits(&self) -> u32 {
        self.0
    }
}

/// Full 64-bit segment descriptor (16 bytes).
/// Layout matches the C structure exactly.
#[repr(C)]
#[derive(Clone, Copy)]
pub struct SegmentDescriptor64 {
    pub limit_low: u16,
    pub base_address_low: u16,
    pub flags: SegmentDescriptorFlags,
    pub base_address_upper: u32,
    pub reversed: u32,
}

impl SegmentDescriptor64 {
    /// Create a new zero-initialized descriptor.
    pub const fn new() -> Self {
        Self {
            limit_low: 0,
            base_address_low: 0,
            flags: SegmentDescriptorFlags(0),
            base_address_upper: 0,
            reversed: 0,
        }
    }

    /// Read a descriptor from a raw linear/physical address.
    ///
    /// # Safety
    /// The address must point to a valid descriptor in memory.
    #[inline]
    pub unsafe fn from_addr(addr: u64) -> Self {
        let ptr = addr as *const Self;
        unsafe { ptr.read_volatile() }
    }

    /// Read a descriptor from a raw pointer.
    #[inline]
    pub unsafe fn from_ptr(ptr: *const Self) -> Self {
        unsafe { ptr.read_unaligned() }
    }

    /// Return the full 64-bit base address.
    #[inline]
    pub fn base(&self) -> u64 {
        let flags = self.flags;
        let low = self.base_address_low as u64;
        let mid = flags.base_address_middle() as u64;
        let high = flags.base_address_high() as u64;
        let mut base = low | (mid << 16) | (high << 24);

        if flags.descriptor_type() as u8 == SEGMENT_DESCRIPTOR_TYPE_SYSTEM {
            base |= (self.base_address_upper as u64) << 32;
        }
        base
    }

    /// Return the full 32-bit segment limit (low + high).
    #[inline]
    pub fn limit(&self) -> u32 {
        let high = (self.flags.segment_limit_high() & 0xF) as u32;
        let low = self.limit_low as u32;
        (high << 16) | low
    }

    /// Check if the descriptor is null (all zeros).
    pub fn is_null(&self) -> bool {
        self.base_address_low == 0
            && self.flags.bits() == 0
            && self.base_address_upper == 0
            && self.reversed == 0
    }
}

impl Debug for SegmentDescriptor64 {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        let base = self.base();
        let limit = self.limit();
        let flags: SegmentDescriptorFlags = self.flags;
        let reversed = self.reversed;

        f.debug_struct("SegmentDescriptor64")
            .field("base", &base)
            .field("limit", &limit)
            .field("flags", &flags)
            .field("reversed", &reversed)
            .finish()
    }
}

pub fn segment_base(gdtr: &Gdtr, selector: &SegmentSelector) -> u64 {
    let index = selector.index();
    if index == 0 {
        return 0;
    }

    let offset = (index as u64) * 8;

    let desc_ptr = (gdtr.base_address + offset) as *const u8;
    let desc = unsafe { SegmentDescriptor64::from_ptr(desc_ptr as *const SegmentDescriptor64) };
    desc.base()
}

pub fn segment_limit(selector: &SegmentSelector) -> u32 {
    let limit: u32;
    unsafe {
        asm!(
            "lsl {limit:e}, {selector:x}",
            limit = out(reg) limit,
            selector = in(reg) selector.raw(),
            options(nostack, preserves_flags)
        );
    }
    limit
}

bitfield::bitfield! {
    /// 32-bit segment access rights field.
    /// This structure is layout-compatible with `u32` (`repr(transparent)`).
    #[derive(Clone, Copy, PartialEq, Eq)]
    pub struct VmxSegmentAccessRights(u32);
    impl Debug;

    /// Bits 3:0 – Segment type
    pub type_, set_type: 3, 0;

    /// Bit 4 – Descriptor type (0 = system, 1 = code/data)
    pub descriptor_type, set_descriptor_type: 4, 4;

    /// Bits 6:5 – Descriptor privilege level (DPL)
    pub descriptor_privilege_level, set_descriptor_privilege_level: 6, 5;

    /// Bit 7 – Segment present
    pub present, set_present: 7, 7;

    /// Bits 11:8 – Reserved (must be zero)
    pub reserved1, set_reserved1: 11, 8;

    /// Bit 12 – Available for system software
    pub available_bit, set_available_bit: 12, 12;

    /// Bit 13 – Long mode (for CS only)
    pub long_mode, set_long_mode: 13, 13;

    /// Bit 14 – Default operation size (0 = 16-bit, 1 = 32-bit)
    pub default_big, set_default_big: 14, 14;

    /// Bit 15 – Granularity
    pub granularity, set_granularity: 15, 15;

    /// Bit 16 – Segment unusable
    pub unusable, set_unusable: 16, 16;

    /// Bits 31:17 – Reserved (must be zero)
    pub reserved2, _: 31, 17;
}

impl VmxSegmentAccessRights {
    pub const fn new() -> Self {
        Self(0)
    }
    pub const fn from_flags(flags: u32) -> Self {
        Self(flags)
    }
    pub const fn flags(&self) -> u32 {
        self.0
    }
    pub unsafe fn from_raw(ptr: *const u8) -> Self {
        use core::ptr::read_unaligned;
        Self(unsafe { read_unaligned(ptr as *const u32) })
    }
    pub unsafe fn write_to_raw(&self, ptr: *mut u8) {
        use core::ptr::write_unaligned;
        unsafe { write_unaligned(ptr as *mut u32, self.0) };
    }
    pub fn reserved_fields_zero(&self) -> bool {
        self.reserved1() == 0 && self.reserved2() == 0
    }

    pub fn raw(&self) -> u32 {
        self.0
    }
}

pub fn segment_access(gdtr: &Gdtr, selector: &SegmentSelector) -> VmxSegmentAccessRights {
    let mut access = VmxSegmentAccessRights::new();

    let index = selector.index();
    if index == 0 {
        access.set_unusable(1);
    }

    let offset = (index as u64) * 8;

    let desc_ptr = (gdtr.base_address + offset) as *const u8;
    let desc = unsafe { SegmentDescriptor64::from_ptr(desc_ptr as *const SegmentDescriptor64) };

    let flags = desc.flags;

    access.set_type(flags.type_() as u32);
    access.set_descriptor_type(flags.descriptor_type() as u32);
    access.set_descriptor_privilege_level(flags.descriptor_privilege_level() as u32);
    access.set_present(flags.present() as u32);
    access.set_available_bit(flags.available_bit() as u32);
    access.set_long_mode(flags.long_mode() as u32);
    access.set_default_big(flags.default_big() as u32);
    access.set_granularity(flags.granularity() as u32);

    access
}
