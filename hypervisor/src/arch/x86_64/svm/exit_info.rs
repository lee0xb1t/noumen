//! SVM exit codes and per-code `exit_info` decoding.

// Decode helpers cover the architectural exit-code surface.
#![allow(dead_code)]

use super::vmcb::control::ControlArea;

/// SVM exit code.
#[repr(transparent)]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct SvmExitCode(pub u32);

impl SvmExitCode {
    pub const INVALID: u32 = 0xffff_ffff;

    pub const EXCEPTION_BASE: u32 = 0x40;
    pub const EXCEPTION_LAST: u32 = 0x5f;

    pub const INTR: u32 = 0x60;
    pub const NMI: u32 = 0x61;
    pub const SMI: u32 = 0x62;
    pub const INIT: u32 = 0x63;
    pub const VINTR: u32 = 0x64;
    pub const CR0_SEL_WRITE: u32 = 0x65;
    pub const IDTR_READ: u32 = 0x66;
    pub const GDTR_READ: u32 = 0x67;
    pub const LDTR_READ: u32 = 0x68;
    pub const TR_READ: u32 = 0x69;
    pub const IDTR_WRITE: u32 = 0x6a;
    pub const GDTR_WRITE: u32 = 0x6b;
    pub const LDTR_WRITE: u32 = 0x6c;
    pub const TR_WRITE: u32 = 0x6d;
    pub const RDTSC: u32 = 0x6e;
    pub const RDPMC: u32 = 0x6f;
    pub const PUSHF: u32 = 0x70;
    pub const POPF: u32 = 0x71;
    pub const CPUID: u32 = 0x72;
    pub const RSM: u32 = 0x73;
    pub const IRET: u32 = 0x74;
    pub const SWINT: u32 = 0x75;
    pub const INVD: u32 = 0x76;
    pub const PAUSE: u32 = 0x77;
    pub const HLT: u32 = 0x78;
    pub const INVLPG: u32 = 0x79;
    pub const INVLPGA: u32 = 0x7a;
    pub const IOIO: u32 = 0x7b;
    pub const MSR: u32 = 0x7c;
    pub const TASK_SWITCH: u32 = 0x7d;
    pub const FERR_FREEZE: u32 = 0x7e;
    pub const SHUTDOWN: u32 = 0x7f;
    pub const VMRUN: u32 = 0x80;
    pub const VMMCALL: u32 = 0x81;
    pub const VMLOAD: u32 = 0x82;
    pub const VMSAVE: u32 = 0x83;
    pub const STGI: u32 = 0x84;
    pub const CLGI: u32 = 0x85;
    pub const SKINIT: u32 = 0x86;
    pub const RDTSCP: u32 = 0x87;
    pub const ICEBP: u32 = 0x88;
    pub const WBINVD: u32 = 0x89;
    pub const MONITOR: u32 = 0x8a;
    pub const MWAIT: u32 = 0x8b;
    pub const MWAIT_COND: u32 = 0x8c;
    pub const XSETBV: u32 = 0x8d;
    pub const RDPRU: u32 = 0x8e;
    pub const EFER_WRITE_TRAP: u32 = 0x8f;
    pub const CR_WRITE_TRAP_BASE: u32 = 0x90;
    pub const INVPCID: u32 = 0xa2;
    pub const BUS_LOCK: u32 = 0xa5;
    pub const IDLE_HLT: u32 = 0xa6;

    pub const NPF: u32 = 0x400;
    pub const AVIC_INCOMPLETE_IPI: u32 = 0x401;
    pub const AVIC_UNACCELERATED_ACCESS: u32 = 0x402;
    pub const VMGEXIT: u32 = 0x403;

    pub const fn from_raw(raw: u32) -> Self {
        Self(raw)
    }

    pub const fn raw(self) -> u32 {
        self.0
    }

    /// `VMRUN` failed; no guest instruction ran.
    pub const fn is_invalid(self) -> bool {
        self.0 == Self::INVALID
    }

    pub const fn is_exception(self) -> bool {
        self.0 >= Self::EXCEPTION_BASE && self.0 <= Self::EXCEPTION_LAST
    }

    /// Exception vector, for exception exits.
    pub const fn exception_vector(self) -> Option<u8> {
        if self.is_exception() {
            Some((self.0 - Self::EXCEPTION_BASE) as u8)
        } else {
            None
        }
    }

    pub const fn is_cr_read(self) -> bool {
        self.0 < 0x10
    }

    pub const fn is_cr_write(self) -> bool {
        self.0 >= 0x10 && self.0 < 0x20
    }

    pub const fn is_dr_read(self) -> bool {
        self.0 >= 0x20 && self.0 < 0x30
    }

    pub const fn is_dr_write(self) -> bool {
        self.0 >= 0x30 && self.0 < 0x40
    }

    /// Move-to-CR exits advance RIP after emulation; faults do not.
    pub const fn is_trap_class(self) -> bool {
        matches!(
            self.0,
            Self::CPUID
                | Self::MSR
                | Self::IOIO
                | Self::VMMCALL
                | Self::HLT
                | Self::INVD
                | Self::RDTSC
                | Self::RDTSCP
                | Self::RDPMC
                | Self::XSETBV
                | Self::WBINVD
                | Self::PAUSE
        ) || self.is_cr_write()
            || self.is_cr_read()
            || self.is_dr_read()
            || self.is_dr_write()
    }
}

/// Snapshot of the control-block exit information.
#[derive(Clone, Copy, Debug)]
pub struct ExitInfo {
    pub code: SvmExitCode,
    pub info1: u64,
    pub info2: u64,
    pub exit_int_info: u32,
    pub exit_int_info_err: u32,
}

impl ExitInfo {
    pub const fn from_control(control: &ControlArea) -> Self {
        Self {
            code: SvmExitCode(control.exit_code),
            info1: control.exit_info_1,
            info2: control.exit_info_2,
            exit_int_info: control.exit_int_info,
            exit_int_info_err: control.exit_int_info_err,
        }
    }

    pub const fn new(code: u32, info1: u64, info2: u64) -> Self {
        Self {
            code: SvmExitCode(code),
            info1,
            info2,
            exit_int_info: 0,
            exit_int_info_err: 0,
        }
    }

    /// Nested-page-fault details; only meaningful for `0x400`.
    pub const fn npf(&self) -> Option<NpfInfo> {
        if self.code.0 != SvmExitCode::NPF {
            return None;
        }

        Some(NpfInfo::from_parts(self.info1, self.info2))
    }

    /// Control-register access details; only for `0x00..=0x1F`.
    pub const fn cr_access(&self) -> Option<CrAccess> {
        if !(self.code.is_cr_read() || self.code.is_cr_write()) {
            return None;
        }

        Some(CrAccess::from_parts(self.code.0, self.info1))
    }

    /// I/O access details; only for `0x7B`.
    pub const fn io(&self) -> Option<IoInfo> {
        if self.code.0 != SvmExitCode::IOIO {
            return None;
        }

        Some(IoInfo::from_parts(self.info1, self.info2))
    }

    /// Intercepted exception details from `exit_int_info`.
    pub const fn exception(&self) -> Option<ExceptionInfo> {
        if !self.code.is_exception() {
            return None;
        }

        Some(ExceptionInfo::from_raw(
            self.exit_int_info,
            self.exit_int_info_err,
        ))
    }
}

/// `#NPF` (exit code `0x400`).
///
/// `info1` is a page-fault error code, but bit 0 means **present**, not "read";
/// there is no field describing the faulting entry's permissions.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct NpfInfo {
    /// Faulting guest physical address.
    pub gpa: u64,
    /// The walked entry was present, i.e. this is a permission violation.
    pub present: bool,
    pub write: bool,
    pub user: bool,
    /// A reserved bit was set (misconfiguration).
    pub rsvd: bool,
    /// Instruction fetch.
    pub fetch: bool,
}

impl NpfInfo {
    pub const PRESENT: u64 = 1 << 0;
    pub const WRITE: u64 = 1 << 1;
    pub const USER: u64 = 1 << 2;
    pub const RSVD: u64 = 1 << 3;
    pub const FETCH: u64 = 1 << 4;

    pub const fn from_parts(info1: u64, info2: u64) -> Self {
        Self {
            gpa: info2,
            present: info1 & Self::PRESENT != 0,
            write: info1 & Self::WRITE != 0,
            user: info1 & Self::USER != 0,
            rsvd: info1 & Self::RSVD != 0,
            fetch: info1 & Self::FETCH != 0,
        }
    }
}

/// Control-register access (`0x00..=0x1F`).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct CrAccess {
    pub cr: u8,
    pub write: bool,
    /// Operand GPR, valid only when [`Self::valid`] is set.
    pub gpr: u8,
    pub valid: bool,
}

impl CrAccess {
    pub const VALID: u64 = 1 << 63;
    pub const GPR_MASK: u64 = 0xf;

    pub const fn from_parts(exit_code: u32, info1: u64) -> Self {
        Self {
            cr: (exit_code & 0xf) as u8,
            write: exit_code & 0x10 != 0,
            gpr: (info1 & Self::GPR_MASK) as u8,
            valid: info1 & Self::VALID != 0,
        }
    }
}

/// I/O access (`0x7B`).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct IoInfo {
    pub port: u16,
    /// 1, 2 or 4 bytes.
    pub size: u8,
    pub string: bool,
    pub rep: bool,
    /// `true` for IN, `false` for OUT.
    pub input: bool,
    /// Address of the instruction after the access (per AMD).
    pub next_rip: u64,
}

impl IoInfo {
    pub const fn from_parts(info1: u64, info2: u64) -> Self {
        let size_code = ((info1 >> 4) & 0x7) as u8;

        Self {
            port: ((info1 >> 16) & 0xffff) as u16,
            size: match size_code {
                0 => 1,
                1 => 2,
                3 => 4,
                _ => 0,
            },
            string: info1 & (1 << 2) != 0,
            rep: info1 & (1 << 3) != 0,
            input: info1 & 1 != 0,
            next_rip: info2,
        }
    }
}

/// `exit_int_info` for an intercepted exception.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ExceptionInfo {
    pub vector: u8,
    pub typ: u8,
    pub error_code: Option<u32>,
    /// The `VALID` bit of `exit_int_info`; the other fields are stale if clear.
    pub valid: bool,
}

impl ExceptionInfo {
    pub const VALID: u32 = 1 << 31;
    pub const VALID_ERR: u32 = 1 << 11;

    pub const fn from_raw(info: u32, err: u32) -> Self {
        Self {
            vector: (info & 0xff) as u8,
            typ: ((info >> 8) & 0x7) as u8,
            error_code: if info & Self::VALID_ERR != 0 {
                Some(err)
            } else {
                None
            },
            valid: info & Self::VALID != 0,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cr_and_dr_codes_follow_the_intercept_bits() {
        assert!(SvmExitCode(0x00).is_cr_read());
        assert!(SvmExitCode(0x03).is_cr_read());
        assert!(SvmExitCode(0x10).is_cr_write());
        assert!(SvmExitCode(0x13).is_cr_write());
        assert!(SvmExitCode(0x20).is_dr_read());
        assert!(SvmExitCode(0x37).is_dr_write());
        assert!(!SvmExitCode(0x40).is_cr_read());
    }

    #[test]
    fn cr_access_decodes_gpr_and_direction() {
        let info = ExitInfo::new(0x13, 0x8000_0000_0000_0002, 0);
        let cr = info.cr_access().unwrap();

        assert_eq!(cr.cr, 3);
        assert!(cr.write);
        assert!(cr.valid);
        assert_eq!(cr.gpr, 2);
    }

    #[test]
    fn cr_access_without_valid_bit_is_reported() {
        let info = ExitInfo::new(0x00, 0, 0);
        let cr = info.cr_access().unwrap();

        assert!(!cr.valid);
        assert!(!cr.write);
        assert_eq!(cr.cr, 0);
    }

    #[test]
    fn npf_bit_zero_is_present_not_read() {
        // Permission violation on a fetch: P=1, I/D=1, no write.
        let info = ExitInfo::new(SvmExitCode::NPF, NpfInfo::PRESENT | NpfInfo::FETCH, 0x1234_5000);
        let npf = info.npf().unwrap();

        assert!(npf.present);
        assert!(npf.fetch);
        assert!(!npf.write);
        assert_eq!(npf.gpa, 0x1234_5000);

        // Not-present read: P=0.
        let missing = ExitInfo::new(SvmExitCode::NPF, 0, 0x1000).npf().unwrap();
        assert!(!missing.present);
        assert!(!missing.fetch);
    }

    #[test]
    fn io_info_decodes_port_and_size() {
        let info1 = 1 | (3 << 4) | (1 << 2) | (0x3f8 << 16);
        let io = ExitInfo::new(SvmExitCode::IOIO, info1, 0xdead).io().unwrap();

        assert_eq!(io.port, 0x3f8);
        assert_eq!(io.size, 4);
        assert!(io.input);
        assert!(io.string);
        assert!(!io.rep);
        assert_eq!(io.next_rip, 0xdead);
    }

    #[test]
    fn exception_vector_comes_from_the_code() {
        let code = SvmExitCode(SvmExitCode::EXCEPTION_BASE + 14);
        assert!(code.is_exception());
        assert_eq!(code.exception_vector(), Some(14));
        assert_eq!(SvmExitCode(0x60).exception_vector(), None);
    }

    #[test]
    fn exception_info_carries_optional_error_code() {
        let info = (14 | (3 << 8) | (1 << 11)) | (1 << 31);
        let ex = ExceptionInfo::from_raw(info, 0x6);

        assert_eq!(ex.vector, 14);
        assert_eq!(ex.typ, 3);
        assert_eq!(ex.error_code, Some(6));
        assert!(ex.valid);

        let no_err = ExceptionInfo::from_raw(3 | (3 << 8) | (1 << 31), 0);
        assert_eq!(no_err.error_code, None);
        assert!(no_err.valid);

        let stale = ExceptionInfo::from_raw(3 | (3 << 8), 0);
        assert!(!stale.valid);
    }

    #[test]
    fn invalid_vmrun_is_detected() {
        assert!(SvmExitCode(SvmExitCode::INVALID).is_invalid());
        assert!(!SvmExitCode(SvmExitCode::NPF).is_invalid());
    }

    #[test]
    fn trap_class_exits_advance_rip() {
        assert!(SvmExitCode(SvmExitCode::CPUID).is_trap_class());
        assert!(SvmExitCode(SvmExitCode::MSR).is_trap_class());
        assert!(SvmExitCode(SvmExitCode::VMMCALL).is_trap_class());
        assert!(!SvmExitCode(SvmExitCode::NPF).is_trap_class());
        assert!(!SvmExitCode(SvmExitCode::SHUTDOWN).is_trap_class());
        assert!(!SvmExitCode(SvmExitCode::EXCEPTION_BASE + 14).is_trap_class());
    }
}
