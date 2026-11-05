//! Guest general-purpose register context shared by the VMX and SVM backends.
//!
//! The layout is fixed by `vmx/stub.S` (and, later, `svm/stub.S`), so every
//! field offset is asserted at compile time. The two backends fill it
//! differently:
//!
//! * VMX does not context-switch GPRs: the exit stub saves the guest values
//!   directly, and `RSP` lives in the `GUEST_RSP` VMCS field.
//! * SVM context-switches exactly `RAX` and `RSP` through the VMCB; the exit
//!   stub must fill `rax` from `vmcb.save.rax` and leave `rsp` out of this
//!   structure (the SVM side reads `vmcb.save.rsp`).
//!
//! Keep `RSP` out of the structure: both backends keep it in hardware state.

use core::mem::offset_of;

use crate::arch::x86_64::vmx::vmcs::guest;
use crate::error::VmResult;

/// General-purpose register.
#[repr(u8)]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum GeneralPurposeRegister {
    Rax = 0,
    Rcx = 1,
    Rdx = 2,
    Rbx = 3,
    Rsp = 4,
    Rbp = 5,
    Rsi = 6,
    Rdi = 7,
    R8 = 8,
    R9 = 9,
    R10 = 10,
    R11 = 11,
    R12 = 12,
    R13 = 13,
    R14 = 14,
    R15 = 15,
}

impl GeneralPurposeRegister {
    /// Decode the 4-bit register number from an instruction encoding or a
    /// VM-exit qualification. Every 4-bit value is a valid register.
    pub const fn from_raw(raw: u8) -> Self {
        match raw {
            0 => Self::Rax,
            1 => Self::Rcx,
            2 => Self::Rdx,
            3 => Self::Rbx,
            4 => Self::Rsp,
            5 => Self::Rbp,
            6 => Self::Rsi,
            7 => Self::Rdi,
            8 => Self::R8,
            9 => Self::R9,
            10 => Self::R10,
            11 => Self::R11,
            12 => Self::R12,
            13 => Self::R13,
            14 => Self::R14,
            15 => Self::R15,
            _ => panic!("invalid general-purpose register number"),
        }
    }
}
#[repr(C)]
#[derive(Clone, Copy, Debug)]
pub struct GuestContext {
    pub rax: u64,
    pub rcx: u64,
    pub rdx: u64,
    pub rbx: u64,
    pub _pad1: u64, // skip rsp
    pub rbp: u64,
    pub rsi: u64,
    pub rdi: u64,
    pub r8: u64,
    pub r9: u64,
    pub r10: u64,
    pub r11: u64,
    pub r12: u64,
    pub r13: u64,
    pub r14: u64,
    pub r15: u64,

    pub cr2: u64,
    pub cr8: u64,

    pub dr0: u64,
    pub dr1: u64,
    pub dr2: u64,
    pub dr3: u64,
    pub dr6: u64,
    pub _pad3: [u8; 8], // padding: 让 XMM 区保持在 192 偏移

    // SSE registers (16 bytes alignment)
    pub xmm0: u128,
    pub xmm1: u128,
    pub xmm2: u128,
    pub xmm3: u128,
    pub xmm4: u128,
    pub xmm5: u128,
    pub xmm6: u128,
    pub xmm7: u128,
    pub xmm8: u128,
    pub xmm9: u128,
    pub xmm10: u128,
    pub xmm11: u128,
    pub xmm12: u128,
    pub xmm13: u128,
    pub xmm14: u128,
    pub xmm15: u128,

    /// Host DR values saved by the SVM stub around VMRUN. DR0–DR3 and DR6 are
    /// not in the VMCB save area; DR7 is, but VMRUN replaces it with the guest
    /// value, so all six must be restored on the host side.
    pub host_dr0: u64,
    pub host_dr1: u64,
    pub host_dr2: u64,
    pub host_dr3: u64,
    pub host_dr6: u64,
    pub host_dr7: u64,
}

impl GuestContext {
    #[inline]
    pub fn eax(&self) -> u32 {
        self.rax as u32
    }

    #[inline]
    pub fn edx(&self) -> u32 {
        self.rdx as u32
    }

    #[inline]
    pub fn ecx(&self) -> u32 {
        self.rcx as u32
    }

    /// Read a guest general-purpose register.
    ///
    /// `RSP` is not stored in this structure: each backend keeps it in its own
    /// hardware state (VMX: `GUEST_RSP`; SVM: `vmcb.save.rsp`).
    #[inline]
    pub fn read_gpr(&self, reg: GeneralPurposeRegister) -> VmResult<u64> {
        let value = match reg {
            GeneralPurposeRegister::Rax => self.rax,
            GeneralPurposeRegister::Rcx => self.rcx,
            GeneralPurposeRegister::Rdx => self.rdx,
            GeneralPurposeRegister::Rbx => self.rbx,
            GeneralPurposeRegister::Rsp => return guest::Rsp::read().map(|rsp| rsp.as_raw()),
            GeneralPurposeRegister::Rbp => self.rbp,
            GeneralPurposeRegister::Rsi => self.rsi,
            GeneralPurposeRegister::Rdi => self.rdi,
            GeneralPurposeRegister::R8 => self.r8,
            GeneralPurposeRegister::R9 => self.r9,
            GeneralPurposeRegister::R10 => self.r10,
            GeneralPurposeRegister::R11 => self.r11,
            GeneralPurposeRegister::R12 => self.r12,
            GeneralPurposeRegister::R13 => self.r13,
            GeneralPurposeRegister::R14 => self.r14,
            GeneralPurposeRegister::R15 => self.r15,
        };

        Ok(value)
    }

    /// Write a guest general-purpose register.
    ///
    /// `RSP` is written back to the backend's own stack field instead of this
    /// structure, for the same reason as [`Self::read_gpr`].
    #[inline]
    pub fn write_gpr(&mut self, reg: GeneralPurposeRegister, value: u64) -> VmResult<()> {
        match reg {
            GeneralPurposeRegister::Rax => self.rax = value,
            GeneralPurposeRegister::Rcx => self.rcx = value,
            GeneralPurposeRegister::Rdx => self.rdx = value,
            GeneralPurposeRegister::Rbx => self.rbx = value,
            GeneralPurposeRegister::Rsp => guest::Rsp::from_raw(value).write()?,
            GeneralPurposeRegister::Rbp => self.rbp = value,
            GeneralPurposeRegister::Rsi => self.rsi = value,
            GeneralPurposeRegister::Rdi => self.rdi = value,
            GeneralPurposeRegister::R8 => self.r8 = value,
            GeneralPurposeRegister::R9 => self.r9 = value,
            GeneralPurposeRegister::R10 => self.r10 = value,
            GeneralPurposeRegister::R11 => self.r11 = value,
            GeneralPurposeRegister::R12 => self.r12 = value,
            GeneralPurposeRegister::R13 => self.r13 = value,
            GeneralPurposeRegister::R14 => self.r14 = value,
            GeneralPurposeRegister::R15 => self.r15 = value,
        }

        Ok(())
    }
}

const _: () = {
    assert!(size_of::<GuestContext>() == 0x1F0);
    assert!(offset_of!(GuestContext, rax) == 0x00);
    assert!(offset_of!(GuestContext, r15) == 0x78);
    assert!(offset_of!(GuestContext, cr2) == 0x80);
    assert!(offset_of!(GuestContext, xmm0) == 0xC0);
    assert!(offset_of!(GuestContext, host_dr0) == 0x1C0);
    assert!(offset_of!(GuestContext, host_dr7) == 0x1E8);
};
