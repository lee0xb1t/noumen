//! VM-exit information fields and exit-qualification decoding.
//!
//! This module owns the read-only exit information the hypervisor consumes
//! from the VMCS (`VM_EXIT_REASON`, `EXIT_QUALIFICATION`,
//! `VMEXIT_INSTRUCTION_LEN`, and the VM-exit interruption information) and
//! the layouts of the qualification field. See Intel SDM Vol. 3C, Section 27.2.

// Decode helpers cover the architectural exit-information surface.
#![allow(dead_code)]

use crate::arch::x86_64::guest_context;
use crate::error::VmResult;
/// Architectural control register.
#[repr(u8)]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum VmxExitQualificationControlRegister {
    Cr0 = 0,
    Cr2 = 2,
    Cr3 = 3,
    Cr4 = 4,
    Cr8 = 8,
}

impl VmxExitQualificationControlRegister {
    /// Decode the control-register number stored in an instruction or VM-exit
    /// qualification.
    ///
    /// Returns `None` for encodings that do not name a control register.
    pub const fn from_raw(raw: u8) -> Option<Self> {
        match raw {
            0 => Some(Self::Cr0),
            2 => Some(Self::Cr2),
            3 => Some(Self::Cr3),
            4 => Some(Self::Cr4),
            8 => Some(Self::Cr8),
            _ => None,
        }
    }
}

/// Kind of control-register access.
#[repr(u8)]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum VmxExitQualificationAccessType {
    /// `MOV to CR`.
    MovToCr = 0,
    /// `MOV from CR`.
    MovFromCr = 1,
    /// `CLTS`.
    Clts = 2,
    /// `LMSW`.
    Lmsw = 3,
}

impl VmxExitQualificationAccessType {
    /// Decode the 2-bit access type stored in an instruction or VM-exit
    /// qualification.
    pub const fn from_raw(raw: u8) -> Self {
        match raw {
            0 => Self::MovToCr,
            1 => Self::MovFromCr,
            2 => Self::Clts,
            3 => Self::Lmsw,
            _ => panic!("invalid control-register access type"),
        }
    }
}

/// Raw value of the `EXIT_QUALIFICATION` VMCS field.
#[repr(transparent)]
#[derive(Copy, Clone, Debug)]
pub struct VmxExitQualification(u64);

impl VmxExitQualification {
    pub fn read() -> VmResult<Self> {
        let field = x86::vmx::vmcs::ro::EXIT_QUALIFICATION;
        let value = unsafe { x86::bits64::vmx::vmread(field)? };

        Ok(Self(value))
    }

    pub const fn as_raw(&self) -> u64 {
        self.0
    }

    /// Reinterpret this qualification as a control-register access.
    ///
    /// Only meaningful for a VM exit whose basic exit reason is
    /// [`VmxExitReason::MOV_CR`].
    pub const fn mov_cr(&self) -> VmxExitQualificationMovCr {
        VmxExitQualificationMovCr::from_raw(self.0)
    }
}

bitfield::bitfield! {
    /// EPT violation 的 exit qualification。
    ///
    /// 见 Intel SDM Vol. 3C, Section 27.2.1 "Exit Qualification for EPT
    /// Violations"。
    ///
    /// 低三位描述 guest 想做什么；bit 3-5 描述出错 EPT 条目实际持有的权限。
    /// hooked 页 armed 时 execute 被清掉，所以取指会报告
    /// `execute_access = 1, ept_executable = 0`；对临时映射到 hook 副本的页
    /// 做数据访问则报告 `read|write = 1, ept_readable|ept_writable = 0`。
    #[repr(transparent)]
    #[derive(Clone, Copy, Debug, Default)]
    pub struct VmxExitQualificationEptViolation(u64);

    /// [Bit 0] 访问是数据读。
    pub bool, read_access, _: 0;
    /// [Bit 1] 访问是数据写。
    pub bool, write_access, _: 1;
    /// [Bit 2] 访问是取指。
    pub bool, execute_access, _: 2;
    /// [Bit 3] EPT 条目允许读。
    pub bool, ept_readable, _: 3;
    /// [Bit 4] EPT 条目允许写。
    pub bool, ept_writable, _: 4;
    /// [Bit 5] EPT 条目允许执行。
    pub bool, ept_executable, _: 5;
    // Bit 6 保留。
    pub _, _: 6;
    /// [Bit 7] `GUEST_LINEAR_ADDRESS` 保存出错的线性地址。
    pub bool, guest_linear_address_valid, _: 7;
    /// [Bit 8] 该 guest 物理地址来自 guest 页表结构的访问（嵌套页走），
    /// 而不是内存访问本身。
    pub bool, guest_physical_address_valid, _: 8;
    pub _, _: 63, 9;
}

impl VmxExitQualificationEptViolation {
    pub const fn as_raw(&self) -> u64 {
        self.0
    }

    /// 读取 qualification 字段。只对基本退出原因是
    /// [`VmxExitReason::EPT_VIOLATION`] 的退出有意义。
    pub fn read() -> VmResult<Self> {
        let field = x86::vmx::vmcs::ro::EXIT_QUALIFICATION;
        let value = unsafe { x86::bits64::vmx::vmread(field)? };

        Ok(Self(value))
    }
}

bitfield::bitfield! {
    /// Exit qualification for a control-register access (`MOV_CR`), which also
    /// covers `CLTS` and `LMSW`.
    ///
    /// See Intel SDM Vol. 3C, Section 27.2.1
    /// "Exit Qualification for Control-Register Accesses".
    #[repr(transparent)]
    #[derive(Clone, Copy, Debug)]
    pub struct VmxExitQualificationMovCr(u64);

    /// [Bits 3:0] Number of the control register (0 for `CLTS` and `LMSW`).
    pub u8, raw_control_register, _: 3, 0;
    /// [Bits 5:4] Access type.
    pub u8, raw_access_type, _: 5, 4;
    /// [Bit 6] `LMSW` operand source: `true` for memory, `false` for a
    /// register. Cleared to `false` for `CLTS` and `MOV CR`.
    pub bool, lmsw_operand_from_memory, _: 6;
    // Bit 7 is reserved.
    pub _, _: 7;
    /// [Bits 11:8] General-purpose register for `MOV CR`.
    pub u8, raw_general_purpose_register, _: 11, 8;
    // Bits 15:12 are reserved.
    pub _, _: 15, 12;
    /// [Bits 31:16] `LMSW` source data; zero for `CLTS` and `MOV CR`.
    pub u16, lmsw_source_data, _: 31, 16;
    // Bits 63:32 are reserved.
    pub _, _: 63, 32;
}

impl VmxExitQualificationMovCr {
    pub const fn from_raw(raw: u64) -> Self {
        Self(raw)
    }

    /// The kind of control-register access that caused the VM exit.
    pub fn access_type(&self) -> VmxExitQualificationAccessType {
        VmxExitQualificationAccessType::from_raw(self.raw_access_type())
    }

    /// The control register accessed by `MOV to CR` or `MOV from CR`.
    ///
    /// Returns `None` for `CLTS` and `LMSW`.
    pub fn control_register(&self) -> Option<VmxExitQualificationControlRegister> {
        match self.access_type() {
            VmxExitQualificationAccessType::MovToCr | VmxExitQualificationAccessType::MovFromCr => {
                VmxExitQualificationControlRegister::from_raw(self.raw_control_register())
            }
            VmxExitQualificationAccessType::Clts | VmxExitQualificationAccessType::Lmsw => None,
        }
    }

    /// The general-purpose register operand of `MOV to CR` or `MOV from CR`.
    ///
    /// Returns `None` for `CLTS` and `LMSW`.
    pub fn general_purpose_register(&self) -> Option<guest_context::GeneralPurposeRegister> {
        match self.access_type() {
            VmxExitQualificationAccessType::MovToCr | VmxExitQualificationAccessType::MovFromCr => {
                Some(guest_context::GeneralPurposeRegister::from_raw(
                    self.raw_general_purpose_register(),
                ))
            }
            VmxExitQualificationAccessType::Clts | VmxExitQualificationAccessType::Lmsw => None,
        }
    }
}

////////////// VMEXIT REASON ///////////////
#[repr(transparent)]
#[derive(Copy, Clone, Debug, Default)]
pub struct VmxExitReason(u32);

impl VmxExitReason {
    pub const fn from_raw(raw: u32) -> Self {
        Self(raw)
    }

    pub const fn as_raw(&self) -> u32 {
        self.0
    }

    pub fn read() -> VmResult<Self> {
        let field = x86::vmx::vmcs::ro::EXIT_REASON;
        let value = unsafe { x86::bits64::vmx::vmread(field)? };

        Ok(Self(value as u32))
    }

    pub const fn basic_exit_reason(&self) -> u16 {
        ((self.0 >> 0) & 0xFFFF) as u16
    }

    pub fn set_basic_exit_reason(&mut self, value: u16) {
        let value = (value as u32) & 0xFFFF;
        self.0 = (self.0 & !0xFFFF) | value;
    }

    pub const fn always0(&self) -> bool {
        ((self.0 >> 16) & 1) != 0
    }

    pub const fn enclave_mode(&self) -> bool {
        ((self.0 >> 27) & 1) != 0
    }

    pub fn set_enclave_mode(&mut self, val: bool) {
        let bit = if val { 1 } else { 0 };
        self.0 = (self.0 & !(1 << 27)) | (bit << 27);
    }

    pub const fn pending_mtf_vm_exit(&self) -> bool {
        ((self.0 >> 28) & 1) != 0
    }

    pub fn set_pending_mtf_vm_exit(&mut self, val: bool) {
        let bit = if val { 1 } else { 0 };
        self.0 = (self.0 & !(1 << 28)) | (bit << 28);
    }

    pub const fn vm_exit_from_vmx_root(&self) -> bool {
        ((self.0 >> 29) & 1) != 0
    }

    pub fn set_vm_exit_from_vmx_root(&mut self, val: bool) {
        let bit = if val { 1 } else { 0 };
        self.0 = (self.0 & !(1 << 29)) | (bit << 29);
    }

    pub const fn vm_entry_failure(&self) -> bool {
        ((self.0 >> 31) & 1) != 0
    }

    pub fn set_vm_entry_failure(&mut self, val: bool) {
        let bit = if val { 1 } else { 0 };
        self.0 = (self.0 & !(1 << 31)) | (bit << 31);
    }
}

impl core::fmt::Display for VmxExitReason {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        write!(
            f,
            "VmxExitReason {{ reason: 0x{:04X}, enclave: {}, mtf: {}, root: {}, entry_fail: {} }}",
            self.basic_exit_reason(),
            self.enclave_mode(),
            self.pending_mtf_vm_exit(),
            self.vm_exit_from_vmx_root(),
            self.vm_entry_failure()
        )
    }
}

////////////// VMEXIT REASON Defines ///////////////
///! VMX basic exit reasons and instruction error numbers.
///!
///! See Intel SDM Volume 3C, Chapter 27 (VMX Basic Exit Reasons) and Section 30.4 (VM-Instruction Error Numbers).

/// VMX basic exit reasons.
///
/// These are the values returned in bits 15:0 of the VM-exit reason field.
/// See Vol3C[VMX BASIC EXIT REASONS].
impl VmxExitReason {
    /// Exception or non-maskable interrupt (NMI).
    ///
    /// Either:
    /// - Guest software caused an exception and the bit in the exception bitmap associated with exception's vector was 1.
    ///   This case includes executions of BOUND that cause #BR, executions of INT1 (they cause #DB), executions of INT3
    ///   (they cause #BP), executions of INTO that cause #OF, and executions of UD0, UD1, and UD2 (they cause #UD).
    /// - An NMI was delivered to the logical processor and the "NMI exiting" VM-execution control was 1.
    pub const EXCEPTION_OR_NMI: u32 = 0x00000000;

    /// External interrupt.
    ///
    /// An external interrupt arrived and the "external-interrupt exiting" VM-execution control was 1.
    pub const EXTERNAL_INTERRUPT: u32 = 0x00000001;

    /// Triple fault.
    ///
    /// The logical processor encountered an exception while attempting to call the double-fault handler and that exception
    /// did not itself cause a VM exit due to the exception bitmap.
    pub const TRIPLE_FAULT: u32 = 0x00000002;

    /// INIT signal.
    ///
    /// An INIT signal arrived.
    pub const INIT_SIGNAL: u32 = 0x00000003;

    /// Start-up IPI (SIPI).
    ///
    /// A SIPI arrived while the logical processor was in the "wait-for-SIPI" state.
    pub const STARTUP_IPI: u32 = 0x00000004;

    /// I/O system-management interrupt (SMI) immediately after retirement of an I/O instruction.
    ///
    /// An SMI arrived immediately after retirement of an I/O instruction and caused an SMM VM exit.
    /// See Vol3C[34.15.2(SMM VM Exits)].
    pub const IO_SMI: u32 = 0x00000005;

    /// Other SMI.
    ///
    /// An SMI arrived and caused an SMM VM exit but not immediately after retirement of an I/O instruction.
    /// See Vol3C[34.15.2(SMM VM Exits)].
    pub const SMI: u32 = 0x00000006;

    /// Interrupt window exiting.
    ///
    /// At the beginning of an instruction, RFLAGS.IF was 1; events were not blocked by STI or by MOV SS; and the
    /// "interrupt-window exiting" VM-execution control was 1.
    pub const INTERRUPT_WINDOW: u32 = 0x00000007;

    /// NMI window exiting.
    ///
    /// At the beginning of an instruction, there was no virtual-NMI blocking; events were not blocked by MOV SS; and the
    /// "NMI-window exiting" VM-execution control was 1.
    pub const NMI_WINDOW: u32 = 0x00000008;

    /// Task switch.
    ///
    /// Guest software attempted a task switch.
    pub const TASK_SWITCH: u32 = 0x00000009;

    /// CPUID.
    ///
    /// Guest software attempted to execute CPUID.
    pub const EXECUTE_CPUID: u32 = 0x0000000A;

    /// GETSEC.
    ///
    /// Guest software attempted to execute GETSEC.
    pub const EXECUTE_GETSEC: u32 = 0x0000000B;

    /// HLT.
    ///
    /// Guest software attempted to execute HLT and the "HLT exiting" VM-execution control was 1.
    pub const EXECUTE_HLT: u32 = 0x0000000C;

    /// INVD.
    ///
    /// Guest software attempted to execute INVD.
    pub const EXECUTE_INVD: u32 = 0x0000000D;

    /// INVLPG.
    ///
    /// Guest software attempted to execute INVLPG and the "INVLPG exiting" VM-execution control was 1.
    pub const EXECUTE_INVLPG: u32 = 0x0000000E;

    /// RDPMC.
    ///
    /// Guest software attempted to execute RDPMC and the "RDPMC exiting" VM-execution control was 1.
    pub const EXECUTE_RDPMC: u32 = 0x0000000F;

    /// RDTSC.
    ///
    /// Guest software attempted to execute RDTSC and the "RDTSC exiting" VM-execution control was 1.
    pub const EXECUTE_RDTSC: u32 = 0x00000010;

    /// RSM in SMM.
    ///
    /// Guest software attempted to execute RSM in SMM.
    pub const EXECUTE_RSM_IN_SMM: u32 = 0x00000011;

    /// VMCALL.
    ///
    /// VMCALL was executed either by guest software (causing an ordinary VM exit) or by the executive monitor (causing an
    /// SMM VM exit). See Vol3C[34.15.2(SMM VM Exits)].
    pub const EXECUTE_VMCALL: u32 = 0x00000012;

    /// VMCLEAR.
    ///
    /// Guest software attempted to execute VMCLEAR.
    pub const EXECUTE_VMCLEAR: u32 = 0x00000013;

    /// VMLAUNCH.
    ///
    /// Guest software attempted to execute VMLAUNCH.
    pub const EXECUTE_VMLAUNCH: u32 = 0x00000014;

    /// VMPTRLD.
    ///
    /// Guest software attempted to execute VMPTRLD.
    pub const EXECUTE_VMPTRLD: u32 = 0x00000015;

    /// VMPTRST.
    ///
    /// Guest software attempted to execute VMPTRST.
    pub const EXECUTE_VMPTRST: u32 = 0x00000016;

    /// VMREAD.
    ///
    /// Guest software attempted to execute VMREAD.
    pub const EXECUTE_VMREAD: u32 = 0x00000017;

    /// VMRESUME.
    ///
    /// Guest software attempted to execute VMRESUME.
    pub const EXECUTE_VMRESUME: u32 = 0x00000018;

    /// VMWRITE.
    ///
    /// Guest software attempted to execute VMWRITE.
    pub const EXECUTE_VMWRITE: u32 = 0x00000019;

    /// VMXOFF.
    ///
    /// Guest software attempted to execute VMXOFF.
    pub const EXECUTE_VMXOFF: u32 = 0x0000001A;

    /// VMXON.
    ///
    /// Guest software attempted to execute VMXON.
    pub const EXECUTE_VMXON: u32 = 0x0000001B;

    /// Control-register accesses.
    ///
    /// Guest software attempted to access CR0, CR3, CR4, or CR8 using CLTS, LMSW, or MOV CR and the VM-execution control
    /// fields indicate that a VM exit should occur. This basic exit reason is not used for trap-like VM exits following
    /// executions of the MOV to CR8 instruction when the "use TPR shadow" VM-execution control is 1. Such VM exits instead
    /// use basic exit reason 43 (TPR_BELOW_THRESHOLD).
    /// See Vol3C[25.1(INSTRUCTIONS THAT CAUSE VM EXITS)].
    pub const MOV_CR: u32 = 0x0000001C;

    /// Debug-register accesses.
    ///
    /// Guest software attempted a MOV to or from a debug register and the "MOV-DR exiting" VM-execution control was 1.
    pub const MOV_DR: u32 = 0x0000001D;

    /// I/O instruction.
    ///
    /// Guest software attempted to execute an I/O instruction and either:
    /// - The "use I/O bitmaps" VM-execution control was 0 and the "unconditional I/O exiting" VM-execution control was 1.
    /// - The "use I/O bitmaps" VM-execution control was 1 and a bit in the I/O bitmap associated with one of the ports
    ///   accessed by the I/O instruction was 1.
    pub const EXECUTE_IO_INSTRUCTION: u32 = 0x0000001E;

    /// RDMSR.
    ///
    /// Guest software attempted to execute RDMSR and either:
    /// - The "use MSR bitmaps" VM-execution control was 0.
    /// - The value of RCX is neither in the range 00000000H - 00001FFFH nor in the range C0000000H - C0001FFFH.
    /// - The value of RCX was in the range 00000000H - 00001FFFH and the nth bit in read bitmap for low MSRs is 1, where n
    ///   was the value of RCX.
    /// - The value of RCX is in the range C0000000H - C0001FFFH and the nth bit in read bitmap for high MSRs is 1, where n
    ///   is the value of RCX & 00001FFFH.
    pub const EXECUTE_RDMSR: u32 = 0x0000001F;

    /// WRMSR.
    ///
    /// Guest software attempted to execute WRMSR and either:
    /// - The "use MSR bitmaps" VM-execution control was 0.
    /// - The value of RCX is neither in the range 00000000H - 00001FFFH nor in the range C0000000H - C0001FFFH.
    /// - The value of RCX was in the range 00000000H - 00001FFFH and the nth bit in write bitmap for low MSRs is 1, where n
    ///   was the value of RCX.
    /// - The value of RCX is in the range C0000000H - C0001FFFH and the nth bit in write bitmap for high MSRs is 1, where n
    ///   is the value of RCX & 00001FFFH.
    pub const EXECUTE_WRMSR: u32 = 0x00000020;

    /// VM-entry failure due to invalid guest state.
    ///
    /// A VM entry failed one of the checks identified in Section 26.3.1.
    pub const ERROR_INVALID_GUEST_STATE: u32 = 0x00000021;

    /// VM-entry failure due to MSR loading.
    ///
    /// A VM entry failed in an attempt to load MSRs. See Section 26.4.
    pub const ERROR_MSR_LOAD: u32 = 0x00000022;

    /// Guest software executed MWAIT.
    ///
    /// Guest software attempted to execute MWAIT and the "MWAIT exiting" VM-execution control was 1.
    pub const EXECUTE_MWAIT: u32 = 0x00000024;

    /// VM-exit due to monitor trap flag.
    ///
    /// A VM entry occurred due to the 1-setting of the "monitor trap flag" VM-execution control and injection of an MTF VM
    /// exit as part of VM entry. See Vol3C[25.5.2(Monitor Trap Flag)].
    pub const MONITOR_TRAP_FLAG: u32 = 0x00000025;

    /// Guest software attempted to execute MONITOR.
    ///
    /// Guest software attempted to execute MONITOR and the "MONITOR exiting" VM-execution control was 1.
    pub const EXECUTE_MONITOR: u32 = 0x00000027;

    /// Guest software attempted to execute PAUSE.
    ///
    /// Either guest software attempted to execute PAUSE and the "PAUSE exiting" VM-execution control was 1 or the
    /// "PAUSE-loop exiting" VM-execution control was 1 and guest software executed a PAUSE loop with execution time
    /// exceeding PLE_Window. See Vol3C[25.1.3(Instructions That Cause VM Exits Conditionally)].
    pub const EXECUTE_PAUSE: u32 = 0x00000028;

    /// VM-entry failure due to machine-check.
    ///
    /// A machine-check event occurred during VM entry. See Vol3C[26.8(MACHINE-CHECK EVENTS DURING VM ENTRY)].
    pub const ERROR_MACHINE_CHECK: u32 = 0x00000029;

    /// TPR below threshold.
    ///
    /// The logical processor determined that the value of bits 7:4 of the byte at offset 080H on the virtual-APIC page was
    /// below that of the TPR threshold VM-execution control field while the "use TPR shadow" VM-execution control was 1
    /// either as part of TPR virtualization or VM entry.
    /// See Vol3C[29.1.2(TPR Virtualization)] and Vol3C[26.6.7(VM Exits Induced by the TPR Threshold)].
    pub const TPR_BELOW_THRESHOLD: u32 = 0x0000002B;

    /// APIC access.
    ///
    /// Guest software attempted to access memory at a physical address on the APIC-access page and the "virtualize APIC
    /// accesses" VM-execution control was 1. See Vol3C[29.4(VIRTUALIZING MEMORY-MAPPED APIC ACCESSES)].
    pub const APIC_ACCESS: u32 = 0x0000002C;

    /// Virtualized EOI.
    ///
    /// EOI virtualization was performed for a virtual interrupt whose vector indexed a bit set in the EOI-exit bitmap.
    pub const VIRTUALIZED_EOI: u32 = 0x0000002D;

    /// Access to GDTR or IDTR.
    ///
    /// Guest software attempted to execute LGDT, LIDT, SGDT, or SIDT and the "descriptor-table exiting" VM-execution
    /// control was 1.
    pub const GDTR_IDTR_ACCESS: u32 = 0x0000002E;

    /// Access to LDTR or TR.
    ///
    /// Guest software attempted to execute LLDT, LTR, SLDT, or STR and the "descriptor-table exiting" VM-execution control
    /// was 1.
    pub const LDTR_TR_ACCESS: u32 = 0x0000002F;

    /// EPT violation.
    ///
    /// An attempt to access memory with a guest-physical address was disallowed by the configuration of the EPT paging
    /// structures.
    pub const EPT_VIOLATION: u32 = 0x00000030;

    /// EPT misconfiguration.
    ///
    /// An attempt to access memory with a guest-physical address encountered a misconfigured EPT paging-structure entry.
    pub const EPT_MISCONFIGURATION: u32 = 0x00000031;

    /// INVEPT.
    ///
    /// Guest software attempted to execute INVEPT.
    pub const EXECUTE_INVEPT: u32 = 0x00000032;

    /// RDTSCP.
    ///
    /// Guest software attempted to execute RDTSCP and the "enable RDTSCP" and "RDTSC exiting" VM-execution controls were
    /// both 1.
    pub const EXECUTE_RDTSCP: u32 = 0x00000033;

    /// VMX-preemption timer expired.
    ///
    /// The preemption timer counted down to zero.
    pub const VMX_PREEMPTION_TIMER_EXPIRED: u32 = 0x00000034;

    /// INVVPID.
    ///
    /// Guest software attempted to execute INVVPID.
    pub const EXECUTE_INVVPID: u32 = 0x00000035;

    /// WBINVD.
    ///
    /// Guest software attempted to execute WBINVD and the "WBINVD exiting" VM-execution control was 1.
    pub const EXECUTE_WBINVD: u32 = 0x00000036;

    /// XSETBV.
    ///
    /// Guest software attempted to execute XSETBV.
    pub const EXECUTE_XSETBV: u32 = 0x00000037;

    /// APIC write.
    ///
    /// Guest software completed a write to the virtual-APIC page that must be virtualized by VMM software.
    /// See Vol3C[29.4.3.3(APIC-Write VM Exits)].
    pub const APIC_WRITE: u32 = 0x00000038;

    /// RDRAND.
    ///
    /// Guest software attempted to execute RDRAND and the "RDRAND exiting" VM-execution control was 1.
    pub const EXECUTE_RDRAND: u32 = 0x00000039;

    /// INVPCID.
    ///
    /// Guest software attempted to execute INVPCID and the "enable INVPCID" and "INVLPG exiting" VM-execution controls were
    /// both 1.
    pub const EXECUTE_INVPCID: u32 = 0x0000003A;

    /// VMFUNC.
    ///
    /// Guest software invoked a VM function with the VMFUNC instruction and the VM function either was not enabled or
    /// generated a function-specific condition causing a VM exit.
    pub const EXECUTE_VMFUNC: u32 = 0x0000003B;

    /// ENCLS.
    ///
    /// Guest software attempted to execute ENCLS and "enable ENCLS exiting" VM-execution control was 1 and either:
    /// - EAX < 63 and the corresponding bit in the ENCLS-exiting bitmap is 1; or
    /// - EAX >= 63 and bit 63 in the ENCLS-exiting bitmap is 1.
    pub const EXECUTE_ENCLS: u32 = 0x0000003C;

    /// RDSEED.
    ///
    /// Guest software attempted to execute RDSEED and the "RDSEED exiting" VM-execution control was 1.
    pub const EXECUTE_RDSEED: u32 = 0x0000003D;

    /// Page-modification log full.
    ///
    /// The processor attempted to create a page-modification log entry and the value of the PML index was not in the range
    /// 0-511.
    pub const PAGE_MODIFICATION_LOG_FULL: u32 = 0x0000003E;

    /// XSAVES.
    ///
    /// Guest software attempted to execute XSAVES, the "enable XSAVES/XRSTORS" was 1, and a bit was set in the logical-AND
    /// of the following three values: EDX:EAX, the IA32_XSS MSR, and the XSS-exiting bitmap.
    pub const EXECUTE_XSAVES: u32 = 0x0000003F;

    /// XRSTORS.
    ///
    /// Guest software attempted to execute XRSTORS, the "enable XSAVES/XRSTORS" was 1, and a bit was set in the logical-AND
    /// of the following three values: EDX:EAX, the IA32_XSS MSR, and the XSS-exiting bitmap.
    pub const EXECUTE_XRSTORS: u32 = 0x00000040;

    /// PCONFIG.
    ///
    /// Guest software attempted to execute PCONFIG, "enable PCONFIG" VM-execution control was 1, and either (1) EAX < 63
    /// and the corresponding bit in the PCONFIG-exiting bitmap is 1; or (2) EAX >= 63 and bit 63 in the PCONFIG-exiting
    /// bitmap is 1.
    pub const EXECUTE_PCONFIG: u32 = 0x00000041;

    /// SPP-related event.
    ///
    /// The processor attempted to determine an access's sub-page write permission and encountered an SPP miss or an SPP
    /// misconfiguration. See Vol3C[30.3.4.2(Determining an Access's Sub-Page Write Permission)].
    pub const SPP_RELATED_EVENT: u32 = 0x00000042;

    /// UMWAIT.
    ///
    /// Guest software attempted to execute UMWAIT and the "enable user wait and pause" and "RDTSC exiting" VM-execution
    /// controls were both 1.
    pub const EXECUTE_UMWAIT: u32 = 0x00000043;

    /// TPAUSE.
    ///
    /// Guest software attempted to execute TPAUSE and the "enable user wait and pause" and "RDTSC exiting" VM-execution
    /// controls were both 1.
    pub const EXECUTE_TPAUSE: u32 = 0x00000044;

    /// LOADIWKEY.
    ///
    /// Guest software attempted to execute LOADIWKEY and the "LOADIWKEY exiting" VM-execution control was 1.
    pub const EXECUTE_LOADIWKEY: u32 = 0x00000045;

    /// ENCLV.
    ///
    /// Guest software attempted to execute ENCLV, the "enable ENCLV exiting" VM-execution control was 1, and either
    /// (1) EAX < 63 and the corresponding bit in the ENCLV-exiting bitmap is 1; or (2) EAX >= 63 and bit 63 in the
    /// ENCLV-exiting bitmap is 1.
    pub const EXECUTE_ENCLV: u32 = 0x00000046;

    /// ENQCMD PASID translation failure.
    ///
    /// A VM exit occurred during PASID translation because the present bit was clear in a PASID-directory entry, the valid
    /// bit was clear in a PASID-table entry, or one of the entries set a reserved bit.
    pub const EXECUTE_ENQCMD: u32 = 0x00000048;

    /// ENQCMDS PASID translation failure.
    ///
    /// A VM exit occurred during PASID translation because the present bit was clear in a PASID-directory entry, the valid
    /// bit was clear in a PASID-table entry, or one of the entries set a reserved bit.
    pub const EXECUTE_ENQCMDS: u32 = 0x00000049;

    /// Bus lock.
    ///
    /// The processor asserted a bus lock while the "bus-lock detection" VM-execution control was 1. (Such VM exits will
    /// also set bit 26 of the exit-reason field.)
    pub const BUS_LOCK_ASSERTION: u32 = 0x0000004A;

    /// Instruction timeout.
    ///
    /// The "instruction timeout" VM-execution control was 1 and certain operations prevented the processor from reaching an
    /// instruction boundary within the amount of time specified by the instruction-timeout control.
    pub const INSTRUCTION_TIMEOUT: u32 = 0x0000004B;

    /// SEAMCALL.
    ///
    /// Guest software attempted to execute SEAMCALL.
    pub const EXECUTE_SEAMCALL: u32 = 0x0000004C;

    /// TDCALL.
    ///
    /// Guest software attempted to execute TDCALL.
    pub const EXECUTE_TDCALL: u32 = 0x0000004D;

    /// RDMSRLIST.
    ///
    /// Guest software attempted to execute RDMSRLIST and either the "use MSR bitmaps" VM-execution control was 0 or any of
    /// the following holds for the index an MSR being accessed:
    /// * The index is neither in the range 00000000H - 00001FFFH nor in the range C0000000H - C0001FFFH.
    /// * The index is in the range 00000000H - 00001FFFH and the nth bit in read bitmap for low MSRs is 1, where n is the
    ///   index.
    /// * The index is in the range C0000000H - C0001FFFH and the nth bit in read bitmap for high MSRs is 1, where n is the
    ///   logical AND of the index and the value 00001FFFH.
    pub const EXECUTE_RDMSRLIST: u32 = 0x0000004E;

    /// WRMSRLIST.
    ///
    /// Guest software attempted to execute WRMSRLIST and either the "use MSR bitmaps" VM-execution control was 0 or any of
    /// the following holds for the index an MSR being accessed:
    /// * The index is neither in the range 00000000H - 00001FFFH nor in the range C0000000H - C0001FFFH.
    /// * The index is in the range 00000000H - 00001FFFH and the nth bit in write bitmap for low MSRs is 1, where n is the
    ///   index.
    /// * The index is in the range C0000000H - C0001FFFH and the nth bit in write bitmap for high MSRs is 1, where n is the
    ///   logical AND of the index and the value 00001FFFH.
    pub const EXECUTE_WRMSRLIST: u32 = 0x0000004F;
}

#[repr(transparent)]
#[derive(Copy, Clone, Debug)]
pub struct VmxExitInstructionLength(u64);

impl VmxExitInstructionLength {
    pub fn read() -> VmResult<Self> {
        let field = x86::vmx::vmcs::ro::VMEXIT_INSTRUCTION_LEN;
        let value = unsafe { x86::bits64::vmx::vmread(field)? };

        Ok(Self(value))
    }

    #[inline]
    pub fn as_raw(&self) -> u64 {
        self.0
    }
}

bitfield::bitfield! {
    #[repr(transparent)]
    #[derive(Clone, Copy, Debug, Default)]
    pub struct VmxExitInterruptionInformation(u32);

    pub u8, vector, _: 7, 0;
    pub u8, interruption_type, _: 10, 8;
    pub bool, deliver_error_code, _: 11;
    pub bool, valid, _: 31;
}

impl VmxExitInterruptionInformation {
    pub fn read() -> VmResult<Self> {
        let field = x86::vmx::vmcs::ro::VMEXIT_INTERRUPTION_INFO;
        let value = unsafe { x86::bits64::vmx::vmread(field)? };
        Ok(Self(value as u32))
    }
}

#[repr(transparent)]
#[derive(Clone, Copy, Debug, Default)]
pub struct VmxExitInterruptionErrorCode(u32);

impl VmxExitInterruptionErrorCode {
    pub fn read() -> VmResult<Self> {
        let field = x86::vmx::vmcs::ro::VMEXIT_INTERRUPTION_ERR_CODE;
        let value = unsafe { x86::bits64::vmx::vmread(field)? };
        Ok(Self(value as u32))
    }

    pub const fn as_u32(&self) -> u32 {
        self.0
    }
}

/// 当前 VM exit 关联的 guest 物理地址。
#[repr(transparent)]
#[derive(Copy, Clone, Debug, Default)]
pub struct VmxExitGuestPhysicalAddress(u64);

impl VmxExitGuestPhysicalAddress {
    /// 当前 VM exit 关联的 guest 物理地址。对 EPT violation 和 EPT
    /// misconfiguration 有效。
    pub fn read() -> VmResult<Self> {
        let field = x86::vmx::vmcs::ro::GUEST_PHYSICAL_ADDR_FULL;
        let value = unsafe { x86::bits64::vmx::vmread(field)? };

        Ok(Self(value))
    }

    #[inline]
    pub const fn as_raw(&self) -> u64 {
        self.0
    }
}

/// 当前 VM exit 关联的 guest 线性地址。
#[repr(transparent)]
#[derive(Copy, Clone, Debug, Default)]
pub struct VmxExitGuestLinearAddress(u64);

impl VmxExitGuestLinearAddress {
    /// 当前 VM exit 关联的 guest 线性地址。只有 qualification 标记有效时才
    /// 有意义，例如 [`VmxExitQualificationEptViolation`] 的 bit 7。
    pub fn read() -> VmResult<Self> {
        let field = x86::vmx::vmcs::ro::GUEST_LINEAR_ADDR;
        let value = unsafe { x86::bits64::vmx::vmread(field)? };

        Ok(Self(value))
    }

    #[inline]
    pub const fn as_raw(&self) -> u64 {
        self.0
    }
}
