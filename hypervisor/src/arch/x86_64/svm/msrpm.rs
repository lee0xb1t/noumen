//! MSR permission map (MSRPM).

/// Total size of the map: two 4 KiB pages.
pub const MSRPM_BYTES: usize = 8192;
/// Each range is 2 KiB.
pub const RANGE_BYTES: usize = 2048;
/// Read + write bits per MSR.
pub const BITS_PER_MSR: usize = 2;
/// MSRs covered by one range.
pub const MSRS_PER_RANGE: u32 = 0x2000;

/// Which bit of an MSR's pair to touch.
#[repr(u8)]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum MsrAccess {
    Read = 0,
    Write = 1,
}

#[inline]
const fn range_base(msr: u32) -> Option<usize> {
    match msr & !(MSRS_PER_RANGE - 1) {
        0x0000_0000 => Some(0),
        0xc000_0000 => Some(1),
        0xc001_0000 => Some(2),
        _ => None,
    }
}

/// Bit position of `msr`'s read bit inside the map. `None` for MSRs outside
/// the three defined ranges (those are always intercepted).
pub fn bit_position(msr: u32) -> Option<usize> {
    let range = range_base(msr)?;
    let index = (msr & (MSRS_PER_RANGE - 1)) as usize;

    Some(range * RANGE_BYTES * 8 + index * BITS_PER_MSR)
}

#[inline]
fn byte_bit(bit: usize) -> (usize, u8) {
    (bit / 8, 1u8 << (bit % 8))
}

/// Set the read or write intercept bit for `msr`.
///
/// Returns `false` when the MSR is outside the map: it is already intercepted
/// unconditionally, so there is nothing to set.
pub fn set(map: &mut [u8], msr: u32, access: MsrAccess) -> bool {
    let Some(bit) = bit_position(msr) else {
        return false;
    };

    let (byte, mask) = byte_bit(bit + access as usize);
    map[byte] |= mask;

    true
}

/// Clear the read or write intercept bit for `msr`.
///
/// Returns `false` when the MSR is outside the map: those cannot be
/// passed through.
#[allow(dead_code)] // MSRPM API used by host tests and future interception paths
pub fn clear(map: &mut [u8], msr: u32, access: MsrAccess) -> bool {
    let Some(bit) = bit_position(msr) else {
        return false;
    };

    let (byte, mask) = byte_bit(bit + access as usize);
    map[byte] &= !mask;

    true
}

/// Whether the intercept bit is set. MSRs outside the map are reported as
/// intercepted (they have no bit to clear), so this returns `true`.
#[allow(dead_code)] // MSRPM API used by host tests and future interception paths
pub fn is_set(map: &[u8], msr: u32, access: MsrAccess) -> bool {
    let Some(bit) = bit_position(msr) else {
        return true;
    };

    let (byte, mask) = byte_bit(bit + access as usize);

    map[byte] & mask != 0
}

/// An 8 KiB map. Use `memory::alloc_pages::<MsrpmPage>(2)`; the page allocator
/// returns contiguous pages, which is what the hardware requires.
#[repr(C, align(4096))]
pub struct MsrpmPage(pub [u8; MSRPM_BYTES]);

impl MsrpmPage {
    /// All zero: every mapped MSR is allowed; `intercept` sets bits.
    pub const fn new() -> Self {
        Self([0; MSRPM_BYTES])
    }

    pub fn set(&mut self, msr: u32, access: MsrAccess) -> bool {
        set(&mut self.0, msr, access)
    }

    #[allow(dead_code)] // MSRPM API used by host tests and future interception paths
    pub fn clear(&mut self, msr: u32, access: MsrAccess) -> bool {
        clear(&mut self.0, msr, access)
    }

    #[allow(dead_code)] // MSRPM API used by host tests and future interception paths
    pub fn is_set(&self, msr: u32, access: MsrAccess) -> bool {
        is_set(&self.0, msr, access)
    }

    /// Intercept both directions of `msr` (set both bits).
    pub fn intercept(&mut self, msr: u32) {
        self.set(msr, MsrAccess::Read);
        self.set(msr, MsrAccess::Write);
    }

    /// Allow both directions of `msr` (clear both bits).
    #[allow(dead_code)] // MSRPM API used by host tests and future interception paths
    pub fn pass_through(&mut self, msr: u32) {
        self.clear(msr, MsrAccess::Read);
        self.clear(msr, MsrAccess::Write);
    }
}

impl Default for MsrpmPage {
    fn default() -> Self {
        Self::new()
    }
}

const _: () = assert!(MSRPM_BYTES == 2 * 4096);
const _: () = assert!((MSRS_PER_RANGE as usize) * 2 / 8 == RANGE_BYTES);

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn range_boundaries() {
        assert_eq!(bit_position(0x0000_0000), Some(0));
        assert_eq!(bit_position(0x0000_1fff), Some(0x3ffe));
        assert_eq!(bit_position(0xc000_0000), Some(0x4000));
        assert_eq!(bit_position(0xc000_1fff), Some(0x7ffe));
        assert_eq!(bit_position(0xc001_0000), Some(0x8000));
        assert_eq!(bit_position(0xc001_1fff), Some(0xbffe));
    }

    #[test]
    fn out_of_range_has_no_bit() {
        assert_eq!(bit_position(0x0000_2000), None);
        assert_eq!(bit_position(0xc002_0000), None);
        assert_eq!(bit_position(0x4000_0000), None);

        // No bit exists, so hardware reports these as intercepted.
        let map = MsrpmPage::new();
        assert!(map.is_set(0x4000_0000, MsrAccess::Read));
    }

    #[test]
    fn default_allows_everything() {
        let map = MsrpmPage::new();

        assert!(map.0.iter().all(|&byte| byte == 0));
        assert!(!map.is_set(0xc000_0080, MsrAccess::Read));
    }

    #[test]
    fn read_and_write_bits_are_adjacent() {
        let mut map = MsrpmPage::new();

        // MSR 0x1a -> bit 52 -> byte 6, read=bit 4, write=bit 5.
        map.intercept(0x1a);
        assert_eq!(map.0[6], (1 << 4) | (1 << 5));
        assert!(map.is_set(0x1a, MsrAccess::Read));
        assert!(map.is_set(0x1a, MsrAccess::Write));

        map.pass_through(0x1a);
        assert_eq!(map.0[6], 0);
    }

    #[test]
    fn set_clear_round_trip() {
        let mut map = MsrpmPage::new();
        let msr = 0xc000_0080; // EFER

        assert!(!map.is_set(msr, MsrAccess::Read));

        map.intercept(msr);
        assert!(map.is_set(msr, MsrAccess::Read));
        assert!(map.is_set(msr, MsrAccess::Write));

        map.pass_through(msr);
        assert!(!map.is_set(msr, MsrAccess::Read));
        assert!(!map.is_set(msr, MsrAccess::Write));
    }

    #[test]
    fn outside_the_map_cannot_be_passed_through() {
        let mut map = MsrpmPage::new();

        assert!(!map.clear(0x4000_0000, MsrAccess::Read));
        assert!(!map.clear(0x0000_2000, MsrAccess::Write));
    }

    #[test]
    fn efer_lands_at_the_documented_bit() {
        // EFER 0xC000_0080: range 1, index 0x80 -> bit 0x4000 + 0x100.
        assert_eq!(bit_position(0xc000_0080), Some(0x4100));
    }
}
