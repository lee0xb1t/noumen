//! EPT paging-structure entries and the EPT pointer.
//!
//! See Intel SDM Vol3C, 28.2.3.2 "EPT Page-Table Entries" and 24.6.11
//! "Extended-Page-Table Pointer (EPTP)".

// Complete EPT entry accessor surface.
#![allow(dead_code)]

use crate::arch::x86_64::paging::MemoryType;

bitfield::bitfield! {
    /// EPT PML4 entry, references an EPT PDPT.
    #[repr(transparent)]
    #[derive(Clone, Copy, Debug, Default)]
    pub struct EptPml4e(u64);

    pub read, set_read: 0;
    pub write, set_write: 1;
    pub execute, set_execute: 2;
    pub _, _: 7, 3;
    pub accessed, set_accessed: 8;
    pub _, _: 9;
    pub user_mode_execute, set_user_mode_execute: 10;
    pub _, _: 11;
    pub pfn, set_pfn: 47, 12;
    pub _, _: 63, 48;
}

bitfield::bitfield! {
    /// EPT PDPT entry, references an EPT PD.
    #[repr(transparent)]
    #[derive(Clone, Copy, Debug, Default)]
    pub struct EptPdpte(u64);

    pub read, set_read: 0;
    pub write, set_write: 1;
    pub execute, set_execute: 2;
    pub _, _: 7, 3;
    pub accessed, set_accessed: 8;
    pub _, _: 9;
    pub user_mode_execute, set_user_mode_execute: 10;
    pub _, _: 11;
    pub pfn, set_pfn: 47, 12;
    pub _, _: 63, 48;
}

bitfield::bitfield! {
    /// EPT PD entry, references an EPT PT.
    #[repr(transparent)]
    #[derive(Clone, Copy, Debug, Default)]
    pub struct EptPde(u64);

    pub read, set_read: 0;
    pub write, set_write: 1;
    pub execute, set_execute: 2;
    pub _, _: 7, 3;
    pub accessed, set_accessed: 8;
    pub _, _: 9;
    pub user_mode_execute, set_user_mode_execute: 10;
    pub _, _: 11;
    pub pfn, set_pfn: 47, 12;
    pub _, _: 63, 48;
}

bitfield::bitfield! {
    /// EPT PD entry that maps a 2 MiB page.
    #[repr(transparent)]
    #[derive(Clone, Copy, Debug, Default)]
    pub struct EptPde2mb(u64);

    pub read, set_read: 0;
    pub write, set_write: 1;
    pub execute, set_execute: 2;
    pub u8, memory_type, set_memory_type: 5, 3;
    pub ignore_pat, set_ignore_pat: 6;
    pub large_page, set_large_page: 7;
    pub accessed, set_accessed: 8;
    pub dirty, set_dirty: 9;
    pub user_mode_execute, set_user_mode_execute: 10;
    pub _, _: 20, 11;
    pub pfn, set_pfn: 47, 21;
    pub _, _: 56, 48;
    pub verify_guest_paging, set_verify_guest_paging: 57;
    pub paging_write_access, set_paging_write_access: 58;
    pub _, _: 59;
    pub supervisor_shadow_stack, set_supervisor_shadow_stack: 60;
    pub _, _: 62, 61;
    pub suppress_ve, set_suppress_ve: 63;
}

bitfield::bitfield! {
    /// EPT PT entry, maps a 4 KiB page.
    #[repr(transparent)]
    #[derive(Clone, Copy, Debug, Default)]
    pub struct EptPte(u64);

    pub read, set_read: 0;
    pub write, set_write: 1;
    pub execute, set_execute: 2;
    pub u8, memory_type, set_memory_type: 5, 3;
    pub ignore_pat, set_ignore_pat: 6;
    pub _, _: 7;
    pub accessed, set_accessed: 8;
    pub dirty, set_dirty: 9;
    pub user_mode_execute, set_user_mode_execute: 10;
    pub _, _: 11;
    pub pfn, set_pfn: 47, 12;
    pub _, _: 56, 48;
    pub verify_guest_paging, set_verify_guest_paging: 57;
    pub paging_write_access, set_paging_write_access: 58;
    pub _, _: 59;
    pub supervisor_shadow_stack, set_supervisor_shadow_stack: 60;
    pub sub_page_write_permissions, set_sub_page_write_permissions: 61;
    pub _, _: 62;
    pub suppress_ve, set_suppress_ve: 63;
}

bitfield::bitfield! {
    /// Extended-Page-Table Pointer (EPTP).
    #[repr(transparent)]
    #[derive(Clone, Copy, Debug, Default)]
    pub struct Eptp(u64);

    pub u8, memory_type, set_memory_type: 2, 0;
    pub page_walk_length, set_page_walk_length: 5, 3;
    pub enable_access_and_dirty_flags, set_enable_access_and_dirty_flags: 6;
    pub enable_supervisor_shadow_stack_pages, set_enable_supervisor_shadow_stack_pages: 7;
    pub _, _: 11, 8;
    pub pfn, set_pfn: 47, 12;
    pub _, _: 63, 48;
}

macro_rules! impl_raw {
    ($($entry:ident),* $(,)?) => {
        $(
            impl $entry {
                pub const fn new() -> Self {
                    Self(0)
                }

                pub const fn from_raw(raw: u64) -> Self {
                    Self(raw)
                }

                pub const fn as_raw(&self) -> u64 {
                    self.0
                }
            }
        )*
    };
}

impl_raw!(EptPml4e, EptPdpte, EptPde, EptPde2mb, EptPte, Eptp);

impl EptPde2mb {
    /// Reinterprets a non-large entry as a PDE that points to an EPT PT.
    pub const fn as_table(&self) -> EptPde {
        EptPde::from_raw(self.as_raw())
    }
}

impl Eptp {
    /// 4-level EPT: the stored value is one less than the page-walk length.
    pub const PAGE_WALK_LENGTH_4: u64 = 3;

    pub const fn from_pfn(pfn: u64) -> Self {
        Self((pfn << 12) | (Self::PAGE_WALK_LENGTH_4 << 3) | MemoryType::WriteBack as u64)
    }
}
