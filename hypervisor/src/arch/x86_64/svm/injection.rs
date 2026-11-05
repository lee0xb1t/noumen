//! Event injection helpers.

use super::vmcb::{Vmcb, control::SvmEventType};

/// Inject a hardware exception. RIP is left unchanged (fault semantics).
pub fn inject_exception(vmcb: &mut Vmcb, vector: u8, error_code: Option<u32>) {
    vmcb.control
        .inject_event(vector, SvmEventType::Exception, error_code);
}

/// Inject `#UD`.
pub fn inject_ud(vmcb: &mut Vmcb) {
    inject_exception(vmcb, 6, None);
}

/// Inject `#GP(code)`.
pub fn inject_gp(vmcb: &mut Vmcb, code: u32) {
    inject_exception(vmcb, 13, Some(code));
}

/// Inject a software exception (#BP/#OF/#INT n).
///
/// `next_rip` must point past the instruction: the guest RIP still points at
/// `INT3`/`INTO` when the exit is taken, and without `next_rip` the handler
/// would return to the same instruction forever.
#[allow(dead_code)] // SVM event-injection API, retained for future exit paths
pub fn inject_software_exception(
    vmcb: &mut Vmcb,
    vector: u8,
    error_code: Option<u32>,
    next_rip: u64,
    nrips: bool,
) {
    if nrips {
        vmcb.control.next_rip = next_rip;
    } else {
        // No NRIPS: the only way to make hardware advance RIP is to move it
        // before the injection.
        vmcb.save.rip = next_rip;
    }

    vmcb.control
        .inject_event(vector, SvmEventType::Exception, error_code);
}

/// Inject an NMI (type 2, vector field ignored).
#[allow(dead_code)] // SVM NMI-injection API, retained for future exit paths
pub fn inject_nmi(vmcb: &mut Vmcb) {
    vmcb.control.inject_event(0, SvmEventType::Nmi, None);
}

#[cfg(test)]
mod tests {
    use super::*;

    fn zeroed() -> Vmcb {
        unsafe { core::mem::zeroed() }
    }

    #[test]
    fn hardware_exception_keeps_rip() {
        let mut vmcb = zeroed();
        vmcb.save.rip = 0x1234;

        inject_gp(&mut vmcb, 0);

        assert_eq!(vmcb.save.rip, 0x1234);
        assert_eq!(vmcb.control.event_inj.vector(), 13);
        assert!(vmcb.control.event_inj.error_code_valid());
        assert!(vmcb.control.event_inj.valid());
    }

    #[test]
    fn software_exception_sets_next_rip_when_supported() {
        let mut vmcb = zeroed();
        vmcb.save.rip = 0x1000;

        inject_software_exception(&mut vmcb, 3, None, 0x1001, true);

        assert_eq!(vmcb.save.rip, 0x1000);
        assert_eq!(vmcb.control.next_rip, 0x1001);
        assert!(vmcb.control.event_inj.valid());
    }

    #[test]
    fn software_exception_moves_rip_without_nrips() {
        let mut vmcb = zeroed();
        vmcb.save.rip = 0x1000;

        inject_software_exception(&mut vmcb, 3, None, 0x1001, false);

        assert_eq!(vmcb.save.rip, 0x1001);
        assert_eq!(vmcb.control.next_rip, 0);
    }

    #[test]
    fn nmi_uses_type_two() {
        let mut vmcb = zeroed();
        inject_nmi(&mut vmcb);
        assert_eq!(vmcb.control.event_inj.typ(), 2);
    }
}
