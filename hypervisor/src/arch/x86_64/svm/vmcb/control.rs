//! VMCB control area (offset 0x000..0x3FF).

use core::mem::offset_of;

use super::clean::CleanBits;

/// Event-injection type. SVM defines only these four values; VMX's "software
/// exception" and "privileged software exception" types do not exist.
#[repr(u8)]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SvmEventType {
    ExternalInterrupt = 0,
    Nmi = 2,
    Exception = 3,
    SoftwareInterrupt = 4,
}

bitfield::bitfield! {
    /// CR0..CR15 read intercepts. Bit N = CRN.
    #[repr(transparent)]
    #[derive(Clone, Copy, Default, PartialEq, Eq)]
    pub struct InterceptCrRead(u16);

    pub cr0, set_cr0: 0;
    pub cr2, set_cr2: 2;
    pub cr3, set_cr3: 3;
    pub cr4, set_cr4: 4;
    pub cr8, set_cr8: 8;
    pub cr15, set_cr15: 15;
}

bitfield::bitfield! {
    /// CR0..CR15 write intercepts. Bit N = CRN.
    #[repr(transparent)]
    #[derive(Clone, Copy, Default, PartialEq, Eq)]
    pub struct InterceptCrWrite(u16);

    pub cr0, set_cr0: 0;
    pub cr3, set_cr3: 3;
    pub cr4, set_cr4: 4;
    pub cr8, set_cr8: 8;
    pub cr15, set_cr15: 15;
}

bitfield::bitfield! {
    /// DR0..DR15 read intercepts. Bit N = DRN.
    #[repr(transparent)]
    #[derive(Clone, Copy, Default, PartialEq, Eq)]
    pub struct InterceptDrRead(u16);

    pub dr0, set_dr0: 0;
    pub dr6, set_dr6: 6;
    pub dr7, set_dr7: 7;
    pub dr15, set_dr15: 15;
}

bitfield::bitfield! {
    /// DR0..DR15 write intercepts. Bit N = DRN.
    #[repr(transparent)]
    #[derive(Clone, Copy, Default, PartialEq, Eq)]
    pub struct InterceptDrWrite(u16);

    pub dr0, set_dr0: 0;
    pub dr6, set_dr6: 6;
    pub dr7, set_dr7: 7;
    pub dr15, set_dr15: 15;
}

bitfield::bitfield! {
    /// Exception intercepts: bit N = vector N.
    #[repr(transparent)]
    #[derive(Clone, Copy, Default, PartialEq, Eq)]
    pub struct InterceptException(u32);

    pub de, set_de: 0;
    pub db, set_db: 1;
    pub nmi, set_nmi: 2;
    pub bp, set_bp: 3;
    pub of, set_of: 4;
    pub br, set_br: 5;
    pub ud, set_ud: 6;
    pub nm, set_nm: 7;
    pub df, set_df: 8;
    pub ts, set_ts: 10;
    pub np, set_np: 11;
    pub ss, set_ss: 12;
    pub gp, set_gp: 13;
    pub pf, set_pf: 14;
    pub mf, set_mf: 16;
    pub ac, set_ac: 17;
    pub mc, set_mc: 18;
    pub xm, set_xm: 19;
    pub ve, set_ve: 20;
    pub cp, set_cp: 21;
}

bitfield::bitfield! {
    /// Intercept word 1 (0x00C).
    #[repr(transparent)]
    #[derive(Clone, Copy, Default, PartialEq, Eq)]
    pub struct InterceptMisc1(u32);

    pub intr, set_intr: 0;
    pub nmi, set_nmi: 1;
    pub smi, set_smi: 2;
    pub init, set_init: 3;
    pub vintr, set_vintr: 4;
    pub selective_cr0, set_selective_cr0: 5;
    pub store_idtr, set_store_idtr: 6;
    pub store_gdtr, set_store_gdtr: 7;
    pub store_ldtr, set_store_ldtr: 8;
    pub store_tr, set_store_tr: 9;
    pub load_idtr, set_load_idtr: 10;
    pub load_gdtr, set_load_gdtr: 11;
    pub load_ldtr, set_load_ldtr: 12;
    pub load_tr, set_load_tr: 13;
    /// TSC 拦截：**暂不实现**（不置位，guest 直接读物理 TSC）。
    pub rdtsc, set_rdtsc: 14;
    pub rdpmc, set_rdpmc: 15;
    pub pushf, set_pushf: 16;
    pub popf, set_popf: 17;
    pub cpuid, set_cpuid: 18;
    pub rsm, set_rsm: 19;
    pub iret, set_iret: 20;
    pub intn, set_intn: 21;
    pub invd, set_invd: 22;
    pub pause, set_pause: 23;
    pub hlt, set_hlt: 24;
    pub invlpg, set_invlpg: 25;
    pub invlpga, set_invlpga: 26;
    pub ioio_prot, set_ioio_prot: 27;
    pub msr_prot, set_msr_prot: 28;
    pub task_switch, set_task_switch: 29;
    pub ferr_freeze, set_ferr_freeze: 30;
    pub shutdown, set_shutdown: 31;
}

bitfield::bitfield! {
    /// Intercept word 2 (0x010).
    #[repr(transparent)]
    #[derive(Clone, Copy, Default, PartialEq, Eq)]
    pub struct InterceptMisc2(u32);

    pub vmrun, set_vmrun: 0;
    pub vmmcall, set_vmmcall: 1;
    pub vmload, set_vmload: 2;
    pub vmsave, set_vmsave: 3;
    pub stgi, set_stgi: 4;
    pub clgi, set_clgi: 5;
    pub skinit, set_skinit: 6;
    /// `RDTSCP` 拦截：**暂不实现**（不置位）。
    pub rdtscp, set_rdtscp: 7;
    pub icebp, set_icebp: 8;
    pub wbinvd, set_wbinvd: 9;
    pub monitor, set_monitor: 10;
    pub mwait, set_mwait: 11;
    pub mwait_cond, set_mwait_cond: 12;
    pub xsetbv, set_xsetbv: 13;
    pub rdpru, set_rdpru: 14;
    pub efer_write_trap, set_efer_write_trap: 15;
    pub cr0_write_trap, set_cr0_write_trap: 16;
    pub cr4_write_trap, set_cr4_write_trap: 20;
    pub cr8_write_trap, set_cr8_write_trap: 24;
}

bitfield::bitfield! {
    /// Intercept word 3 (0x014). Newer APM additions; noumen leaves it zero.
    #[repr(transparent)]
    #[derive(Clone, Copy, Default, PartialEq, Eq)]
    pub struct InterceptMisc3(u32);

    pub invlpgb, set_invlpgb: 0;
    pub invlpgb_illegal, set_invlpgb_illegal: 1;
    pub invpcid, set_invpcid: 2;
    pub mcommit, set_mcommit: 3;
    pub tlbsync, set_tlbsync: 4;
    pub buslock, set_buslock: 5;
    pub idle_hlt, set_idle_hlt: 6;
}

bitfield::bitfield! {
    /// `int_ctl` (0x060): virtual interrupt / AVIC controls.
    #[repr(transparent)]
    #[derive(Clone, Copy, Default, PartialEq, Eq)]
    pub struct IntCtl(u32);

    /// Only the low four bits are defined.
    pub u8, v_tpr, set_v_tpr: 3, 0;
    pub v_irq, set_v_irq: 8;
    pub vgif, set_vgif: 9;
    pub v_nmi_pending, set_v_nmi_pending: 11;
    pub v_nmi_blocking, set_v_nmi_blocking: 12;
    pub u8, v_intr_prio, set_v_intr_prio: 19, 16;
    pub v_ign_tpr, set_v_ign_tpr: 20;
    pub v_intr_masking, set_v_intr_masking: 24;
    pub v_gif_enable, set_v_gif_enable: 25;
    pub v_nmi_enable, set_v_nmi_enable: 26;
    pub x2avic_enable, set_x2avic_enable: 30;
    pub avic_enable, set_avic_enable: 31;
}

bitfield::bitfield! {
    /// Event injection (0x0A8). The error code lives in the adjacent u32.
    #[repr(transparent)]
    #[derive(Clone, Copy, Default, PartialEq, Eq)]
    pub struct EventInj(u32);

    pub u8, vector, set_vector: 7, 0;
    pub u8, typ, set_typ: 10, 8;
    pub error_code_valid, set_error_code_valid: 11;
    pub valid, set_valid: 31;
}

/// Control area. Offsets are asserted below; keep the padding fields.
#[repr(C)]
#[derive(Clone, Copy)]
pub struct ControlArea {
    pub intercept_cr_read: InterceptCrRead,
    pub intercept_cr_write: InterceptCrWrite,
    pub intercept_dr_read: InterceptDrRead,
    pub intercept_dr_write: InterceptDrWrite,
    pub intercept_exception: InterceptException,
    pub intercept_misc1: InterceptMisc1,
    pub intercept_misc2: InterceptMisc2,
    pub intercept_misc3: InterceptMisc3,
    reserved0: [u8; 0x3c - 0x18],

    pub pause_filter_thresh: u16,
    pub pause_filter_count: u16,
    pub iopm_base_pa: u64,
    pub msrpm_base_pa: u64,
    /// TSC offset：**暂不实现**（保持 0，guest TSC = 物理 TSC）。
    pub tsc_offset: u64,
    pub asid: u32,
    pub tlb_control: u8,
    reserved1: [u8; 3],

    pub int_ctl: IntCtl,
    pub int_vector: u32,
    /// Hardware-maintained: bit0 interrupt shadow, bit1 guest IF.
    pub int_state: u32,
    reserved2: [u8; 4],

    pub exit_code: u32,
    pub exit_code_hi: u32,
    pub exit_info_1: u64,
    pub exit_info_2: u64,
    pub exit_int_info: u32,
    pub exit_int_info_err: u32,

    pub nested_ctl: u64,
    pub avic_vapic_bar: u64,
    pub ghcb_gpa: u64,
    pub event_inj: EventInj,
    pub event_inj_err: u32,
    pub nested_cr3: u64,
    pub virt_ext: u64,
    pub clean: CleanBits,
    reserved3: u32,
    pub next_rip: u64,
    pub insn_len: u8,
    pub insn_bytes: [u8; 15],
    pub avic_backing_page: u64,
    reserved4: u64,
    pub avic_logical_id: u64,
    pub avic_physical_id: u64,
    reserved5: u64,
    pub vmsa_pa: u64,
    reserved6: [u8; 0x3e0 - 0x110],
    /// 0x3E0..0x3FF is reserved for the hypervisor.
    pub host: [u8; 0x20],
}

const _: () = assert!(core::mem::size_of::<ControlArea>() == 0x400);
const _: () = assert!(offset_of!(ControlArea, intercept_cr_read) == 0x000);
const _: () = assert!(offset_of!(ControlArea, intercept_cr_write) == 0x002);
const _: () = assert!(offset_of!(ControlArea, intercept_dr_read) == 0x004);
const _: () = assert!(offset_of!(ControlArea, intercept_dr_write) == 0x006);
const _: () = assert!(offset_of!(ControlArea, intercept_exception) == 0x008);
const _: () = assert!(offset_of!(ControlArea, intercept_misc1) == 0x00c);
const _: () = assert!(offset_of!(ControlArea, intercept_misc2) == 0x010);
const _: () = assert!(offset_of!(ControlArea, intercept_misc3) == 0x014);
const _: () = assert!(offset_of!(ControlArea, pause_filter_thresh) == 0x03c);
const _: () = assert!(offset_of!(ControlArea, iopm_base_pa) == 0x040);
const _: () = assert!(offset_of!(ControlArea, msrpm_base_pa) == 0x048);
const _: () = assert!(offset_of!(ControlArea, tsc_offset) == 0x050);
const _: () = assert!(offset_of!(ControlArea, asid) == 0x058);
const _: () = assert!(offset_of!(ControlArea, tlb_control) == 0x05c);
const _: () = assert!(offset_of!(ControlArea, int_ctl) == 0x060);
const _: () = assert!(offset_of!(ControlArea, int_vector) == 0x064);
const _: () = assert!(offset_of!(ControlArea, int_state) == 0x068);
const _: () = assert!(offset_of!(ControlArea, exit_code) == 0x070);
const _: () = assert!(offset_of!(ControlArea, exit_info_1) == 0x078);
const _: () = assert!(offset_of!(ControlArea, exit_info_2) == 0x080);
const _: () = assert!(offset_of!(ControlArea, exit_int_info) == 0x088);
const _: () = assert!(offset_of!(ControlArea, nested_ctl) == 0x090);
const _: () = assert!(offset_of!(ControlArea, event_inj) == 0x0a8);
const _: () = assert!(offset_of!(ControlArea, event_inj_err) == 0x0ac);
const _: () = assert!(offset_of!(ControlArea, nested_cr3) == 0x0b0);
const _: () = assert!(offset_of!(ControlArea, virt_ext) == 0x0b8);
const _: () = assert!(offset_of!(ControlArea, clean) == 0x0c0);
const _: () = assert!(offset_of!(ControlArea, next_rip) == 0x0c8);
const _: () = assert!(offset_of!(ControlArea, insn_len) == 0x0d0);
const _: () = assert!(offset_of!(ControlArea, avic_backing_page) == 0x0e0);
const _: () = assert!(offset_of!(ControlArea, vmsa_pa) == 0x108);
const _: () = assert!(offset_of!(ControlArea, host) == 0x3e0);

impl ControlArea {
    /// Program an event for the next `VMRUN`.
    ///
    /// `valid` is written last so a half-written injection can never be
    /// consumed. The error code goes to the adjacent u32, not into the event
    /// word.
    pub fn inject_event(&mut self, vector: u8, typ: SvmEventType, error_code: Option<u32>) {
        self.event_inj_err = error_code.unwrap_or(0);

        let mut event = EventInj(0);
        event.set_vector(vector);
        event.set_typ(typ as u8);
        event.set_error_code_valid(error_code.is_some());
        event.set_valid(true);

        self.event_inj = event;
    }

    /// Cancel a pending injection without delivering it.
    pub fn cancel_injection(&mut self) {
        self.event_inj = EventInj(0);
        self.event_inj_err = 0;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn intercept_bit_positions_match_the_apm() {
        let mut misc1 = InterceptMisc1::default();
        misc1.set_cpuid(true);
        assert_eq!(misc1.0, 1 << 18);
        misc1.set_msr_prot(true);
        assert_eq!(misc1.0, (1 << 18) | (1 << 28));

        let mut misc2 = InterceptMisc2::default();
        misc2.set_vmrun(true);
        misc2.set_vmmcall(true);
        misc2.set_efer_write_trap(true);
        assert_eq!(misc2.0, (1 << 0) | (1 << 1) | (1 << 15));

        let mut cr = InterceptCrRead::default();
        cr.set_cr3(true);
        assert_eq!(cr.0, 1 << 3);
    }

    #[test]
    fn int_ctl_fields_do_not_overlap() {
        let mut ctl = IntCtl::default();
        ctl.set_v_tpr(0xf);
        ctl.set_v_irq(true);
        ctl.set_v_intr_masking(true);
        assert_eq!(ctl.0, 0xf | (1 << 8) | (1 << 24));
        assert_eq!(ctl.v_tpr(), 0xf);
        assert!(ctl.v_irq());
    }

    #[test]
    fn gp_injection_encoding() {
        let mut control = unsafe { core::mem::zeroed::<ControlArea>() };
        control.inject_event(13, SvmEventType::Exception, Some(0));

        assert_eq!(control.event_inj_err, 0);
        // vector 13, type 3 (<<8), error-code-valid (<<11), valid (<<31).
        assert_eq!(control.event_inj.0, 13 | (3 << 8) | (1 << 11) | (1 << 31));
        assert!(control.event_inj.valid());
        assert_eq!(control.event_inj.vector(), 13);
        assert_eq!(control.event_inj.typ(), 3);
    }

    #[test]
    fn breakpoint_injection_has_no_error_code() {
        let mut control = unsafe { core::mem::zeroed::<ControlArea>() };
        control.inject_event(3, SvmEventType::Exception, None);
        assert_eq!(control.event_inj_err, 0);
        assert_eq!(control.event_inj.0, 3 | (3 << 8) | (1 << 31));
        assert!(!control.event_inj.error_code_valid());
    }

    #[test]
    fn cancellation_clears_both_words() {
        let mut control = unsafe { core::mem::zeroed::<ControlArea>() };
        control.inject_event(14, SvmEventType::Exception, Some(7));
        control.cancel_injection();
        assert_eq!(control.event_inj.0, 0);
        assert_eq!(control.event_inj_err, 0);
    }

    #[test]
    fn control_area_offsets_are_stable() {
        assert_eq!(core::mem::size_of::<ControlArea>(), 0x400);
        assert_eq!(offset_of!(ControlArea, event_inj), 0x0a8);
        assert_eq!(offset_of!(ControlArea, event_inj_err), 0x0ac);
        assert_eq!(offset_of!(ControlArea, nested_cr3), 0x0b0);
    }
}
