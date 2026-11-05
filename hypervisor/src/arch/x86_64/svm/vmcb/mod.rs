//! The Virtual Machine Control Block (VMCB).

// The VMCB is a complete hardware ABI surface; keep the full typed field set.
#![allow(dead_code)]

pub mod access;
pub mod clean;
pub mod control;
pub mod save;

use core::mem::{offset_of, size_of};

pub const VMCB_SIZE: usize = 0x1000;
pub const CONTROL_SIZE: usize = 0x400;
pub const SAVE_OFFSET: usize = 0x400;

#[repr(C, align(4096))]
pub struct Vmcb {
    pub control: control::ControlArea,
    pub save: save::StateSaveArea,
    /// 0x6E8..0x1000 is architecturally reserved; spelled out so the size and
    /// offset assertions below stay honest.
    reserved: [u8; VMCB_SIZE - CONTROL_SIZE - size_of::<save::StateSaveArea>()],
}

const _: () = assert!(size_of::<control::ControlArea>() == CONTROL_SIZE);
const _: () = assert!(size_of::<save::StateSaveArea>() == 0x2e8);
const _: () = assert!(size_of::<Vmcb>() == VMCB_SIZE);
const _: () = assert!(offset_of!(Vmcb, control) == 0);
const _: () = assert!(offset_of!(Vmcb, save) == SAVE_OFFSET);

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn vmcb_is_one_page_with_the_save_area_at_0x400() {
        assert_eq!(size_of::<Vmcb>(), 0x1000);
        assert_eq!(offset_of!(Vmcb, save), 0x400);
    }

    #[test]
    fn reserved_tail_starts_after_spec_ctrl() {
        assert_eq!(offset_of!(Vmcb, reserved), 0x6e8);
    }
}
