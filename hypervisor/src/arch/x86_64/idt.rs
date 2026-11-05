#[repr(u8)]
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum ExceptionVector {
    /// #DE - Divide Error.
    /// Source: DIV and IDIV instructions.
    /// Error Code: No.
    DivideError = 0x00,

    /// #DB - Debug.
    /// Source: Any code or data reference.
    /// Error Code: No.
    Debug = 0x01,

    /// Nonmaskable Interrupt.
    /// Source: Generated externally by asserting the processor's NMI pin or
    ///         through an NMI request set by the I/O APIC to the local APIC.
    /// Error Code: No.
    Nmi = 0x02,

    /// #BP - Breakpoint.
    /// Source: INT3 instruction.
    /// Error Code: No.
    Breakpoint = 0x03,

    /// #OF - Overflow.
    /// Source: INTO instruction.
    /// Error Code: No.
    Overflow = 0x04,

    /// #BR - BOUND Range Exceeded.
    /// Source: BOUND instruction.
    /// Error Code: No.
    BoundRangeExceeded = 0x05,

    /// #UD - Invalid Opcode (Undefined Opcode).
    /// Source: UD instruction or reserved opcode.
    /// Error Code: No.
    InvalidOpcode = 0x06,

    /// #NM - Device Not Available (No Math Coprocessor).
    /// Source: Floating-point or WAIT/FWAIT instruction.
    /// Error Code: No.
    DeviceNotAvailable = 0x07,

    /// #DF - Double Fault.
    /// Source: Any instruction that can generate an exception, an NMI, or an INTR.
    /// Error Code: Yes (zero).
    DoubleFault = 0x08,

    /// #-- - Coprocessor Segment Overrun (reserved).
    /// Source: Floating-point instruction.
    /// Error Code: No.
    ///
    /// **Note:** Processors after the Intel386 processor do not generate this exception.
    CoprocessorSegmentOverrun = 0x09,

    /// #TS - Invalid TSS.
    /// Source: Task switch or TSS access.
    /// Error Code: Yes.
    InvalidTss = 0x0A,

    /// #NP - Segment Not Present.
    /// Source: Loading segment registers or accessing system segments.
    /// Error Code: Yes.
    SegmentNotPresent = 0x0B,

    /// #SS - Stack Segment Fault.
    /// Source: Stack operations and SS register loads.
    /// Error Code: Yes.
    StackSegmentFault = 0x0C,

    /// #GP - General Protection.
    /// Source: Any memory reference and other protection checks.
    /// Error Code: Yes.
    GeneralProtection = 0x0D,

    /// #PF - Page Fault.
    /// Source: Any memory reference.
    /// Error Code: Yes.
    PageFault = 0x0E,

    /// #MF - Floating-Point Error (Math Fault).
    /// Source: Floating-point or WAIT/FWAIT instruction.
    /// Error Code: No.
    X87FloatingPointError = 0x10,

    /// #AC - Alignment Check.
    /// Source: Any data reference in memory.
    /// Error Code: Yes.
    AlignmentCheck = 0x11,

    /// #MC - Machine Check.
    /// Source: Model dependent machine check errors.
    /// Error Code: No.
    MachineCheck = 0x12,

    /// #XM - SIMD Floating-Point Numeric Error.
    /// Source: SSE/SSE2/SSE3 floating-point instructions.
    /// Error Code: No.
    SimdFloatingPointError = 0x13,

    /// #VE - Virtualization Exception.
    /// Source: EPT violations.
    /// Error Code: No.
    VirtualizationException = 0x14,

    /// #CP - Control Protection Exception.
    /// Source: Control flow transfer attempt violated the control flow
    ///         enforcement technology constraints.
    /// Error Code: Yes.
    ControlProtection = 0x15,
}

impl ExceptionVector {
    #[inline]
    pub const fn as_u8(self) -> u8 {
        self as u8
    }

    /// Decode a vector taken from a VMCS field; `None` for reserved and
    /// interrupt vectors.
    pub const fn from_u8(raw: u8) -> Option<Self> {
        Some(match raw {
            0x00 => Self::DivideError,
            0x01 => Self::Debug,
            0x02 => Self::Nmi,
            0x03 => Self::Breakpoint,
            0x04 => Self::Overflow,
            0x05 => Self::BoundRangeExceeded,
            0x06 => Self::InvalidOpcode,
            0x07 => Self::DeviceNotAvailable,
            0x08 => Self::DoubleFault,
            0x09 => Self::CoprocessorSegmentOverrun,
            0x0A => Self::InvalidTss,
            0x0B => Self::SegmentNotPresent,
            0x0C => Self::StackSegmentFault,
            0x0D => Self::GeneralProtection,
            0x0E => Self::PageFault,
            0x10 => Self::X87FloatingPointError,
            0x11 => Self::AlignmentCheck,
            0x12 => Self::MachineCheck,
            0x13 => Self::SimdFloatingPointError,
            0x14 => Self::VirtualizationException,
            0x15 => Self::ControlProtection,
            _ => return None,
        })
    }
}

// Architectural interruption types; only the ones the hypervisor delivers
// are constructed in this build.
#[allow(dead_code)]
#[repr(u8)]
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum InterruptionType {
    /// External interrupt.
    ExternalInterrupt = 0x00,

    /// Non-maskable interrupt (NMI).
    NonMaskableInterrupt = 0x02,

    /// Hardware exception (e.g., #PF).
    HardwareException = 0x03,

    /// Software interrupt (INT n).
    SoftwareInterrupt = 0x04,

    /// Privileged software exception (INT1).
    PrivilegedSoftwareException = 0x05,

    /// Software exception (INT3 or INTO).
    SoftwareException = 0x06,

    /// Other event. This type is used for injection of events that are not
    /// delivered through the IDT.
    OtherEvent = 0x07,
}

impl InterruptionType {
    #[inline]
    pub const fn as_u8(self) -> u8 {
        self as u8
    }
}
