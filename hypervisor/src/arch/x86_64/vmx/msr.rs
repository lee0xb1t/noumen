use crate::arch::x86_64::vmx::vmcs::control;

bitfield::bitfield! {
        #[repr(transparent)]
    #[derive(Debug)]
    pub struct VmxBasicMsr(u64);

    pub revision_id, _: 30, 0;
    pub reserved_1, _: 31, 31;
    pub vmcs_size_in_bytes, _: 44, 32;
    pub reserved_2, _: 47, 45;
    pub physical_address_width, _: 48;
    pub dual_monitor, _: 49;
    pub memory_type, _: 53, 50;
    pub ins_outs_info, _: 54;
    pub true_controls, _: 55;
    pub reserved_3, _: 63, 56;
}

impl VmxBasicMsr {
    pub fn read() -> Self {
        let vmx_basic = unsafe { x86::msr::rdmsr(x86::msr::IA32_VMX_BASIC) };
        Self(vmx_basic)
    }

    pub fn as_raw(&self) -> u64 {
        self.0
    }
}

bitfield::bitfield! {
    #[repr(transparent)]
    #[derive(Debug)]
    pub struct VmxEptVpidCap(u64);

    pub execute_only_pages, _: 0;
    pub page_walk_length_4, _: 6;
    pub memory_type_uncacheable, _: 8;
    pub memory_type_write_back, _: 14;
    pub pde_2mb_pages, _: 16;
    pub pdpte_1gb_pages, _: 17;
    pub invept, _: 20;
    pub ept_accessed_and_dirty_flags, _: 21;
    pub advanced_vmexit_ept_violations_information, _: 22;
    pub invept_single_context, _: 25;
    pub invept_all_contexts, _: 26;
    pub invvpid, _: 32;
    pub invvpid_individual_address, _: 40;
    pub invvpid_single_context, _: 41;
    pub invvpid_all_contexts, _: 42;
    pub invvpid_single_context_retain_globals, _: 43;
}

impl VmxEptVpidCap {
    pub fn read() -> Self {
        let raw = unsafe { x86::msr::rdmsr(x86::msr::IA32_VMX_EPT_VPID_CAP) };
        Self(raw)
    }

    pub fn as_raw(&self) -> u64 {
        self.0
    }
}

#[repr(transparent)]
pub struct VmxPinbasedCtls(pub control::PinbasedCtls);
impl VmxPinbasedCtls {
    pub fn read() -> Self {
        let raw = unsafe { x86::msr::rdmsr(x86::msr::IA32_VMX_PINBASED_CTLS) };
        Self(control::PinbasedCtls(raw))
    }

    pub fn as_raw(&self) -> u64 {
        self.0.as_raw()
    }
}

#[repr(transparent)]
pub struct VmxTruePinbasedCtls(pub control::PinbasedCtls);
impl VmxTruePinbasedCtls {
    pub fn read() -> Self {
        let raw = unsafe { x86::msr::rdmsr(x86::msr::IA32_VMX_TRUE_PINBASED_CTLS) };
        Self(control::PinbasedCtls(raw))
    }

    pub fn as_raw(&self) -> u64 {
        self.0.as_raw()
    }
}

#[repr(transparent)]
pub struct VmxProcBasedCtls(pub control::ProcBasedCtls);
impl VmxProcBasedCtls {
    pub fn read() -> Self {
        let raw = unsafe { x86::msr::rdmsr(x86::msr::IA32_VMX_PROCBASED_CTLS) };
        Self(control::ProcBasedCtls(raw))
    }

    pub fn as_raw(&self) -> u64 {
        self.0.as_raw()
    }
}

#[repr(transparent)]
pub struct VmxTrueProcBasedCtls(pub control::ProcBasedCtls);
impl VmxTrueProcBasedCtls {
    pub fn read() -> Self {
        let raw = unsafe { x86::msr::rdmsr(x86::msr::IA32_VMX_TRUE_PROCBASED_CTLS) };
        Self(control::ProcBasedCtls(raw))
    }

    pub fn as_raw(&self) -> u64 {
        self.0.as_raw()
    }
}

#[repr(transparent)]
pub struct VmxProcBasedCtls2(pub control::ProcBasedCtls2);
impl VmxProcBasedCtls2 {
    pub fn read() -> Self {
        let raw = unsafe { x86::msr::rdmsr(x86::msr::IA32_VMX_PROCBASED_CTLS2) };
        Self(control::ProcBasedCtls2(raw))
    }

    pub fn as_raw(&self) -> u64 {
        self.0.as_raw()
    }
}

#[repr(transparent)]
pub struct VmxExitCtls(pub control::VmExitCtls);
impl VmxExitCtls {
    pub fn read() -> Self {
        let raw = unsafe { x86::msr::rdmsr(x86::msr::IA32_VMX_EXIT_CTLS) };
        Self(control::VmExitCtls(raw))
    }

    pub fn as_raw(&self) -> u64 {
        self.0.as_raw()
    }
}

#[repr(transparent)]
pub struct VmxTrueExitCtls(pub control::VmExitCtls);
impl VmxTrueExitCtls {
    pub fn read() -> Self {
        let raw = unsafe { x86::msr::rdmsr(x86::msr::IA32_VMX_TRUE_EXIT_CTLS) };
        Self(control::VmExitCtls(raw))
    }

    pub fn as_raw(&self) -> u64 {
        self.0.as_raw()
    }
}

#[repr(transparent)]
pub struct VmxEntryCtls(pub control::VmEntryCtls);
impl VmxEntryCtls {
    pub fn read() -> Self {
        let raw = unsafe { x86::msr::rdmsr(x86::msr::IA32_VMX_ENTRY_CTLS) };
        Self(control::VmEntryCtls(raw))
    }

    pub fn as_raw(&self) -> u64 {
        self.0.as_raw()
    }
}

#[repr(transparent)]
pub struct VmxTrueEntryCtls(pub control::VmEntryCtls);
impl VmxTrueEntryCtls {
    pub fn read() -> Self {
        let raw = unsafe { x86::msr::rdmsr(x86::msr::IA32_VMX_TRUE_ENTRY_CTLS) };
        Self(control::VmEntryCtls(raw))
    }

    pub fn as_raw(&self) -> u64 {
        self.0.as_raw()
    }
}

bitfield::bitfield! {
    /// IA32_VMX_MISC (MSR 0x485) — VMX capability reporting, **read-only**.
    /// See Intel SDM Vol3C, Appendix A.6 "MISCELLANEOUS DATA".
    #[repr(transparent)]
    #[derive(Clone, Copy, Debug)]
    pub struct Ia32VmxMisc(u64);

    // [4:0]   preemption timer / TSC relationship
    pub preemption_timer_tsc_relationship, _: 4, 0;

    // [5]     VM-exit stores EFER.LMA into "IA-32e mode guest"
    pub store_efer_lma_on_vmexit, _: 5;

    // [8:6]   activity states bitmap (bit6=HLT, bit7=shutdown, bit8=wait-for-SIPI)
    pub activity_states, _: 8, 6;

    // [13:9]  reserved
    pub _, _: 13, 9;

    // [14]    Intel PT usable in VMX operation
    pub intel_pt_available_in_vmx, _: 14;

    // [15]    RDMSR can read IA32_SMBASE in SMM
    pub rdmsr_can_read_ia32_smbase_msr_in_smm, _: 15;

    // [24:16] CR3-target count **raw** value (256 特例见 impl)
    pub cr3_target_count_raw, _: 24, 16;

    // [27:25] MSR-list size **raw** N (实际值 = 512*(N+1)，见 impl)
    pub max_number_of_msr_raw, _: 27, 25;

    // [28]    IA32_SMM_MONITOR_CTL[2] can be set
    pub smm_monitor_ctl_b2, _: 28;

    // [29]    VMWRITE can write VM-exit info fields
    pub vmwrite_vmexit_info, _: 29;

    // [30]    VM-entry can inject len-0 SW interrupt/exception
    pub zero_length_instruction_vmentry_injection, _: 30;

    // [31]    reserved
    pub _, _: 31;

    // [63:32] MSEG revision identifier
    pub mseg_id, _: 63, 32;
}

impl Ia32VmxMisc {
    pub const MSR: u32 = 0x0000_0485;

    pub fn read() -> Self {
        let raw = unsafe { x86::msr::rdmsr(Self::MSR) };
        Self(raw)
    }

    pub fn as_raw(&self) -> u64 {
        self.0
    }
}
