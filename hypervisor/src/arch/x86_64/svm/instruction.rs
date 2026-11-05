//! Instruction-level helpers: advancing the guest RIP after a trap-like exit.

use super::vmcb::Vmcb;

/// Advance RIP for a trap-like exit, preferring `next_rip`.
pub fn advance_rip(vmcb: &mut Vmcb, default_len: u64, nrips: bool) {
    let next = if nrips && vmcb.control.next_rip != 0 {
        vmcb.control.next_rip
    } else {
        vmcb.save.rip.wrapping_add(default_len)
    };

    vmcb.save.rip = next;
}

/// Use an exit-specific next RIP (e.g. IOIO's `exit_info_2`), falling back to
/// `default_len` when the field is zero.
#[allow(dead_code)] // retained for exit paths that use an explicit next RIP
pub fn advance_to(vmcb: &mut Vmcb, next_rip: u64, default_len: u64) {
    vmcb.save.rip = if next_rip != 0 {
        next_rip
    } else {
        vmcb.save.rip.wrapping_add(default_len)
    };
}

#[cfg(test)]
mod tests {
    use super::*;

    fn zeroed() -> Vmcb {
        unsafe { core::mem::zeroed() }
    }

    #[test]
    fn prefers_next_rip_when_available() {
        let mut vmcb = zeroed();
        vmcb.save.rip = 0x1000;
        vmcb.control.next_rip = 0x1002;

        advance_rip(&mut vmcb, 2, true);

        assert_eq!(vmcb.save.rip, 0x1002);
    }

    #[test]
    fn ignores_next_rip_without_nrips() {
        let mut vmcb = zeroed();
        vmcb.save.rip = 0x1000;
        vmcb.control.next_rip = 0x1002;

        advance_rip(&mut vmcb, 3, false);

        assert_eq!(vmcb.save.rip, 0x1003);
    }

    #[test]
    fn falls_back_when_next_rip_is_zero() {
        let mut vmcb = zeroed();
        vmcb.save.rip = 0x2000;

        advance_rip(&mut vmcb, 2, true);

        assert_eq!(vmcb.save.rip, 0x2002);
    }

    #[test]
    fn explicit_next_rip_wins_over_default_length() {
        let mut vmcb = zeroed();
        vmcb.save.rip = 0x3000;

        advance_to(&mut vmcb, 0x3010, 2);
        assert_eq!(vmcb.save.rip, 0x3010);

        let mut vmcb = zeroed();
        vmcb.save.rip = 0x3000;
        advance_to(&mut vmcb, 0, 2);
        assert_eq!(vmcb.save.rip, 0x3002);
    }
}
