//! Wrappers around the VMX instruction set.
//!

use core::arch::asm;

use crate::arch::x86_64::addr::PhysAddr;
use crate::error::VmResult;

pub fn vmxon(phys: &PhysAddr) -> VmResult<()> {
    unsafe { x86::bits64::vmx::vmxon(phys.as_u64())? };
    Ok(())
}

pub fn vmxoff() -> VmResult<()> {
    unsafe { x86::bits64::vmx::vmxoff()? };
    Ok(())
}

pub fn vmclear(phys: &PhysAddr) -> VmResult<()> {
    unsafe { x86::bits64::vmx::vmclear(phys.as_u64())? };
    Ok(())
}

pub fn vmptrld(phys: &PhysAddr) -> VmResult<()> {
    unsafe { x86::bits64::vmx::vmptrld(phys.as_u64())? };
    Ok(())
}

/// Type of invalidation performed by `INVEPT`.
#[repr(u32)]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum InveptType {
    /// Invalidate translations derived from a single EPTP.
    SingleContext = 0,
    /// Invalidate all translations derived from EPT.
    AllContexts = 1,
}

impl InveptType {
    pub const fn as_u32(self) -> u32 {
        self as u32
    }
}

/// Operand of the `INVEPT` instruction.
#[allow(dead_code)] // layout is consumed by the INVEPT stub
pub struct InveptDescriptor {
    pub ept_pointer: u64,
    pub reserved: u64,
}

impl InveptDescriptor {
    pub const fn from_eptp(ept_pointer: u64) -> Self {
        Self {
            ept_pointer,
            reserved: 0,
        }
    }
}

/// `INVEPT` — invalidate cached EPT translations.
#[repr(C)]
pub struct Invept {
    typ: InveptType,
    pub eptp: u64,
}

impl Invept {
    pub const fn new(typ: InveptType, eptp: u64) -> Self {
        Self { typ, eptp }
    }

    /// `INVEPT`。返回是否成功（CF = 0）。
    ///
    /// IMPORTANT: INVEPT/INVVPID **会写标志位**，所以不能用
    /// `options(preserves_flags)` —— 那等于骗编译器「标志没变」，可能让它
    /// 复用失效的条件码。失败是静默的（CF=1，不做任何失效），必须自己看。
    pub fn invept(&self) -> bool {
        let desc = InveptDescriptor::from_eptp(self.eptp);
        let ok: u8;

        unsafe {
            asm!(
                "invept rcx, [rdx]",
                "setnc {ok}",
                in("rcx") self.typ.as_u32(),
                in("rdx") &desc as *const _,
                ok = out(reg_byte) ok,
                options(nostack),
            );
        }

        ok != 0
    }

    pub fn all(eptp: u64) -> bool {
        Self::new(InveptType::AllContexts, eptp).invept()
    }

    #[allow(dead_code)] // single-context INVEPT is not used by this hypervisor
    pub fn single(eptp: u64) -> bool {
        Self::new(InveptType::SingleContext, eptp).invept()
    }
}

#[repr(u32)]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum InvvpidType {
    SingleContext = 1,
    #[allow(dead_code)]
    AllContext = 2,
    SingleContextRetainingGlobals = 3,
}

impl InvvpidType {
    pub const fn as_u32(self) -> u32 {
        self as u32
    }
}

#[repr(C)]
pub struct InvvpidDescriptor {
    pub vpid: u16,
    pub reserved1: u16,
    pub reserved2: u32,
    pub linear_address: u64,
}

impl InvvpidDescriptor {
    pub const fn new(vpid: u16, linear_address: u64) -> Self {
        Self {
            vpid,
            reserved1: 0,
            reserved2: 0,
            linear_address,
        }
    }
}

#[repr(C)]
pub struct Invvpid {
    typ: InvvpidType,
    desc: InvvpidDescriptor,
}

impl Invvpid {
    pub fn new(typ: InvvpidType, vpid: u16, linear_address: u64) -> Self {
        Self {
            typ,
            desc: InvvpidDescriptor::new(vpid, linear_address),
        }
    }
    /// `INVVPID`。返回是否成功（CF = 0）；同 [`Invept::invept`]，不能声明
    /// `preserves_flags`。
    pub fn invvpid(&self) -> bool {
        let ok: u8;

        unsafe {
            asm!(
                "invvpid rcx, [rdx]",
                "setnc {ok}",
                in("rcx") self.typ.as_u32(),
                in("rdx") &self.desc as *const _,
                ok = out(reg_byte) ok,
                options(nostack),
            );
        }

        ok != 0
    }
}

#[repr(transparent)]
pub struct InstructionError(u64);

// Architectural VMX instruction-error codes; only some are handled here.
#[allow(dead_code)]
impl InstructionError {
    /// VMCALL executed in VMX root operation.
    pub const VMCALL_IN_VMX_ROOT_OPERATION: u32 = 0x00000001;

    /// VMCLEAR with invalid physical address.
    pub const VMCLEAR_INVALID_PHYSICAL_ADDRESS: u32 = 0x00000002;

    /// VMCLEAR with VMXON pointer.
    pub const VMCLEAR_INVALID_VMXON_POINTER: u32 = 0x00000003;

    /// VMLAUNCH with non-clear VMCS.
    pub const VMLAUCH_NON_CLEAR_VMCS: u32 = 0x00000004;

    /// VMRESUME with non-launched VMCS.
    pub const VMRESUME_NON_LAUNCHED_VMCS: u32 = 0x00000005;

    /// VMRESUME after VMXOFF (VMXOFF and VMXON between VMLAUNCH and VMRESUME).
    pub const VMRESUME_AFTER_VMXOFF: u32 = 0x00000006;

    /// VM entry with invalid control field(s).
    pub const VMENTRY_INVALID_CONTROL_FIELDS: u32 = 0x00000007;

    /// VM entry with invalid host-state field(s).
    pub const VMENTRY_INVALID_HOST_STATE: u32 = 0x00000008;

    /// VMPTRLD with invalid physical address.
    pub const VMPTRLD_INVALID_PHYSICAL_ADDRESS: u32 = 0x00000009;

    /// VMPTRLD with VMXON pointer.
    pub const VMPTRLD_VMXON_POINTER: u32 = 0x0000000A;

    /// VMPTRLD with incorrect VMCS revision identifier.
    pub const VMPTRLD_INCORRECT_VMCS_REVISION_ID: u32 = 0x0000000B;

    /// VMREAD/VMWRITE from/to unsupported VMCS component.
    pub const VMREAD_VMWRITE_INVALID_COMPONENT: u32 = 0x0000000C;

    /// VMWRITE to read-only VMCS component.
    pub const VMWRITE_READONLY_COMPONENT: u32 = 0x0000000D;

    /// VMXON executed in VMX root operation.
    pub const VMXON_IN_VMX_ROOT_OP: u32 = 0x0000000F;

    /// VM entry with invalid executive-VMCS pointer.
    pub const VMENTRY_INVALID_VMCS_EXECUTIVE_POINTER: u32 = 0x00000010;

    /// VM entry with non-launched executive VMCS.
    pub const VMENTRY_NON_LAUNCHED_EXECUTIVE_VMCS: u32 = 0x00000011;

    /// VM entry with executive-VMCS pointer not VMXON pointer (when attempting to deactivate the dual-monitor treatment of
    /// SMIs and SMM).
    pub const VMENTRY_EXECUTIVE_VMCS_PTR: u32 = 0x00000012;

    /// VMCALL with non-clear VMCS (when attempting to activate the dual-monitor treatment of SMIs and SMM).
    pub const VMCALL_NON_CLEAR_VMCS: u32 = 0x00000013;

    /// VMCALL with invalid VM-exit control fields.
    pub const VMCALL_INVALID_VMEXIT_FIELDS: u32 = 0x00000014;

    /// VMCALL with incorrect MSEG revision identifier (when attempting to activate the dual-monitor treatment of SMIs and
    /// SMM).
    pub const VMCALL_INVALID_MSEG_REVISION_ID: u32 = 0x00000016;

    /// VMXOFF under dual-monitor treatment of SMIs and SMM.
    pub const VMXOFF_DUAL_MONITOR: u32 = 0x00000017;

    /// VMCALL with invalid SMM-monitor features (when attempting to activate the dual-monitor treatment of SMIs and SMM).
    pub const VMCALL_INVALID_SMM_MONITOR: u32 = 0x00000018;

    /// VM entry with invalid VM-execution control fields in executive VMCS (when attempting to return from SMM).
    pub const VMENTRY_INVALID_VM_EXECUTION_CONTROL: u32 = 0x00000019;

    /// VM entry with events blocked by MOV SS.
    pub const VMENTRY_MOV_SS: u32 = 0x0000001A;

    /// Invalid operand to INVEPT/INVVPID.
    pub const INVEPT_INVVPID_INVALID_OPERAND: u32 = 0x0000001C;

    pub fn read() -> VmResult<Self> {
        let value = unsafe { x86::bits64::vmx::vmread(x86::vmx::vmcs::ro::VM_INSTRUCTION_ERROR)? };
        Ok(Self(value))
    }

    pub fn as_raw(&self) -> u64 {
        self.0
    }
}
