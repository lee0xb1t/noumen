//! Host-state area fields of the VMCS.
//!
//! Every field is wrapped in its own type with a `read`/`write` pair, mirroring
//! the layout of [`crate::arch::x86_64::vmcs::control`]. Field types are named
//! after their VMCS encoding without the `HOST_` prefix, so they are used as
//! `host::Cr0`, `host::Rip`, and so on.
//!
//! See Intel SDM Volume 3, Section 25.4.2 (Host-State Area) and Appendix B.1.3,
//! B.2.4, B.3.4 and B.4.4 for the corresponding field encodings.

use crate::arch::x86_64::{addr, cr, paging};
use crate::error::VmResult;
use x86_64::structures::gdt::SegmentSelector;

/////////////////// 16-bit host-state fields /////////////////////

/// Host ES selector.
#[repr(transparent)]
#[derive(Clone, Copy, Debug, Default)]
pub struct EsSelector(u16);

impl EsSelector {
    pub fn read() -> VmResult<Self> {
        let field = x86::vmx::vmcs::host::ES_SELECTOR;
        let value = unsafe { x86::bits64::vmx::vmread(field)? };

        Ok(Self(value as u16))
    }

    pub fn write(&self) -> VmResult<()> {
        let field = x86::vmx::vmcs::host::ES_SELECTOR;
        unsafe { x86::bits64::vmx::vmwrite(field, self.0 as u64)? };

        Ok(())
    }

    pub const fn from_raw(raw: u16) -> Self {
        Self(raw)
    }

    pub const fn from_selector(selector: SegmentSelector) -> Self {
        Self(selector.0)
    }

    pub const fn as_raw(&self) -> u16 {
        self.0
    }
}

/// Host CS selector.
#[repr(transparent)]
#[derive(Clone, Copy, Debug, Default)]
pub struct CsSelector(u16);

impl CsSelector {
    pub fn read() -> VmResult<Self> {
        let field = x86::vmx::vmcs::host::CS_SELECTOR;
        let value = unsafe { x86::bits64::vmx::vmread(field)? };

        Ok(Self(value as u16))
    }

    pub fn write(&self) -> VmResult<()> {
        let field = x86::vmx::vmcs::host::CS_SELECTOR;
        unsafe { x86::bits64::vmx::vmwrite(field, self.0 as u64)? };

        Ok(())
    }

    pub const fn from_raw(raw: u16) -> Self {
        Self(raw)
    }

    pub const fn from_selector(selector: SegmentSelector) -> Self {
        Self(selector.0)
    }

    pub const fn as_raw(&self) -> u16 {
        self.0
    }
}

/// Host SS selector.
#[repr(transparent)]
#[derive(Clone, Copy, Debug, Default)]
pub struct SsSelector(u16);

impl SsSelector {
    pub fn read() -> VmResult<Self> {
        let field = x86::vmx::vmcs::host::SS_SELECTOR;
        let value = unsafe { x86::bits64::vmx::vmread(field)? };

        Ok(Self(value as u16))
    }

    pub fn write(&self) -> VmResult<()> {
        let field = x86::vmx::vmcs::host::SS_SELECTOR;
        unsafe { x86::bits64::vmx::vmwrite(field, self.0 as u64)? };

        Ok(())
    }

    pub const fn from_raw(raw: u16) -> Self {
        Self(raw)
    }

    pub const fn from_selector(selector: SegmentSelector) -> Self {
        Self(selector.0)
    }

    pub const fn as_raw(&self) -> u16 {
        self.0
    }
}

/// Host DS selector.
#[repr(transparent)]
#[derive(Clone, Copy, Debug, Default)]
pub struct DsSelector(u16);

impl DsSelector {
    pub fn read() -> VmResult<Self> {
        let field = x86::vmx::vmcs::host::DS_SELECTOR;
        let value = unsafe { x86::bits64::vmx::vmread(field)? };

        Ok(Self(value as u16))
    }

    pub fn write(&self) -> VmResult<()> {
        let field = x86::vmx::vmcs::host::DS_SELECTOR;
        unsafe { x86::bits64::vmx::vmwrite(field, self.0 as u64)? };

        Ok(())
    }

    pub const fn from_raw(raw: u16) -> Self {
        Self(raw)
    }

    pub const fn from_selector(selector: SegmentSelector) -> Self {
        Self(selector.0)
    }

    pub const fn as_raw(&self) -> u16 {
        self.0
    }
}

/// Host FS selector.
#[repr(transparent)]
#[derive(Clone, Copy, Debug, Default)]
pub struct FsSelector(u16);

impl FsSelector {
    pub fn read() -> VmResult<Self> {
        let field = x86::vmx::vmcs::host::FS_SELECTOR;
        let value = unsafe { x86::bits64::vmx::vmread(field)? };

        Ok(Self(value as u16))
    }

    pub fn write(&self) -> VmResult<()> {
        let field = x86::vmx::vmcs::host::FS_SELECTOR;
        unsafe { x86::bits64::vmx::vmwrite(field, self.0 as u64)? };

        Ok(())
    }

    pub const fn from_raw(raw: u16) -> Self {
        Self(raw)
    }

    pub const fn from_selector(selector: SegmentSelector) -> Self {
        Self(selector.0)
    }

    pub const fn as_raw(&self) -> u16 {
        self.0
    }
}

/// Host GS selector.
#[repr(transparent)]
#[derive(Clone, Copy, Debug, Default)]
pub struct GsSelector(u16);

impl GsSelector {
    pub fn read() -> VmResult<Self> {
        let field = x86::vmx::vmcs::host::GS_SELECTOR;
        let value = unsafe { x86::bits64::vmx::vmread(field)? };

        Ok(Self(value as u16))
    }

    pub fn write(&self) -> VmResult<()> {
        let field = x86::vmx::vmcs::host::GS_SELECTOR;
        unsafe { x86::bits64::vmx::vmwrite(field, self.0 as u64)? };

        Ok(())
    }

    pub const fn from_raw(raw: u16) -> Self {
        Self(raw)
    }

    pub const fn from_selector(selector: SegmentSelector) -> Self {
        Self(selector.0)
    }

    pub const fn as_raw(&self) -> u16 {
        self.0
    }
}

/// Host TR selector.
#[repr(transparent)]
#[derive(Clone, Copy, Debug, Default)]
pub struct TrSelector(u16);

impl TrSelector {
    pub fn read() -> VmResult<Self> {
        let field = x86::vmx::vmcs::host::TR_SELECTOR;
        let value = unsafe { x86::bits64::vmx::vmread(field)? };

        Ok(Self(value as u16))
    }

    pub fn write(&self) -> VmResult<()> {
        let field = x86::vmx::vmcs::host::TR_SELECTOR;
        unsafe { x86::bits64::vmx::vmwrite(field, self.0 as u64)? };

        Ok(())
    }

    pub const fn from_raw(raw: u16) -> Self {
        Self(raw)
    }

    pub const fn from_selector(selector: SegmentSelector) -> Self {
        Self(selector.0)
    }

    pub const fn as_raw(&self) -> u16 {
        self.0
    }
}

/////////////////// 64-bit host-state fields /////////////////////

/// Host IA32_PAT.
#[repr(transparent)]
#[derive(Clone, Copy, Debug, Default)]
pub struct Ia32Pat(paging::PatRegister);

impl Ia32Pat {
    pub fn read() -> VmResult<Self> {
        let field = x86::vmx::vmcs::host::IA32_PAT_FULL;
        let value = unsafe { x86::bits64::vmx::vmread(field)? };

        Ok(Self::from_raw(value))
    }

    pub fn write(&self) -> VmResult<()> {
        let field = x86::vmx::vmcs::host::IA32_PAT_FULL;
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

/// Host IA32_EFER.
#[repr(transparent)]
#[derive(Clone, Copy, Debug, Default)]
pub struct Ia32Efer(u64);

impl Ia32Efer {
    pub fn read() -> VmResult<Self> {
        let field = x86::vmx::vmcs::host::IA32_EFER_FULL;
        let value = unsafe { x86::bits64::vmx::vmread(field)? };

        Ok(Self(value))
    }

    pub fn write(&self) -> VmResult<()> {
        let field = x86::vmx::vmcs::host::IA32_EFER_FULL;
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

/// Host IA32_PERF_GLOBAL_CTRL.
#[repr(transparent)]
#[derive(Clone, Copy, Debug, Default)]
pub struct Ia32PerfGlobalCtrl(u64);

impl Ia32PerfGlobalCtrl {
    pub fn read() -> VmResult<Self> {
        let field = x86::vmx::vmcs::host::IA32_PERF_GLOBAL_CTRL_FULL;
        let value = unsafe { x86::bits64::vmx::vmread(field)? };

        Ok(Self(value))
    }

    pub fn write(&self) -> VmResult<()> {
        let field = x86::vmx::vmcs::host::IA32_PERF_GLOBAL_CTRL_FULL;
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

/////////////////// 32-bit host-state field /////////////////////

/// Host IA32_SYSENTER_CS.
#[repr(transparent)]
#[derive(Clone, Copy, Debug, Default)]
pub struct Ia32SysenterCs(u32);

impl Ia32SysenterCs {
    pub fn read() -> VmResult<Self> {
        let field = x86::vmx::vmcs::host::IA32_SYSENTER_CS;
        let value = unsafe { x86::bits64::vmx::vmread(field)? };

        Ok(Self(value as u32))
    }

    pub fn write(&self) -> VmResult<()> {
        let field = x86::vmx::vmcs::host::IA32_SYSENTER_CS;
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

/////////////////// Natural-width host-state fields /////////////////////

/// Host CR0.
#[repr(transparent)]
#[derive(Clone, Copy, Debug, Default)]
pub struct Cr0(cr::Cr0);

impl Cr0 {
    pub fn read() -> VmResult<Self> {
        let field = x86::vmx::vmcs::host::CR0;
        let value = unsafe { x86::bits64::vmx::vmread(field)? };

        Ok(Self::from_raw(value))
    }

    pub fn write(&self) -> VmResult<()> {
        let field = x86::vmx::vmcs::host::CR0;
        unsafe { x86::bits64::vmx::vmwrite(field, self.as_raw())? };

        Ok(())
    }

    pub const fn from_bits(bits: u64) -> Self {
        Self(cr::Cr0::from_bits(bits))
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
}

/// Host CR3.
#[repr(transparent)]
#[derive(Clone, Copy, Debug, Default)]
pub struct Cr3(u64);

impl Cr3 {
    pub fn read() -> VmResult<Self> {
        let field = x86::vmx::vmcs::host::CR3;
        let value = unsafe { x86::bits64::vmx::vmread(field)? };

        Ok(Self(value))
    }

    pub fn write(&self) -> VmResult<()> {
        let field = x86::vmx::vmcs::host::CR3;
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

/// Host CR4.
#[repr(transparent)]
#[derive(Clone, Copy, Debug, Default)]
pub struct Cr4(cr::Cr4);

impl Cr4 {
    pub fn read() -> VmResult<Self> {
        let field = x86::vmx::vmcs::host::CR4;
        let value = unsafe { x86::bits64::vmx::vmread(field)? };

        Ok(Self::from_raw(value))
    }

    pub fn write(&self) -> VmResult<()> {
        let field = x86::vmx::vmcs::host::CR4;
        unsafe { x86::bits64::vmx::vmwrite(field, self.as_raw())? };

        Ok(())
    }

    pub const fn from_bits(bits: u64) -> Self {
        Self(cr::Cr4::from_bits(bits))
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
}

/// Host FS base.
#[repr(transparent)]
#[derive(Clone, Copy, Debug)]
pub struct FsBase(addr::VirtAddr);

impl Default for FsBase {
    fn default() -> Self {
        Self(addr::VirtAddr::null())
    }
}

impl FsBase {
    pub fn read() -> VmResult<Self> {
        let field = x86::vmx::vmcs::host::FS_BASE;
        let value = unsafe { x86::bits64::vmx::vmread(field)? };

        Ok(Self::from_raw(value))
    }

    pub fn write(&self) -> VmResult<()> {
        let field = x86::vmx::vmcs::host::FS_BASE;
        unsafe { x86::bits64::vmx::vmwrite(field, self.as_raw())? };

        Ok(())
    }

    pub const fn from_va(va: addr::VirtAddr) -> Self {
        Self(va)
    }

    pub const fn from_raw(raw: u64) -> Self {
        Self(addr::VirtAddr::new(raw))
    }

    pub const fn as_va(&self) -> addr::VirtAddr {
        self.0
    }

    pub const fn as_raw(&self) -> u64 {
        self.0.as_u64()
    }
}

/// Host GS base.
#[repr(transparent)]
#[derive(Clone, Copy, Debug)]
pub struct GsBase(addr::VirtAddr);

impl Default for GsBase {
    fn default() -> Self {
        Self(addr::VirtAddr::null())
    }
}

impl GsBase {
    pub fn read() -> VmResult<Self> {
        let field = x86::vmx::vmcs::host::GS_BASE;
        let value = unsafe { x86::bits64::vmx::vmread(field)? };

        Ok(Self::from_raw(value))
    }

    pub fn write(&self) -> VmResult<()> {
        let field = x86::vmx::vmcs::host::GS_BASE;
        unsafe { x86::bits64::vmx::vmwrite(field, self.as_raw())? };

        Ok(())
    }

    pub const fn from_va(va: addr::VirtAddr) -> Self {
        Self(va)
    }

    pub const fn from_raw(raw: u64) -> Self {
        Self(addr::VirtAddr::new(raw))
    }

    pub const fn as_va(&self) -> addr::VirtAddr {
        self.0
    }

    pub const fn as_raw(&self) -> u64 {
        self.0.as_u64()
    }
}

/// Host TR base.
#[repr(transparent)]
#[derive(Clone, Copy, Debug)]
pub struct TrBase(addr::VirtAddr);

impl Default for TrBase {
    fn default() -> Self {
        Self(addr::VirtAddr::null())
    }
}

impl TrBase {
    pub fn read() -> VmResult<Self> {
        let field = x86::vmx::vmcs::host::TR_BASE;
        let value = unsafe { x86::bits64::vmx::vmread(field)? };

        Ok(Self::from_raw(value))
    }

    pub fn write(&self) -> VmResult<()> {
        let field = x86::vmx::vmcs::host::TR_BASE;
        unsafe { x86::bits64::vmx::vmwrite(field, self.as_raw())? };

        Ok(())
    }

    pub const fn from_va(va: addr::VirtAddr) -> Self {
        Self(va)
    }

    pub const fn from_raw(raw: u64) -> Self {
        Self(addr::VirtAddr::new(raw))
    }

    pub const fn as_va(&self) -> addr::VirtAddr {
        self.0
    }

    pub const fn as_raw(&self) -> u64 {
        self.0.as_u64()
    }
}

/// Host GDTR base.
#[repr(transparent)]
#[derive(Clone, Copy, Debug)]
pub struct GdtrBase(addr::VirtAddr);

impl Default for GdtrBase {
    fn default() -> Self {
        Self(addr::VirtAddr::null())
    }
}

impl GdtrBase {
    pub fn read() -> VmResult<Self> {
        let field = x86::vmx::vmcs::host::GDTR_BASE;
        let value = unsafe { x86::bits64::vmx::vmread(field)? };

        Ok(Self::from_raw(value))
    }

    pub fn write(&self) -> VmResult<()> {
        let field = x86::vmx::vmcs::host::GDTR_BASE;
        unsafe { x86::bits64::vmx::vmwrite(field, self.as_raw())? };

        Ok(())
    }

    pub const fn from_va(va: addr::VirtAddr) -> Self {
        Self(va)
    }

    pub const fn from_raw(raw: u64) -> Self {
        Self(addr::VirtAddr::new(raw))
    }

    pub const fn as_va(&self) -> addr::VirtAddr {
        self.0
    }

    pub const fn as_raw(&self) -> u64 {
        self.0.as_u64()
    }
}

/// Host IDTR base.
#[repr(transparent)]
#[derive(Clone, Copy, Debug)]
pub struct IdtrBase(addr::VirtAddr);

impl Default for IdtrBase {
    fn default() -> Self {
        Self(addr::VirtAddr::null())
    }
}

impl IdtrBase {
    pub fn read() -> VmResult<Self> {
        let field = x86::vmx::vmcs::host::IDTR_BASE;
        let value = unsafe { x86::bits64::vmx::vmread(field)? };

        Ok(Self::from_raw(value))
    }

    pub fn write(&self) -> VmResult<()> {
        let field = x86::vmx::vmcs::host::IDTR_BASE;
        unsafe { x86::bits64::vmx::vmwrite(field, self.as_raw())? };

        Ok(())
    }

    pub const fn from_va(va: addr::VirtAddr) -> Self {
        Self(va)
    }

    pub const fn from_raw(raw: u64) -> Self {
        Self(addr::VirtAddr::new(raw))
    }

    pub const fn as_va(&self) -> addr::VirtAddr {
        self.0
    }

    pub const fn as_raw(&self) -> u64 {
        self.0.as_u64()
    }
}

/// Host IA32_SYSENTER_ESP.
#[repr(transparent)]
#[derive(Clone, Copy, Debug, Default)]
pub struct Ia32SysenterEsp(u64);

impl Ia32SysenterEsp {
    pub fn read() -> VmResult<Self> {
        let field = x86::vmx::vmcs::host::IA32_SYSENTER_ESP;
        let value = unsafe { x86::bits64::vmx::vmread(field)? };

        Ok(Self(value))
    }

    pub fn write(&self) -> VmResult<()> {
        let field = x86::vmx::vmcs::host::IA32_SYSENTER_ESP;
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

/// Host IA32_SYSENTER_EIP.
#[repr(transparent)]
#[derive(Clone, Copy, Debug, Default)]
pub struct Ia32SysenterEip(u64);

impl Ia32SysenterEip {
    pub fn read() -> VmResult<Self> {
        let field = x86::vmx::vmcs::host::IA32_SYSENTER_EIP;
        let value = unsafe { x86::bits64::vmx::vmread(field)? };

        Ok(Self(value))
    }

    pub fn write(&self) -> VmResult<()> {
        let field = x86::vmx::vmcs::host::IA32_SYSENTER_EIP;
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

/// Host RSP.
#[repr(transparent)]
#[derive(Clone, Copy, Debug)]
pub struct Rsp(addr::VirtAddr);

impl Default for Rsp {
    fn default() -> Self {
        Self(addr::VirtAddr::null())
    }
}

impl Rsp {
    pub fn read() -> VmResult<Self> {
        let field = x86::vmx::vmcs::host::RSP;
        let value = unsafe { x86::bits64::vmx::vmread(field)? };

        Ok(Self::from_raw(value))
    }

    pub fn write(&self) -> VmResult<()> {
        let field = x86::vmx::vmcs::host::RSP;
        unsafe { x86::bits64::vmx::vmwrite(field, self.as_raw())? };

        Ok(())
    }

    pub const fn from_va(va: addr::VirtAddr) -> Self {
        Self(va)
    }

    pub const fn from_raw(raw: u64) -> Self {
        Self(addr::VirtAddr::new(raw))
    }

    pub const fn as_va(&self) -> addr::VirtAddr {
        self.0
    }

    pub const fn as_raw(&self) -> u64 {
        self.0.as_u64()
    }
}

/// Host RIP.
#[repr(transparent)]
#[derive(Clone, Copy, Debug)]
pub struct Rip(addr::VirtAddr);

impl Default for Rip {
    fn default() -> Self {
        Self(addr::VirtAddr::null())
    }
}

impl Rip {
    pub fn read() -> VmResult<Self> {
        let field = x86::vmx::vmcs::host::RIP;
        let value = unsafe { x86::bits64::vmx::vmread(field)? };

        Ok(Self::from_raw(value))
    }

    pub fn write(&self) -> VmResult<()> {
        let field = x86::vmx::vmcs::host::RIP;
        unsafe { x86::bits64::vmx::vmwrite(field, self.as_raw())? };

        Ok(())
    }

    pub const fn from_va(va: addr::VirtAddr) -> Self {
        Self(va)
    }

    pub const fn from_raw(raw: u64) -> Self {
        Self(addr::VirtAddr::new(raw))
    }

    pub const fn as_va(&self) -> addr::VirtAddr {
        self.0
    }

    pub const fn as_raw(&self) -> u64 {
        self.0.as_u64()
    }
}

/// CET host-state field encodings (SDM B.4.3).
macro_rules! host_cet_field {
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

host_cet_field!(
    /// Host IA32_S_CET.
    Ia32SCet,
    0x0000_6c18
);

host_cet_field!(
    /// Host SSP.
    Ssp,
    0x0000_6c1a
);

host_cet_field!(
    /// Host IA32_INTERRUPT_SSP_TABLE_ADDR.
    Ia32InterruptSspTable,
    0x0000_6c1c
);
