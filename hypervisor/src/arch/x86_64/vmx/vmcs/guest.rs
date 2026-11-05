//! Guest-state area fields of the VMCS.
//!
//! Every field is wrapped in its own type with a `read`/`write` pair, mirroring
//! the layout of [`crate::arch::x86_64::vmcs::control`] and
//! [`crate::arch::x86_64::vmcs::host`]. Field types are named after their VMCS
//! encoding without the `GUEST_` prefix, so they are used as `guest::Rip`,
//! `guest::CsSelector`, and so on.
//!
//! See Intel SDM Volume 3, Section 25.4.1 (Guest-State Area) and Appendix B.1.2,
//! B.2.3, B.3.3 and B.4.3 for the corresponding field encodings.

use crate::arch::x86_64::vmx::vmcs::control;
use crate::arch::x86_64::{cr, paging, segment};
use crate::error::{VmFail, VmResult};

//////////// 16-bit guest-state fields ////////////

macro_rules! guest_selector {
    ($(#[$meta:meta])* $name:ident, $field:ident) => {
        $(#[$meta])*
        #[repr(transparent)]
        #[derive(Clone, Copy, Debug, Default)]
        pub struct $name(u16);

        impl $name {
            pub fn read() -> VmResult<Self> {
                let field = x86::vmx::vmcs::guest::$field;
                let value = unsafe { x86::bits64::vmx::vmread(field)? };

                Ok(Self(value as u16))
            }

            pub fn write(&self) -> VmResult<()> {
                let field = x86::vmx::vmcs::guest::$field;
                unsafe { x86::bits64::vmx::vmwrite(field, self.0 as u64)? };

                Ok(())
            }

            pub const fn from_raw(raw: u16) -> Self {
                Self(raw)
            }

            pub fn from_selector(selector: &segment::SegmentSelector) -> Self {
                Self(selector.raw())
            }

            pub const fn as_raw(&self) -> u16 {
                self.0
            }
        }
    };
}

guest_selector!(
    /// Guest ES selector.
    EsSelector,
    ES_SELECTOR
);

guest_selector!(
    /// Guest CS selector.
    CsSelector,
    CS_SELECTOR
);

guest_selector!(
    /// Guest SS selector.
    SsSelector,
    SS_SELECTOR
);

guest_selector!(
    /// Guest DS selector.
    DsSelector,
    DS_SELECTOR
);

guest_selector!(
    /// Guest FS selector.
    FsSelector,
    FS_SELECTOR
);

guest_selector!(
    /// Guest GS selector.
    GsSelector,
    GS_SELECTOR
);

guest_selector!(
    /// Guest LDTR selector.
    LdtrSelector,
    LDTR_SELECTOR
);

guest_selector!(
    /// Guest TR selector.
    TrSelector,
    TR_SELECTOR
);

//////////// 64-bit guest-state fields ////////////

macro_rules! guest_u64_field {
    ($(#[$meta:meta])* $name:ident, $field:ident) => {
        $(#[$meta])*
        #[repr(transparent)]
        #[derive(Clone, Copy, Debug, Default)]
        pub struct $name(u64);

        impl $name {
            pub fn read() -> VmResult<Self> {
                let field = x86::vmx::vmcs::guest::$field;
                let value = unsafe { x86::bits64::vmx::vmread(field)? };

                Ok(Self(value))
            }

            pub fn write(&self) -> VmResult<()> {
                let field = x86::vmx::vmcs::guest::$field;
                unsafe { x86::bits64::vmx::vmwrite(field, self.0)? };

                Ok(())
            }

            pub const fn from_raw(raw: u64) -> Self {
                Self(raw)
            }

            pub const fn as_raw(&self) -> u64 {
                self.0
            }
        }
    };
}

bitfield::bitfield! {
    #[derive(Clone, Copy, Debug, Default)]
    pub struct Ia32DebugctlRegister(u64);

    pub lbr, set_lbr: 0;
    pub btf, set_btf: 1;
    reserved1, set_reserved1: 5, 2;
    pub tr, set_tr: 6;
    pub bts, set_bts: 7;
    pub btint, set_btint: 8;
    pub bts_off_os, set_bts_off_os: 9;
    pub bts_off_usr, set_bts_off_usr: 10;
    pub freeze_lbrs_on_pmi, set_freeze_lbrs_on_pmi: 11;
    pub freeze_perfmon_on_pmi, set_freeze_perfmon_on_pmi: 12;
    pub enable_uncore_pmi, set_enable_uncore_pmi: 13;
    pub freeze_while_smm, set_freeze_while_smm: 14;
    pub rtm_debug, set_rtm_debug: 15;
    reserved2, set_reserved2: 63, 16;
}

impl Ia32DebugctlRegister {
    pub const fn from_raw(raw: u64) -> Self {
        Self(raw)
    }

    pub fn read() -> VmResult<Self> {
        let field = x86::vmx::vmcs::guest::IA32_DEBUGCTL_FULL;
        let value = unsafe { x86::bits64::vmx::vmread(field)? };

        Ok(Self(value))
    }

    pub fn write(&self) -> VmResult<()> {
        let field = x86::vmx::vmcs::guest::IA32_DEBUGCTL_FULL;
        unsafe { x86::bits64::vmx::vmwrite(field, self.0)? };

        Ok(())
    }

    #[inline]
    pub const fn as_raw(&self) -> u64 {
        self.0
    }
}

/// Guest IA32_PAT.
#[repr(transparent)]
#[derive(Clone, Copy, Debug, Default)]
pub struct Ia32Pat(paging::PatRegister);

impl Ia32Pat {
    pub fn read() -> VmResult<Self> {
        let field = x86::vmx::vmcs::guest::IA32_PAT_FULL;
        let value = unsafe { x86::bits64::vmx::vmread(field)? };

        Ok(Self::from_raw(value))
    }

    pub fn write(&self) -> VmResult<()> {
        let field = x86::vmx::vmcs::guest::IA32_PAT_FULL;
        unsafe { x86::bits64::vmx::vmwrite(field, self.as_raw())? };

        Ok(())
    }

    pub const fn from_pat(pat: paging::PatRegister) -> Self {
        Self(pat)
    }

    pub const fn from_raw(raw: u64) -> Self {
        Self(paging::PatRegister::from_raw(raw))
    }

    pub const fn as_pat(&self) -> paging::PatRegister {
        self.0
    }

    pub const fn as_raw(&self) -> u64 {
        self.0.as_raw()
    }
}

guest_u64_field!(
    /// Guest IA32_EFER.
    Ia32Efer,
    IA32_EFER_FULL
);

guest_u64_field!(
    /// Guest IA32_PERF_GLOBAL_CTRL.
    Ia32PerfGlobalCtrl,
    IA32_PERF_GLOBAL_CTRL_FULL
);

guest_u64_field!(
    /// VMCS link pointer.
    LinkPointer,
    LINK_PTR_FULL
);

//////////// 32-bit guest-state fields ////////////

macro_rules! guest_u32_field {
    ($(#[$meta:meta])* $name:ident, $field:ident) => {
        $(#[$meta])*
        #[repr(transparent)]
        #[derive(Clone, Copy, Debug, Default)]
        pub struct $name(u32);

        impl $name {
            pub fn read() -> VmResult<Self> {
                let field = x86::vmx::vmcs::guest::$field;
                let value = unsafe { x86::bits64::vmx::vmread(field)? };

                Ok(Self(value as u32))
            }

            pub fn write(&self) -> VmResult<()> {
                let field = x86::vmx::vmcs::guest::$field;
                unsafe { x86::bits64::vmx::vmwrite(field, self.0 as u64)? };

                Ok(())
            }

            pub const fn from_raw(raw: u32) -> Self {
                Self(raw)
            }

            pub const fn as_raw(&self) -> u32 {
                self.0
            }
        }
    };
}

guest_u32_field!(
    /// Guest ES limit.
    EsLimit,
    ES_LIMIT
);

guest_u32_field!(
    /// Guest CS limit.
    CsLimit,
    CS_LIMIT
);

guest_u32_field!(
    /// Guest SS limit.
    SsLimit,
    SS_LIMIT
);

guest_u32_field!(
    /// Guest DS limit.
    DsLimit,
    DS_LIMIT
);

guest_u32_field!(
    /// Guest FS limit.
    FsLimit,
    FS_LIMIT
);

guest_u32_field!(
    /// Guest GS limit.
    GsLimit,
    GS_LIMIT
);

guest_u32_field!(
    /// Guest LDTR limit.
    LdtrLimit,
    LDTR_LIMIT
);

guest_u32_field!(
    /// Guest TR limit.
    TrLimit,
    TR_LIMIT
);

guest_u32_field!(
    /// Guest GDTR limit.
    GdtrLimit,
    GDTR_LIMIT
);

guest_u32_field!(
    /// Guest IDTR limit.
    IdtrLimit,
    IDTR_LIMIT
);

macro_rules! guest_access_rights {
    ($(#[$meta:meta])* $name:ident, $field:ident) => {
        bitfield::bitfield! {
            $(#[$meta])*
            #[repr(transparent)]
            #[derive(Clone, Copy, Debug, Default)]
            pub struct $name(u32);

            pub u32, type_, _: 3, 0;
            pub u32, descriptor_type, _: 4;
            pub u32, descriptor_privilege_level, _: 6, 5;
            pub u32, present, _: 7;
            pub u32, reserved1, _: 11, 8;
            pub u32, available_bit, _: 12;
            pub u32, long_mode, _: 13;
            pub u32, default_big, _: 14;
            pub u32, granularity, _: 15;
            pub u32, unusable, _: 16;
            pub u32, reserved2, _: 31, 17;
        }

        impl $name {
            pub fn read() -> VmResult<Self> {
                let field = x86::vmx::vmcs::guest::$field;
                let value = unsafe { x86::bits64::vmx::vmread(field)? };

                Ok(Self(value as u32))
            }

            pub fn write(&self) -> VmResult<()> {
                let field = x86::vmx::vmcs::guest::$field;
                unsafe { x86::bits64::vmx::vmwrite(field, self.0 as u64)? };

                Ok(())
            }

            pub const fn from_raw(raw: u32) -> Self {
                Self(raw)
            }

            pub const fn as_raw(&self) -> u32 {
                self.0
            }
        }
    };
}

guest_access_rights!(
    /// Guest ES access rights.
    EsAccessRights,
    ES_ACCESS_RIGHTS
);

guest_access_rights!(
    /// Guest CS access rights.
    CsAccessRights,
    CS_ACCESS_RIGHTS
);

guest_access_rights!(
    /// Guest SS access rights.
    SsAccessRights,
    SS_ACCESS_RIGHTS
);

guest_access_rights!(
    /// Guest DS access rights.
    DsAccessRights,
    DS_ACCESS_RIGHTS
);

guest_access_rights!(
    /// Guest FS access rights.
    FsAccessRights,
    FS_ACCESS_RIGHTS
);

guest_access_rights!(
    /// Guest GS access rights.
    GsAccessRights,
    GS_ACCESS_RIGHTS
);

guest_access_rights!(
    /// Guest LDTR access rights.
    LdtrAccessRights,
    LDTR_ACCESS_RIGHTS
);

guest_access_rights!(
    /// Guest TR access rights.
    TrAccessRights,
    TR_ACCESS_RIGHTS
);

bitfield::bitfield! {
    #[derive(Clone, Copy, Debug, Default)]
    pub struct VmxInterruptibilityState(u32);

    pub u32, blocking_by_sti, set_blocking_by_sti: 0;
    pub u32, blocking_by_mov_ss, set_blocking_by_mov_ss: 1;
    pub u32, blocking_by_smi, set_blocking_by_smi: 2;
    pub u32, blocking_by_nmi, set_blocking_by_nmi: 3;
    pub u32, enclave_interruption, set_enclave_interruption: 4;
    pub u32, reserved1, set_reserved1: 31, 5;
}

impl VmxInterruptibilityState {
    pub const fn new() -> Self {
        Self(0)
    }

    pub fn from_u32(v: u32) -> Self {
        Self(v)
    }

    pub const fn from_raw(raw: u32) -> Self {
        Self(raw)
    }

    pub fn read() -> VmResult<Self> {
        let field = x86::vmx::vmcs::guest::INTERRUPTIBILITY_STATE;
        let vmread = unsafe { x86::bits64::vmx::vmread(field) };

        if let Err(e) = vmread {
            match e {
                x86::vmx::VmFail::VmFailInvalid => {
                    return Err(VmFail::VmFailInvalid);
                }
                x86::vmx::VmFail::VmFailValid => {
                    return Err(VmFail::VmFailValid);
                }
            }
        }

        Ok(Self(vmread.unwrap() as u32))
    }

    pub fn write(&self) -> VmResult<()> {
        let field = x86::vmx::vmcs::guest::INTERRUPTIBILITY_STATE;
        let vmwrite = unsafe { x86::bits64::vmx::vmwrite(field, self.0 as u64) };
        if let Err(e) = vmwrite {
            match e {
                x86::vmx::VmFail::VmFailInvalid => {
                    return Err(VmFail::VmFailInvalid);
                }
                x86::vmx::VmFail::VmFailValid => {
                    return Err(VmFail::VmFailValid);
                }
            }
        }

        Ok(())
    }

    #[inline]
    pub fn as_u32(&self) -> u32 {
        self.0
    }

    #[inline]
    pub const fn as_raw(&self) -> u32 {
        self.0
    }
}

guest_u32_field!(
    /// Guest activity state (0: active, 1: HLT, 2: shutdown, 3: wait-for-SIPI).
    ActivityState,
    ACTIVITY_STATE
);

guest_u32_field!(
    /// Guest IA32_SYSENTER_CS.
    Ia32SysenterCs,
    IA32_SYSENTER_CS
);

//////////// Natural-width guest-state fields ////////////

guest_u64_field!(
    /// Guest ES base.
    EsBase,
    ES_BASE
);

guest_u64_field!(
    /// Guest CS base.
    CsBase,
    CS_BASE
);

guest_u64_field!(
    /// Guest SS base.
    SsBase,
    SS_BASE
);

guest_u64_field!(
    /// Guest DS base.
    DsBase,
    DS_BASE
);

guest_u64_field!(
    /// Guest FS base.
    FsBase,
    FS_BASE
);

guest_u64_field!(
    /// Guest GS base.
    GsBase,
    GS_BASE
);

guest_u64_field!(
    /// Guest LDTR base.
    LdtrBase,
    LDTR_BASE
);

guest_u64_field!(
    /// Guest TR base.
    TrBase,
    TR_BASE
);

guest_u64_field!(
    /// Guest GDTR base.
    GdtrBase,
    GDTR_BASE
);

guest_u64_field!(
    /// Guest IDTR base.
    IdtrBase,
    IDTR_BASE
);

guest_u64_field!(
    /// Guest CR3.
    Cr3,
    CR3
);

impl Cr3 {
    pub fn set_raw(&mut self, raw: u64) {
        self.0 = raw;
    }
}

guest_u64_field!(
    /// Guest DR7.
    Dr7,
    DR7
);

guest_u64_field!(
    /// Guest IA32_SYSENTER_ESP.
    Ia32SysenterEsp,
    IA32_SYSENTER_ESP
);

guest_u64_field!(
    /// Guest IA32_SYSENTER_EIP.
    Ia32SysenterEip,
    IA32_SYSENTER_EIP
);

/// CET guest-state field encodings (SDM B.3.3).
macro_rules! guest_cet_field {
    ($(#[$meta:meta])* $name:ident, $field:expr) => {
        $(#[$meta])*
        #[repr(transparent)]
        #[derive(Clone, Copy, Debug, Default)]
        pub struct $name(u64);

        impl $name {
            pub fn read() -> VmResult<Self> {
                let value = unsafe { x86::bits64::vmx::vmread($field)? };

                Ok(Self(value))
            }

            pub fn write(&self) -> VmResult<()> {
                unsafe { x86::bits64::vmx::vmwrite($field, self.0)? };

                Ok(())
            }

            pub const fn from_raw(raw: u64) -> Self {
                Self(raw)
            }

            pub const fn as_raw(&self) -> u64 {
                self.0
            }
        }
    };
}

guest_cet_field!(
    /// Guest IA32_S_CET.
    Ia32SCet,
    0x0000_6828
);

guest_cet_field!(
    /// Guest SSP.
    Ssp,
    0x0000_682a
);

guest_cet_field!(
    /// Guest IA32_INTERRUPT_SSP_TABLE_ADDR.
    Ia32InterruptSspTable,
    0x0000_682c
);

#[repr(transparent)]
#[derive(Clone, Copy, Default, Debug)]
pub struct Cr0(cr::Cr0);

impl Cr0 {
    pub const fn new(flags: u64) -> Self {
        Self(cr::Cr0::from_bits(flags))
    }
    pub const fn from_bits(flags: u64) -> Self {
        Self(cr::Cr0::from_bits(flags))
    }

    pub const fn from_raw(raw: u64) -> Self {
        Self::from_bits(raw)
    }

    pub const fn bits(&self) -> u64 {
        self.0.bits()
    }

    pub const fn as_raw(&self) -> u64 {
        self.0.bits()
    }

    pub fn read() -> VmResult<Self> {
        let field = x86::vmx::vmcs::guest::CR0;
        let value = unsafe { x86::bits64::vmx::vmread(field)? };

        Ok(Self(cr::Cr0(value)))
    }

    pub fn read_effective() -> VmResult<Self> {
        let cr0_ctrl = control::Cr0Shadow::read()?;
        let guest_cr0 = Self::read()?;

        let mask = cr0_ctrl.raw_mask();
        let shadow = cr0_ctrl.raw_shadow();

        let cr0 = (shadow & mask) | (guest_cr0.bits() & !mask);

        Ok(Self(cr::Cr0(cr0)))
    }

    pub fn write(&self) -> VmResult<()> {
        let field = x86::vmx::vmcs::guest::CR0;
        unsafe { x86::bits64::vmx::vmwrite(field, self.as_raw())? };

        Ok(())
    }
}

impl core::ops::Deref for Cr0 {
    type Target = cr::Cr0;
    fn deref(&self) -> &Self::Target {
        &self.0
    }
}

impl core::ops::DerefMut for Cr0 {
    fn deref_mut(&mut self) -> &mut Self::Target {
        &mut self.0
    }
}

#[repr(transparent)]
#[derive(Clone, Copy, Default, Debug)]
pub struct Cr4(cr::Cr4);

impl Cr4 {
    pub const fn new(v: u64) -> Self {
        Self(cr::Cr4::from_bits(v))
    }
    pub const fn from_bits(flags: u64) -> Self {
        Self(cr::Cr4::from_bits(flags))
    }

    pub const fn from_raw(raw: u64) -> Self {
        Self::from_bits(raw)
    }

    pub const fn bits(&self) -> u64 {
        self.0.bits()
    }

    pub const fn as_raw(&self) -> u64 {
        self.0.bits()
    }

    pub fn read() -> VmResult<Self> {
        let field = x86::vmx::vmcs::guest::CR4;
        let value = unsafe { x86::bits64::vmx::vmread(field)? };

        Ok(Self(cr::Cr4(value)))
    }

    pub fn read_effective() -> VmResult<Self> {
        let cr4_ctrl = control::Cr4Shadow::read()?;
        let guest_cr4 = Self::read()?;

        let mask = cr4_ctrl.raw_mask();
        let shadow = cr4_ctrl.raw_shadow();

        let cr4 = (shadow & mask) | (guest_cr4.bits() & !mask);

        Ok(Self(cr::Cr4(cr4)))
    }

    pub fn write(&self) -> VmResult<()> {
        let field = x86::vmx::vmcs::guest::CR4;
        unsafe { x86::bits64::vmx::vmwrite(field, self.as_raw())? };

        Ok(())
    }
}

impl core::ops::Deref for Cr4 {
    type Target = cr::Cr4;
    fn deref(&self) -> &Self::Target {
        &self.0
    }
}

impl core::ops::DerefMut for Cr4 {
    fn deref_mut(&mut self) -> &mut Self::Target {
        &mut self.0
    }
}

#[repr(transparent)]
#[derive(Copy, Clone, Debug, Default)]
pub struct Rip(u64);

impl Rip {
    pub fn from_raw(v: u64) -> Self {
        Self(v)
    }

    pub fn read() -> VmResult<Self> {
        let field = x86::vmx::vmcs::guest::RIP;
        let value = unsafe { x86::bits64::vmx::vmread(field)? };

        Ok(Self(value))
    }

    pub fn write(&self) -> VmResult<()> {
        let field = x86::vmx::vmcs::guest::RIP;
        unsafe { x86::bits64::vmx::vmwrite(field, self.0)? };

        Ok(())
    }

    #[inline]
    pub fn as_raw(&self) -> u64 {
        self.0
    }
}

#[repr(transparent)]
#[derive(Copy, Clone, Debug, Default)]
pub struct Rsp(u64);

impl Rsp {
    pub fn from_raw(v: u64) -> Self {
        Self(v)
    }

    pub fn read() -> VmResult<Self> {
        let field = x86::vmx::vmcs::guest::RSP;
        let value = unsafe { x86::bits64::vmx::vmread(field)? };

        Ok(Self(value))
    }

    pub fn write(&self) -> VmResult<()> {
        let field = x86::vmx::vmcs::guest::RSP;
        unsafe { x86::bits64::vmx::vmwrite(field, self.0)? };

        Ok(())
    }

    #[inline]
    pub fn as_raw(&self) -> u64 {
        self.0
    }
}

bitfield::bitfield! {
    #[derive(Clone, Copy, Debug, Default)]
    pub struct Rflags(u64);

    pub carry_flag, set_carry_flag: 0;
    pub read_as_1, set_read_as_1: 1;
    pub parity_flag, set_parity_flag: 2;
    // reserved1: bit 3
    pub auxiliary_carry_flag, set_auxiliary_carry_flag: 4;
    // reserved2: bit 5
    pub zero_flag, set_zero_flag: 6;
    pub sign_flag, set_sign_flag: 7;
    pub trap_flag, set_trap_flag: 8;
    pub interrupt_enable_flag, set_interrupt_enable_flag: 9;
    pub direction_flag, set_direction_flag: 10;
    pub overflow_flag, set_overflow_flag: 11;
    pub io_privilege_level, set_io_privilege_level: 13, 12;
    pub nested_task_flag, set_nested_task_flag: 14;
    // reserved3: bit 15
    pub resume_flag, set_resume_flag: 16;
    pub virtual_8086_mode_flag, set_virtual_8086_mode_flag: 17;
    pub alignment_check_flag, set_alignment_check_flag: 18;
    pub virtual_interrupt_flag, set_virtual_interrupt_flag: 19;
    pub virtual_interrupt_pending_flag, set_virtual_interrupt_pending_flag: 20;
    pub identification_flag, set_identification_flag: 21;
    // reserved4: bits 22-63
}

impl Rflags {
    pub const fn from_raw(raw: u64) -> Self {
        Self(raw)
    }

    pub fn read() -> VmResult<Self> {
        let field = x86::vmx::vmcs::guest::RFLAGS;
        let value = unsafe { x86::bits64::vmx::vmread(field)? };

        Ok(Self(value))
    }

    pub fn write(&self) -> VmResult<()> {
        let field = x86::vmx::vmcs::guest::RFLAGS;
        unsafe { x86::bits64::vmx::vmwrite(field, self.0)? };

        Ok(())
    }

    #[inline]
    pub const fn as_raw(&self) -> u64 {
        self.0
    }
}

bitfield::bitfield! {
    /// VMX pending debug exceptions
    #[derive(Clone, Copy, Debug, Default)]
    pub struct VmxPendingDebugExceptions(u64);

    pub u8, b0, set_b0: 0;
    pub u8, b1, set_b1: 1;
    pub u8, b2, set_b2: 2;
    pub u8, b3, set_b3: 3;
    pub u8, enabled_breakpoint, set_enabled_breakpoint: 12;
    pub u8, bs, set_bs: 14;
    pub u8, rtm, set_rtm: 16;
}

impl VmxPendingDebugExceptions {
    pub const fn from_raw(raw: u64) -> Self {
        Self(raw)
    }

    pub fn read() -> VmResult<Self> {
        let field = x86::vmx::vmcs::guest::PENDING_DBG_EXCEPTIONS;
        let value = unsafe { x86::bits64::vmx::vmread(field)? };

        Ok(Self(value))
    }

    pub fn write(&self) -> VmResult<()> {
        let field = x86::vmx::vmcs::guest::PENDING_DBG_EXCEPTIONS;
        unsafe { x86::bits64::vmx::vmwrite(field, self.0)? };

        Ok(())
    }

    #[inline]
    pub const fn as_raw(&self) -> u64 {
        self.0
    }
}

