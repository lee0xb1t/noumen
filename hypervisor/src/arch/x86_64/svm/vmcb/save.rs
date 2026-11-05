//! VMCB state-save area (offset 0x400 in the VMCB).

use core::mem::{offset_of, size_of};

/// One segment register as stored in the VMCB.
///
/// `attrib` is the **16-bit** descriptor attribute, not the 32-bit "access
/// rights" layout used by VMX. An unusable segment is represented by `P=0`.
#[repr(C)]
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct VmcbSeg {
    pub selector: u16,
    pub attrib: u16,
    pub limit: u32,
    pub base: u64,
}

impl VmcbSeg {
    pub const fn new(selector: u16, attrib: u16, limit: u32, base: u64) -> Self {
        Self {
            selector,
            attrib,
            limit,
            base,
        }
    }
}

const _: () = assert!(size_of::<VmcbSeg>() == 16);
const _: () = assert!(offset_of!(VmcbSeg, selector) == 0x0);
const _: () = assert!(offset_of!(VmcbSeg, attrib) == 0x2);
const _: () = assert!(offset_of!(VmcbSeg, limit) == 0x4);
const _: () = assert!(offset_of!(VmcbSeg, base) == 0x8);

/// The state-save area. Offsets are relative to the start of the area; add
/// `0x400` for the VMCB-relative offset.
#[repr(C)]
#[derive(Clone, Copy)]
pub struct StateSaveArea {
    pub es: VmcbSeg,   // 0x000
    pub cs: VmcbSeg,   // 0x010
    pub ss: VmcbSeg,   // 0x020
    pub ds: VmcbSeg,   // 0x030
    pub fs: VmcbSeg,   // 0x040
    pub gs: VmcbSeg,   // 0x050
    pub gdtr: VmcbSeg, // 0x060
    pub ldtr: VmcbSeg, // 0x070
    pub idtr: VmcbSeg, // 0x080
    pub tr: VmcbSeg,   // 0x090

    reserved0: [u8; 42], // 0x0A0
    /// SEV VMPL; write 0 on non-SEV.
    pub vmpl: u8, // 0x0CA
    /// Current privilege level; must agree with `SS.DPL`.
    pub cpl: u8,  // 0x0CB
    reserved1: [u8; 4], // 0x0CC

    pub efer: u64,          // 0x0D0
    reserved2: [u8; 112],   // 0x0D8
    pub cr4: u64,           // 0x148
    pub cr3: u64,           // 0x150
    pub cr0: u64,           // 0x158
    pub dr7: u64,           // 0x160
    pub dr6: u64,           // 0x168
    pub rflags: u64,        // 0x170
    pub rip: u64,           // 0x178
    reserved3: [u8; 88],    // 0x180
    pub rsp: u64,           // 0x1D8
    pub s_cet: u64,         // 0x1E0
    pub ssp: u64,           // 0x1E8
    pub isst_addr: u64,     // 0x1F0
    pub rax: u64,           // 0x1F8

    pub star: u64,          // 0x200
    pub lstar: u64,         // 0x208
    pub cstar: u64,         // 0x210
    pub sfmask: u64,        // 0x218
    pub kernel_gs_base: u64, // 0x220
    pub sysenter_cs: u64,   // 0x228
    pub sysenter_esp: u64,  // 0x230
    pub sysenter_eip: u64,  // 0x238
    pub cr2: u64,           // 0x240
    reserved4: [u8; 32],    // 0x248

    pub g_pat: u64,         // 0x268
    pub dbgctl: u64,        // 0x270
    pub br_from: u64,       // 0x278
    pub br_to: u64,         // 0x280
    pub last_excp_from: u64, // 0x288
    pub last_excp_to: u64,  // 0x290
    reserved5: [u8; 72],    // 0x298
    pub spec_ctrl: u64,     // 0x2E0
}

const _: () = assert!(size_of::<StateSaveArea>() == 0x2e8);

const _: () = assert!(offset_of!(StateSaveArea, cs) == 0x010);
const _: () = assert!(offset_of!(StateSaveArea, ss) == 0x020);
const _: () = assert!(offset_of!(StateSaveArea, ds) == 0x030);
const _: () = assert!(offset_of!(StateSaveArea, fs) == 0x040);
const _: () = assert!(offset_of!(StateSaveArea, gs) == 0x050);
const _: () = assert!(offset_of!(StateSaveArea, gdtr) == 0x060);
const _: () = assert!(offset_of!(StateSaveArea, ldtr) == 0x070);
const _: () = assert!(offset_of!(StateSaveArea, idtr) == 0x080);
const _: () = assert!(offset_of!(StateSaveArea, tr) == 0x090);
const _: () = assert!(offset_of!(StateSaveArea, vmpl) == 0x0ca);
const _: () = assert!(offset_of!(StateSaveArea, cpl) == 0x0cb);
const _: () = assert!(offset_of!(StateSaveArea, efer) == 0x0d0);
const _: () = assert!(offset_of!(StateSaveArea, cr4) == 0x148);
const _: () = assert!(offset_of!(StateSaveArea, cr3) == 0x150);
const _: () = assert!(offset_of!(StateSaveArea, cr0) == 0x158);
const _: () = assert!(offset_of!(StateSaveArea, dr7) == 0x160);
const _: () = assert!(offset_of!(StateSaveArea, dr6) == 0x168);
const _: () = assert!(offset_of!(StateSaveArea, rflags) == 0x170);
const _: () = assert!(offset_of!(StateSaveArea, rip) == 0x178);
const _: () = assert!(offset_of!(StateSaveArea, rsp) == 0x1d8);
const _: () = assert!(offset_of!(StateSaveArea, rax) == 0x1f8);
const _: () = assert!(offset_of!(StateSaveArea, star) == 0x200);
const _: () = assert!(offset_of!(StateSaveArea, kernel_gs_base) == 0x220);
const _: () = assert!(offset_of!(StateSaveArea, cr2) == 0x240);
const _: () = assert!(offset_of!(StateSaveArea, g_pat) == 0x268);
const _: () = assert!(offset_of!(StateSaveArea, dbgctl) == 0x270);
const _: () = assert!(offset_of!(StateSaveArea, spec_ctrl) == 0x2e0);

impl Default for StateSaveArea {
    fn default() -> Self {
        // The page allocator zeroes memory; this only exists so the type can be
        // used in const-ish contexts and tests.
        unsafe { core::mem::zeroed() }
    }
}

/// Convert a VMX-style 32-bit segment "access rights" value into the 16-bit
/// descriptor attribute that the VMCB stores.
///
/// VMX lays the bits out as `type/S/DPL/P/AVL/L/DB/G/unusable` at 3:0, 4, 6:5,
/// 7, 12, 13, 14, 15, 16; AMD's `attrib` is the architectural descriptor
/// attribute byte pair: `type/S/DPL/P/AVL/L/DB/G` at 3:0, 4, 6:5, 7, 8, 9, 10,
/// 11. An unusable segment is expressed by `P=0`; there is no unusable bit.
pub fn attrib_from_access_rights(access_rights: u32) -> u16 {
    let mut attrib = (access_rights & 0x00ff) as u16; // type, S, DPL, P

    if access_rights & (1 << 16) != 0 {
        // Unusable: clear P and keep everything else zero.
        attrib &= !(1 << 7);
        return attrib;
    }

    if access_rights & (1 << 12) != 0 {
        attrib |= 1 << 8; // AVL
    }
    if access_rights & (1 << 13) != 0 {
        attrib |= 1 << 9; // L
    }
    if access_rights & (1 << 14) != 0 {
        attrib |= 1 << 10; // D/B
    }
    if access_rights & (1 << 15) != 0 {
        attrib |= 1 << 11; // G
    }

    attrib
}

/// Inverse of [`attrib_from_access_rights`]. `unusable` must be supplied by the
/// caller: AMD has no bit for it, and `P=0` alone is ambiguous.
pub fn access_rights_from_attrib(attrib: u16, unusable: bool) -> u32 {
    let mut access_rights = (attrib & 0x00ff) as u32;

    if attrib & (1 << 8) != 0 {
        access_rights |= 1 << 12;
    }
    if attrib & (1 << 9) != 0 {
        access_rights |= 1 << 13;
    }
    if attrib & (1 << 10) != 0 {
        access_rights |= 1 << 14;
    }
    if attrib & (1 << 11) != 0 {
        access_rights |= 1 << 15;
    }
    if unusable {
        access_rights |= 1 << 16;
    }

    access_rights
}

#[cfg(test)]
mod attribute_tests {
    use super::*;

    #[test]
    fn kernel_code_maps_l_and_g() {
        // type=0xB, S=1, DPL=0, P=1, L=1, G=1.
        assert_eq!(attrib_from_access_rights(0x00a09b), 0x0a9b);
    }

    #[test]
    fn kernel_data_maps_db_and_g() {
        // type=3, S=1, DPL=0, P=1, DB=1, G=1.
        assert_eq!(attrib_from_access_rights(0x00c093), 0x0c93);
    }

    #[test]
    fn unusable_is_p_equals_zero() {
        assert_eq!(attrib_from_access_rights(0x10000), 0);
        assert_eq!(attrib_from_access_rights(0x1_00f3), 0x73);
    }

    #[test]
    fn round_trip_for_usable_segments() {
        for access_rights in [0x00a09b, 0x00c093, 0x000092, 0x0000f3] {
            let attrib = attrib_from_access_rights(access_rights);
            assert_eq!(access_rights_from_attrib(attrib, false), access_rights);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn segment_layout_matches_the_architectural_offsets() {
        let seg = VmcbSeg::new(0x33, 0x0a, 0xffff, 0x1_0000);
        assert_eq!(seg.selector, 0x33);
        assert_eq!(seg.attrib, 0x0a);
        assert_eq!(seg.limit, 0xffff);
        assert_eq!(seg.base, 0x1_0000);
    }

    #[test]
    fn save_area_is_744_bytes() {
        assert_eq!(size_of::<StateSaveArea>(), 744);
    }
}
