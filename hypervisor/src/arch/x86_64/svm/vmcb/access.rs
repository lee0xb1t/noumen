//! Typed field access for the VMCB.

use super::clean::CleanBit;
use super::control::{EventInj, SvmEventType};
use super::Vmcb;

macro_rules! plain_field {
    ($name:ident, $field:ident, $ty:ty, $doc:literal) => {
        #[doc = $doc]
        #[repr(transparent)]
        #[derive(Clone, Copy, Debug, PartialEq, Eq)]
        pub struct $name(pub $ty);

        impl $name {
            pub const fn new(value: $ty) -> Self {
                Self(value)
            }

            pub fn read(vmcb: &Vmcb) -> Self {
                Self(vmcb.save.$field)
            }

            /// No clean bit covers this field: it is re-read on every entry.
            pub fn write(self, vmcb: &mut Vmcb) {
                vmcb.save.$field = self.0;
            }

            pub const fn raw(self) -> $ty {
                self.0
            }
        }
    };
}

macro_rules! save_clean_field {
    ($name:ident, $field:ident, $ty:ty, $clean:ident, $doc:literal) => {
        #[doc = $doc]
        #[repr(transparent)]
        #[derive(Clone, Copy, Debug, PartialEq, Eq)]
        pub struct $name(pub $ty);

        impl $name {
            pub const fn new(value: $ty) -> Self {
                Self(value)
            }

            pub fn read(vmcb: &Vmcb) -> Self {
                Self(vmcb.save.$field)
            }

            pub fn write(self, vmcb: &mut Vmcb) {
                vmcb.save.$field = self.0;
                vmcb.control.clean.mark_dirty(CleanBit::$clean);
            }

            pub const fn raw(self) -> $ty {
                self.0
            }
        }
    };
}

macro_rules! ctrl_clean_field {
    ($name:ident, $field:ident, $ty:ty, $clean:ident, $doc:literal) => {
        #[doc = $doc]
        #[repr(transparent)]
        #[derive(Clone, Copy, Debug, PartialEq, Eq)]
        pub struct $name(pub $ty);

        impl $name {
            pub const fn new(value: $ty) -> Self {
                Self(value)
            }

            pub fn read(vmcb: &Vmcb) -> Self {
                Self(vmcb.control.$field)
            }

            pub fn write(self, vmcb: &mut Vmcb) {
                vmcb.control.$field = self.0;
                vmcb.control.clean.mark_dirty(CleanBit::$clean);
            }

            pub const fn raw(self) -> $ty {
                self.0
            }
        }
    };
}

plain_field!(Rip, rip, u64, "Guest RIP.");
plain_field!(Rsp, rsp, u64, "Guest RSP (hardware-swapped).");
plain_field!(Rax, rax, u64, "Guest RAX (the only GPR hardware swaps).");
plain_field!(Rflags, rflags, u64, "Guest RFLAGS; TF is the single-step bit.");

save_clean_field!(Cr0, cr0, u64, Cr, "Guest CR0.");
save_clean_field!(Cr3, cr3, u64, Cr, "Guest CR3 (data under NPT).");
save_clean_field!(Cr4, cr4, u64, Cr, "Guest CR4.");
save_clean_field!(Efer, efer, u64, Cr, "Guest EFER (keeps SVME set, see docs 4.7).");
save_clean_field!(Dr6, dr6, u64, Dr, "Guest DR6.");
save_clean_field!(Dr7, dr7, u64, Dr, "Guest DR7.");
save_clean_field!(Cr2, cr2, u64, Cr2, "Guest CR2; written before injecting #PF.");
save_clean_field!(GPat, g_pat, u64, Npt, "Guest PAT (used by NPT translations).");

ctrl_clean_field!(
    NestedCr3,
    nested_cr3,
    u64,
    Npt,
    "NPT root physical address (0x0B0)."
);
ctrl_clean_field!(
    NestedCtl,
    nested_ctl,
    u64,
    Npt,
    "Nested-paging control (bit0 = NP_ENABLE)."
);
ctrl_clean_field!(TscOffset, tsc_offset, u64, Intercepts, "TSC offset.");
ctrl_clean_field!(IopmBase, iopm_base_pa, u64, PermMaps, "I/O permission map PA.");
ctrl_clean_field!(MsrpmBase, msrpm_base_pa, u64, PermMaps, "MSR permission map PA.");
ctrl_clean_field!(Asid, asid, u32, Asid, "Guest ASID; 0 is reserved for the host.");
ctrl_clean_field!(TlbControl, tlb_control, u8, Asid, "TLB control command (next VMRUN).");

impl Rflags {
    pub const TF: u64 = 1 << 8;
    pub const IF: u64 = 1 << 9;
    pub const RF: u64 = 1 << 16;

    pub fn trap_flag(self) -> bool {
        self.0 & Self::TF != 0
    }

    pub fn interrupt_enable(self) -> bool {
        self.0 & Self::IF != 0
    }

    pub fn with_trap_flag(self, on: bool) -> Self {
        let mask = if on { Self::TF } else { 0 };

        Self((self.0 & !Self::TF) | mask)
    }

    pub fn with_interrupt_enable(self, on: bool) -> Self {
        let mask = if on { Self::IF } else { 0 };

        Self((self.0 & !Self::IF) | mask)
    }
}

impl TlbControl {
    pub const DO_NOTHING: u8 = 0;
    /// Flush the entire TLB (all ASIDs).
    pub const FLUSH_ALL: u8 = 1;
    /// Flush only this guest's ASID.
    pub const FLUSH_ASID: u8 = 3;
    /// Flush this guest's ASID, keeping global translations.
    pub const FLUSH_ASID_NON_GLOBAL: u8 = 7;

    /// Queue a flush for the next `VMRUN`. The hardware does not necessarily
    /// clear the command after honouring it, so always write the next command
    /// explicitly (use [`Self::DO_NOTHING`] when none is needed).
    pub fn request(value: u8) -> Self {
        Self(value)
    }
}

impl Vmcb {
    /// Program an event for the next entry. See
    /// [`super::control::ControlArea::inject_event`].
    pub fn inject_event(&mut self, vector: u8, typ: SvmEventType, error_code: Option<u32>) {
        self.control.inject_event(vector, typ, error_code);
    }

    pub fn event_injection(&self) -> EventInj {
        self.control.event_inj
    }

    /// `VMRUN` requires a 4KB-aligned physical address.
    pub fn assert_pa(pa: u64) {
        assert_eq!(pa & 0xfff, 0, "VMCB physical address must be 4KB aligned");
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn zeroed() -> Vmcb {
        unsafe { core::mem::zeroed() }
    }

    #[test]
    fn cr3_write_clears_the_cr_group_only() {
        let mut vmcb = zeroed();
        vmcb.control.clean.mark_all_clean();

        Cr3::new(0x1234_5000).write(&mut vmcb);

        assert_eq!(Cr3::read(&vmcb).raw(), 0x1234_5000);
        assert!(!vmcb.control.clean.is_clean(CleanBit::Cr));
        assert!(vmcb.control.clean.is_clean(CleanBit::Dr));
        assert!(vmcb.control.clean.is_clean(CleanBit::Npt));
    }

    #[test]
    fn rip_has_no_clean_bit() {
        let mut vmcb = zeroed();
        vmcb.control.clean.mark_all_clean();

        Rip::new(0xdead).write(&mut vmcb);

        assert_eq!(Rip::read(&vmcb).raw(), 0xdead);
        // Everything that was clean stays clean.
        assert!(vmcb.control.clean.is_clean(CleanBit::Cr));
        assert!(vmcb.control.clean.is_clean(CleanBit::Seg));
    }

    #[test]
    fn asid_and_tlb_share_the_asid_group() {
        let mut vmcb = zeroed();
        vmcb.control.clean.mark_all_clean();

        Asid::new(1).write(&mut vmcb);
        TlbControl::request(TlbControl::FLUSH_ASID).write(&mut vmcb);

        assert!(!vmcb.control.clean.is_clean(CleanBit::Asid));
        assert_eq!(Asid::read(&vmcb).raw(), 1);
        assert_eq!(TlbControl::read(&vmcb).raw(), 3);
    }

    #[test]
    fn rflags_helpers_preserve_other_bits() {
        let flags = Rflags::new(0x202).with_trap_flag(true);
        assert!(flags.trap_flag());
        assert!(flags.interrupt_enable());

        let cleared = flags.with_trap_flag(false);
        assert!(!cleared.trap_flag());
        assert!(cleared.interrupt_enable());

        let no_if = cleared.with_interrupt_enable(false);
        assert!(!no_if.interrupt_enable());
        assert_eq!(no_if.raw(), 0x2);
    }

    #[test]
    fn nested_paging_bits_are_in_the_npt_group() {
        let mut vmcb = zeroed();
        vmcb.control.clean.mark_all_clean();

        NestedCtl::new(1).write(&mut vmcb);
        NestedCr3::new(0x2000).write(&mut vmcb);

        assert!(!vmcb.control.clean.is_clean(CleanBit::Npt));
        assert_eq!(NestedCtl::read(&vmcb).raw(), 1);
    }

    #[test]
    fn injection_goes_through_the_vmcb_helper() {
        let mut vmcb = zeroed();
        vmcb.inject_event(3, SvmEventType::Exception, None);

        assert!(vmcb.event_injection().valid());
        assert_eq!(vmcb.event_injection().vector(), 3);
        assert_eq!(vmcb.control.event_inj_err, 0);
    }
}
