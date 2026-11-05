//! VMCB clean bits.

/// One cache group. The discriminants are the bit positions in
/// [`CleanBits`] (and in the VMCB at offset 0x0C0).
#[repr(u32)]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CleanBit {
    /// All six intercept words, `tsc_offset`, pause-filter counters.
    Intercepts = 0,
    /// `iopm_base_pa`, `msrpm_base_pa`.
    PermMaps = 1,
    /// `asid`, `tlb_control`.
    Asid = 2,
    /// `int_ctl`, `int_vector` (always dirty; see [`CleanBits::ALWAYS_DIRTY`]).
    Intr = 3,
    /// `nested_ctl`, `nested_cr3`, `g_pat`.
    Npt = 4,
    /// `cr0`, `cr3`, `cr4`, `efer`.
    Cr = 5,
    /// `dr6`, `dr7`.
    Dr = 6,
    /// `gdtr`, `idtr`.
    Dt = 7,
    /// The ten segment fields and `cpl`.
    Seg = 8,
    /// `cr2` (always dirty).
    Cr2 = 9,
    /// `dbgctl`, branch-record fields.
    Lbr = 10,
    /// AVIC pointers.
    Avic = 11,
    /// CET fields.
    Cet = 12,
    /// Software-owned.
    Sw = 31,
}

bitfield::bitfield! {
    /// The `clean` word (VMCB offset 0x0C0). A set bit means "may reuse the
    /// cached copy"; a clear bit means "re-read".
    #[repr(transparent)]
    #[derive(Clone, Copy, Default, PartialEq, Eq)]
    pub struct CleanBits(u32);

    pub intercepts, set_intercepts: 0;
    pub perm_maps, set_perm_maps: 1;
    pub asid, set_asid: 2;
    pub intr, set_intr: 3;
    pub npt, set_npt: 4;
    pub cr, set_cr: 5;
    pub dr, set_dr: 6;
    pub dt, set_dt: 7;
    pub seg, set_seg: 8;
    pub cr2, set_cr2: 9;
    pub lbr, set_lbr: 10;
    pub avic, set_avic: 11;
    pub cet, set_cet: 12;
    pub sw, set_sw: 31;
}

impl CleanBits {
    /// All defined cache groups (bits 0-12 and 31).
    pub const ALL: u32 = 0x1fff | (1 << 31);

    /// Groups that must never be marked clean: the TPR (`int_ctl`) is rewritten
    /// before every VMRUN, and CR2 changes whenever a #PF is injected.
    pub const ALWAYS_DIRTY: u32 = (1 << CleanBit::Intr as u32) | (1 << CleanBit::Cr2 as u32);

    /// Mark a group as "must re-read".
    #[inline]
    pub fn mark_dirty(&mut self, bit: CleanBit) {
        self.0 &= !(1 << bit as u32);
    }

    /// Mark every group clean except the always-dirty ones.
    #[inline]
    pub fn mark_all_clean(&mut self) {
        self.0 = Self::ALL & !Self::ALWAYS_DIRTY;
    }

    /// Whether the group may reuse its cached copy.
    #[inline]
    pub fn is_clean(&self, bit: CleanBit) -> bool {
        self.0 & (1 << bit as u32) != 0
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn dirty_starts_dirty() {
        let bits = CleanBits::default();
        assert!(!bits.is_clean(CleanBit::Cr));
        assert!(!bits.is_clean(CleanBit::Intercepts));
    }

    #[test]
    fn mark_and_clear_one_group() {
        let mut bits = CleanBits::default();
        bits.mark_all_clean();
        assert!(bits.is_clean(CleanBit::Cr));

        bits.mark_dirty(CleanBit::Cr);
        assert!(!bits.is_clean(CleanBit::Cr));
        // Neighbours are untouched.
        assert!(bits.is_clean(CleanBit::Dr));
        assert!(bits.is_clean(CleanBit::Npt));
    }

    #[test]
    fn always_dirty_groups_stay_dirty() {
        let mut bits = CleanBits::default();
        bits.mark_all_clean();
        assert!(!bits.is_clean(CleanBit::Intr));
        assert!(!bits.is_clean(CleanBit::Cr2));
    }

    #[test]
    fn all_covers_every_defined_group() {
        for bit in [
            CleanBit::Intercepts,
            CleanBit::PermMaps,
            CleanBit::Asid,
            CleanBit::Intr,
            CleanBit::Npt,
            CleanBit::Cr,
            CleanBit::Dr,
            CleanBit::Dt,
            CleanBit::Seg,
            CleanBit::Cr2,
            CleanBit::Lbr,
            CleanBit::Avic,
            CleanBit::Cet,
            CleanBit::Sw,
        ] {
            assert!(CleanBits::ALL & (1 << bit as u32) != 0, "{bit:?} missing");
        }
    }
}
