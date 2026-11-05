//! Nested Page Table entries.

use core::mem::size_of;

bitfield::bitfield! {
    /// One NPT paging-structure entry (PML4E/PDPTE/PDE/PTE share the layout).
    #[repr(transparent)]
    #[derive(Clone, Copy, Default, PartialEq, Eq)]
    pub struct NptEntry(u64);

    pub present, set_present: 0;
    pub write, set_write: 1;
    /// Ignored by NPT permission checks; keep 1 (nested-VMM compatibility).
    pub user, set_user: 2;
    pub pwt, set_pwt: 3;
    pub pcd, set_pcd: 4;
    pub accessed, set_accessed: 5;
    pub dirty, set_dirty: 6;
    /// PS: this entry maps a large page (2 MiB at PDE, 1 GiB at PDPTE).
    pub large_page, set_large_page: 7;
    pub global, set_global: 8;
    /// PAT bit 12 of a 4 KiB entry / PAT of a large entry.
    pub pat, set_pat: 12;
    pub nx, set_nx: 63;
}

// Complete NPT entry surface, including 1 GiB helpers.
#[allow(dead_code)]
impl NptEntry {
    /// 4 KiB page frame mask (bits 51:12).
    pub const PFN_4K_MASK: u64 = 0x000f_ffff_ffff_f000;
    /// 2 MiB page frame mask (bits 51:21).
    pub const PFN_2M_MASK: u64 = 0x000f_ffff_ffe0_0000;
    /// 1 GiB page frame mask (bits 51:30).
    pub const PFN_1G_MASK: u64 = 0x000f_ffff_c000_0000;
    /// Largest page frame number representable in the 52-bit physical address
    /// field: bits 51:12 are the PFN, i.e. 40 bits.
    pub const PFN_MAX: u64 = 0x0000_00ff_ffff_ffff;

    pub const fn new() -> Self {
        Self(0)
    }

    pub const fn from_raw(raw: u64) -> Self {
        Self(raw)
    }

    pub const fn as_raw(&self) -> u64 {
        self.0
    }

    pub fn pfn_4k(&self) -> u64 {
        (self.0 & Self::PFN_4K_MASK) >> 12
    }

    pub fn set_pfn_4k(&mut self, pfn: u64) {
        self.0 = (self.0 & !Self::PFN_4K_MASK) | ((pfn & Self::PFN_MAX) << 12);
    }

    pub fn pfn_2m(&self) -> u64 {
        (self.0 & Self::PFN_2M_MASK) >> 21
    }

    pub fn set_pfn_2m(&mut self, pfn: u64) {
        self.0 = (self.0 & !Self::PFN_2M_MASK) | ((pfn & Self::PFN_MAX) << 21);
    }

    pub fn pfn_1g(&self) -> u64 {
        (self.0 & Self::PFN_1G_MASK) >> 30
    }

    pub fn set_pfn_1g(&mut self, pfn: u64) {
        self.0 = (self.0 & !Self::PFN_1G_MASK) | ((pfn & Self::PFN_MAX) << 30);
    }

    /// A leaf that maps `phys` at 4 KiB granularity, preserving nothing.
    pub fn leaf_4k(phys: u64, write: bool, nx: bool) -> Self {
        let mut entry = Self::new();
        entry.set_present(true);
        entry.set_write(write);
        entry.set_user(true);
        entry.set_pfn_4k(phys >> 12);
        entry.set_nx(nx);
        entry
    }

    /// A 2 MiB leaf. NPT has no execute bit; NX controls execution.
    pub fn leaf_2m(phys: u64, write: bool, nx: bool) -> Self {
        let mut entry = Self::new();
        entry.set_present(true);
        entry.set_write(write);
        entry.set_user(true);
        entry.set_large_page(true);
        entry.set_pfn_2m(phys >> 21);
        entry.set_nx(nx);
        entry
    }

    /// A 1 GiB leaf (PDPTE with PS=1).
    pub fn leaf_1g(phys: u64, write: bool, nx: bool) -> Self {
        let mut entry = Self::new();
        entry.set_present(true);
        entry.set_write(write);
        entry.set_user(true);
        entry.set_large_page(true);
        entry.set_pfn_1g(phys >> 30);
        entry.set_nx(nx);
        entry
    }

    /// A non-leaf entry pointing at `table_pa`.
    pub fn table(table_pa: u64) -> Self {
        let mut entry = Self::new();
        entry.set_present(true);
        entry.set_write(true);
        entry.set_user(true);
        entry.set_pfn_4k(table_pa >> 12);
        entry
    }
}

const _: () = assert!(size_of::<NptEntry>() == 8);

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn leaf_2m_sets_ps_and_the_right_pfn() {
        let entry = NptEntry::leaf_2m(0x4000_0000, true, false);

        assert!(entry.present());
        assert!(entry.write());
        assert!(entry.user());
        assert!(entry.large_page());
        assert!(!entry.nx());
        assert_eq!(entry.pfn_2m(), 0x4000_0000 >> 21);
        assert_eq!(entry.pfn_2m() << 21, 0x4000_0000);
    }

    #[test]
    fn leaf_4k_keeps_pfn_and_nx() {
        let entry = NptEntry::leaf_4k(0x1234_5000, false, true);

        assert!(entry.present());
        assert!(!entry.write());
        assert!(!entry.large_page());
        assert!(entry.nx());
        assert_eq!(entry.pfn_4k() << 12, 0x1234_5000);
    }

    #[test]
    fn table_entry_points_at_the_next_level() {
        let entry = NptEntry::table(0x2000);

        assert!(entry.present());
        assert!(!entry.large_page());
        assert_eq!(entry.pfn_4k(), 2);
    }

    #[test]
    fn pfn_setters_preserve_flags() {
        let mut entry = NptEntry::leaf_2m(0x200000, true, false);
        entry.set_pfn_2m(0xdead);

        assert!(entry.present());
        assert!(entry.large_page());
        assert_eq!(entry.pfn_2m(), 0xdead);
    }

    #[test]
    fn one_gigabyte_pfn_is_bits_51_30() {
        let mut entry = NptEntry::new();
        entry.set_pfn_1g(0x1_0000_0000 >> 30);

        assert_eq!(entry.pfn_1g(), 4);
        assert_eq!(entry.as_raw() & NptEntry::PFN_1G_MASK, 0x1_0000_0000);
    }
}
