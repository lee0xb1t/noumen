//! A VM exit ends in one of three ways: the guest continues, the guest takes an
//! architectural event, or the partition dies. Host-side failures never become
//! guest-visible events.
//!
//! Backend-neutral: both VMX and SVM use this control-flow vocabulary. The only
//! VMX-specific variant is [`Terminate::Vmcs`]; SVM keeps the same shape and
//! will add its own field access error type (or reuse `Vmcs` as "control block
//! access failed") when the exit path lands.

use core::{fmt, panic::Location};

use crate::arch::x86_64::idt::{ExceptionVector, InterruptionType};
use crate::error::VmFail;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Exception {
    pub vector: ExceptionVector,
    pub error_code: Option<u32>,
}

impl Exception {
    pub const fn new(vector: ExceptionVector, error_code: Option<u32>) -> Self {
        Self { vector, error_code }
    }

    pub const fn gp(code: u32) -> Self {
        Self::new(ExceptionVector::GeneralProtection, Some(code))
    }

    pub const fn ud() -> Self {
        Self::new(ExceptionVector::InvalidOpcode, None)
    }

    pub const fn interruption_type(&self) -> InterruptionType {
        InterruptionType::HardwareException
    }
}

#[derive(Debug)]
pub enum EmulationError {
    Guest(Exception),
    Stop,
    Terminate(Terminate),
}

impl EmulationError {
    pub const fn gp(code: u32) -> Self {
        Self::Guest(Exception::gp(code))
    }

    pub const fn ud() -> Self {
        Self::Guest(Exception::ud())
    }
}

pub type EmulationResult<T> = Result<T, EmulationError>;

/// A failed VMCS access is a host-side bug, never a guest fault.
impl From<VmFail> for EmulationError {
    fn from(e: VmFail) -> Self {
        Self::Terminate(Terminate::Vmcs(e))
    }
}

impl From<Terminate> for EmulationError {
    fn from(t: Terminate) -> Self {
        Self::Terminate(t)
    }
}

#[derive(Debug)]
pub enum Terminate {
    UnhandledExit {
        reason: u32,
        qualification: u64,
        guest_rip: u64,
        guest_rsp: u64,
    },
    Internal(&'static Location<'static>),
    Vmcs(VmFail),
}

impl Terminate {
    #[track_caller]
    pub fn internal() -> Self {
        Self::Internal(Location::caller())
    }
}

impl From<VmFail> for Terminate {
    fn from(e: VmFail) -> Self {
        Self::Vmcs(e)
    }
}

pub type TerminateResult<T> = Result<T, Terminate>;

impl fmt::Display for Terminate {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::UnhandledExit {
                reason,
                qualification,
                guest_rip,
                guest_rsp,
            } => write!(
                f,
                "unhandled VM exit: reason={reason:#x} qualification={qualification:#x} \
                 rip={guest_rip:#x} rsp={guest_rsp:#x}"
            ),
            Self::Internal(location) => write!(f, "internal invariant violated at {location}"),
            Self::Vmcs(e) => write!(f, "control block access failed: {e}"),
        }
    }
}

#[must_use = "the resume decision has to be acted on"]
#[derive(Debug)]
pub enum Resume {
    Continue,
    Inject(Exception),
    /// 退出 VMX 并在 root 里直接回到 guest（见 `stub::stop_virtualization`）。
    Stop,
    Terminate(Terminate),
}

impl From<EmulationError> for Resume {
    fn from(e: EmulationError) -> Self {
        match e {
            EmulationError::Guest(exception) => Self::Inject(exception),
            EmulationError::Stop => Self::Stop,
            EmulationError::Terminate(reason) => Self::Terminate(reason),
        }
    }
}
