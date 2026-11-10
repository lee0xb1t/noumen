use crate::arch::x86_64::addr::PhysAddr;
use crate::arch::x86_64::vmx;
use crate::error::{VmFail, VmResult};

/// VMCS TSC offset (0x2010)：guest 的 `RDTSC` = 物理 TSC + 本字段，
/// 前提是 primary control 的 "use TSC offsetting" 已打开。
#[derive(Clone, Copy, Debug, Default)]
pub struct TscOffset(u64);

impl TscOffset {
    pub fn read() -> VmResult<Self> {
        let value = unsafe { x86::bits64::vmx::vmread(x86::vmx::vmcs::control::TSC_OFFSET_FULL)? };

        Ok(Self(value))
    }

    pub fn write(&self) -> VmResult<()> {
        unsafe {
            x86::bits64::vmx::vmwrite(x86::vmx::vmcs::control::TSC_OFFSET_FULL, self.0)?;
        }

        Ok(())
    }

    pub const fn from_raw(raw: u64) -> Self {
        Self(raw)
    }

    pub const fn as_raw(&self) -> u64 {
        self.0
    }
}

bitfield::bitfield! {
    #[repr(transparent)]
    #[derive(Clone, Copy, Debug, Default)]
    pub struct PinbasedCtls(u64);

    pub external_interrupt_exiting, set_external_interrupt_exiting: 0;
    pub reserved1, _: 2, 1;
    pub nmi_exiting, set_nmi_exiting: 3;
    pub reserved2, set_reserved2: 4;
    pub virtual_nmi, set_virtual_nmi: 5;
    pub activate_vmx_preemption_timer, set_activate_vmx_preemption_timer: 6;
    pub process_posted_interrupts, set_process_posted_interrupts: 7;
    pub reserved3, _: 63, 8;
}

impl PinbasedCtls {
    pub fn read() -> VmResult<Self> {
        let field = x86::vmx::vmcs::control::PINBASED_EXEC_CONTROLS;
        let value = unsafe { x86::bits64::vmx::vmread(field)? };
        Ok(Self(value))
    }

    pub fn write_adjust(&self) -> VmResult<()> {
        let vmx_basic = vmx::msr::VmxBasicMsr::read();
        let cap = if vmx_basic.true_controls() {
            vmx::msr::VmxTruePinbasedCtls::read().as_raw()
        } else {
            vmx::msr::VmxPinbasedCtls::read().as_raw()
        };

        let allowed_0 = cap & 0xFFFFFFFF;
        let allowed_1 = cap >> 32;

        let mut value = self.as_raw();
        value |= allowed_0;
        value &= allowed_1;

        assert_eq!(value & allowed_0, allowed_0);

        let field = x86::vmx::vmcs::control::PINBASED_EXEC_CONTROLS;
        unsafe { x86::bits64::vmx::vmwrite(field, value)? };

        Ok(())
    }

    pub const fn from_raw(raw: u64) -> Self {
        Self(raw)
    }

    pub const fn as_raw(&self) -> u64 {
        self.0
    }

    pub fn check_reserved(&self) -> Result<(), &'static str> {
        if self.reserved1() != 0 || self.reserved2() != false || self.reserved3() != 0 {
            Err("Reserved bits must be zero")
        } else {
            Ok(())
        }
    }
}

bitfield::bitfield! {
    #[repr(transparent)]
    #[derive(Clone, Copy, Debug, Default)]
    pub struct ProcBasedCtls(u64);

    pub reserved1, _: 1, 0;
    pub interrupt_window_exiting, set_interrupt_window_exiting: 2;
    pub use_tsc_offsetting, set_use_tsc_offsetting: 3;
    pub reserved2, _: 6, 4;
    pub hlt_exiting, set_hlt_exiting: 7;
    pub reserved3, _: 8;
    pub invlpg_exiting, set_invlpg_exiting: 9;
    pub mwait_exiting, set_mwait_exiting: 10;
    pub rdpmc_exiting, set_rdpmc_exiting: 11;
    pub rdtsc_exiting, set_rdtsc_exiting: 12;
    pub reserved4, _: 14, 13;
    pub cr3_load_exiting, set_cr3_load_exiting: 15;
    pub cr3_store_exiting, set_cr3_store_exiting: 16;
    pub activate_tertiary_controls, set_activate_tertiary_controls: 17;
    pub reserved5, _: 18;
    pub cr8_load_exiting, set_cr8_load_exiting: 19;
    pub cr8_store_exiting, set_cr8_store_exiting: 20;
    pub use_tpr_shadow, set_use_tpr_shadow: 21;
    pub nmi_window_exiting, set_nmi_window_exiting: 22;
    pub mov_dr_exiting, set_mov_dr_exiting: 23;
    pub unconditional_io_exiting, set_unconditional_io_exiting: 24;
    pub use_io_bitmaps, set_use_io_bitmaps: 25;
    pub reserved6, _: 26;
    pub monitor_trap_flag, set_monitor_trap_flag: 27;
    pub use_msr_bitmaps, set_use_msr_bitmaps: 28;
    pub monitor_exiting, set_monitor_exiting: 29;
    pub pause_exiting, set_pause_exiting: 30;
    pub activate_secondary_controls, set_activate_secondary_controls: 31;
    pub reserved7, _: 63, 32;
}

impl ProcBasedCtls {
    pub fn read() -> VmResult<Self> {
        let field = x86::vmx::vmcs::control::PRIMARY_PROCBASED_EXEC_CONTROLS;
        let vmread = unsafe { x86::bits64::vmx::vmread(field)? };

        Ok(Self(vmread))
    }

    pub fn write(&self) -> VmResult<()> {
        let field = x86::vmx::vmcs::control::PRIMARY_PROCBASED_EXEC_CONTROLS;
        unsafe { x86::bits64::vmx::vmwrite(field, self.as_raw())? };

        Ok(())
    }

    pub fn write_adjust(&self) -> VmResult<()> {
        let vmx_basic = vmx::msr::VmxBasicMsr::read();
        let cap = if vmx_basic.true_controls() {
            vmx::msr::VmxTrueProcBasedCtls::read().as_raw()
        } else {
            vmx::msr::VmxProcBasedCtls::read().as_raw()
        };

        let allowed_0 = cap & 0xFFFFFFFF;
        let allowed_1 = cap >> 32;

        let mut value = self.as_raw();
        value |= allowed_0;
        value &= allowed_1;

        assert_eq!(value & allowed_0, allowed_0);

        let field = x86::vmx::vmcs::control::PRIMARY_PROCBASED_EXEC_CONTROLS;
        unsafe { x86::bits64::vmx::vmwrite(field, value)? };

        Ok(())
    }

    pub const fn from_raw(raw: u64) -> Self {
        Self(raw)
    }

    pub const fn as_raw(&self) -> u64 {
        self.0
    }
}

bitfield::bitfield! {
    #[repr(transparent)]
    #[derive(Clone, Copy, Debug, Default)]
    pub struct ProcBasedCtls2(u64);

    /// Bit 0: Virtualize APIC accesses
    pub virtualize_apic_accesses, set_virtualize_apic_accesses: 0;

    /// Bit 1: Enable EPT
    pub enable_ept, set_enable_ept: 1;

    /// Bit 2: Descriptor-table instructions cause VM-exits
    pub descriptor_table_exiting, set_descriptor_table_exiting: 2;

    /// Bit 3: Enable RDTSCP
    pub enable_rdtscp, set_enable_rdtscp: 3;

    /// Bit 4: Virtualize x2APIC mode
    pub virtualize_x2apic_mode, set_virtualize_x2apic_mode: 4;

    /// Bit 5: Enable VPID
    pub enable_vpid, set_enable_vpid: 5;

    /// Bit 6: WBINVD exiting
    pub wbinvd_exiting, set_wbinvd_exiting: 6;

    /// Bit 7: Unrestricted guest
    pub unrestricted_guest, set_unrestricted_guest: 7;

    /// Bit 8: APIC register virtualization
    pub apic_register_virtualization, set_apic_register_virtualization: 8;

    /// Bit 9: Virtual interrupt delivery
    pub virtual_interrupt_delivery, set_virtual_interrupt_delivery: 9;

    /// Bit 10: PAUSE-loop exiting
    pub pause_loop_exiting, set_pause_loop_exiting: 10;

    /// Bit 11: RDRAND exiting
    pub rdrand_exiting, set_rdrand_exiting: 11;

    /// Bit 12: Enable INVPCID
    pub enable_invpcid, set_enable_invpcid: 12;

    /// Bit 13: Enable VM functions
    pub enable_vm_functions, set_enable_vm_functions: 13;

    /// Bit 14: VMCS shadowing
    pub vmcs_shadowing, set_vmcs_shadowing: 14;

    /// Bit 15: Enable ENCLS exiting
    pub enable_encls_exiting, set_enable_encls_exiting: 15;

    /// Bit 16: RDSEED exiting
    pub rdseed_exiting, set_rdseed_exiting: 16;

    /// Bit 17: Enable PML
    pub enable_pml, set_enable_pml: 17;

    /// Bit 18: EPT violation (#VE)
    pub ept_violation, set_ept_violation: 18;

    /// Bit 19: Conceal VMX from PT
    pub conceal_vmx_from_pt, set_conceal_vmx_from_pt: 19;

    /// Bit 20: Enable XSAVES
    pub enable_xsaves, set_enable_xsaves: 20;

    /// Bit 21: Enable PASID translation
    pub enable_pasid_translation, set_enable_pasid_translation: 21;

    /// Bit 22: Mode-based execute control for EPT
    pub mode_based_execute_control_for_ept, set_mode_based_execute_control_for_ept: 22;

    /// Bit 23: Sub-page write permissions for EPT
    pub sub_page_write_permissions_for_ept, set_sub_page_write_permissions_for_ept: 23;

    /// Bit 24: PT uses guest physical addresses
    pub pt_uses_guest_physical_addresses, set_pt_uses_guest_physical_addresses: 24;

    /// Bit 25: Use TSC scaling
    pub use_tsc_scaling, set_use_tsc_scaling: 25;

    /// Bit 26: Enable user wait/pause (TPAUSE, UMONITOR, UMWAIT)
    pub enable_user_wait_pause, set_enable_user_wait_pause: 26;

    /// Bit 27: Enable PCONFIG
    pub enable_pconfig, set_enable_pconfig: 27;

    /// Bit 28: Enable ENCLV exiting
    pub enable_enclv_exiting, set_enable_enclv_exiting: 28;

    /// Bit 29: Reserved
    pub reserved1, _: 29;

    /// Bit 30: Enable VMM bus lock detection
    pub enable_vmm_bus_lock_detection, set_enable_vmm_bus_lock_detection: 30;

    /// Bit 31: Enable instruction timeout exit
    pub enable_instruction_timeout_exit, set_enable_instruction_timeout_exit: 31;

    /// Bits 32-63: Reserved
    pub reserved2, _: 63, 32;
}

impl ProcBasedCtls2 {
    pub fn read() -> VmResult<Self> {
        let field = x86::vmx::vmcs::control::SECONDARY_PROCBASED_EXEC_CONTROLS;
        let value = unsafe { x86::bits64::vmx::vmread(field)? };

        Ok(Self(value))
    }

    pub fn write_adjust(&self) -> VmResult<()> {
        let cap = vmx::msr::VmxProcBasedCtls2::read().as_raw();

        let allowed_0 = cap & 0xFFFFFFFF;
        let allowed_1 = cap >> 32;

        let mut value = self.as_raw();
        value |= allowed_0;
        value &= allowed_1;

        assert_eq!(value & allowed_0, allowed_0);

        let field = x86::vmx::vmcs::control::SECONDARY_PROCBASED_EXEC_CONTROLS;
        unsafe { x86::bits64::vmx::vmwrite(field, value)? };

        Ok(())
    }

    pub const fn from_raw(raw: u64) -> Self {
        Self(raw)
    }

    pub const fn as_raw(&self) -> u64 {
        self.0
    }
}

bitfield::bitfield! {
    #[repr(transparent)]
    #[derive(Clone, Copy, Debug, Default)]
    pub struct VmExitCtls(u64);

    /// Bits 0-1: Reserved
    pub reserved1, _: 1, 0;

    /// Bit 2: Save debug controls (DR7 & IA32_DEBUGCTL)
    pub save_debug_controls, set_save_debug_controls: 2;

    /// Bits 3-8: Reserved
    pub reserved2, _: 8, 3;

    /// Bit 9: Host address space size (return to long mode)
    pub host_address_space_size, set_host_address_space_size: 9;

    /// Bits 10-11: Reserved
    pub reserved3, _: 11, 10;

    /// Bit 12: Load IA32_PERF_GLOBAL_CTRL on VM exit
    pub load_ia32_perf_global_ctrl, set_load_ia32_perf_global_ctrl: 12;

    /// Bits 13-14: Reserved
    pub reserved4, _: 14, 13;

    /// Bit 15: Acknowledge interrupt on VM exit
    pub acknowledge_interrupt_on_exit, set_acknowledge_interrupt_on_exit: 15;

    /// Bits 16-17: Reserved
    pub reserved5, _: 17, 16;

    /// Bit 18: Save IA32_PAT on VM exit
    pub save_ia32_pat, set_save_ia32_pat: 18;

    /// Bit 19: Load IA32_PAT on VM exit
    pub load_ia32_pat, set_load_ia32_pat: 19;

    /// Bit 20: Save IA32_EFER on VM exit
    pub save_ia32_efer, set_save_ia32_efer: 20;

    /// Bit 21: Load IA32_EFER on VM exit
    pub load_ia32_efer, set_load_ia32_efer: 21;

    /// Bit 22: Save VMX preemption timer value on VM exit
    pub save_vmx_preemption_timer_value, set_save_vmx_preemption_timer_value: 22;

    /// Bit 23: Clear IA32_BNDCFGS on VM exit
    pub clear_ia32_bndcfgs, set_clear_ia32_bndcfgs: 23;

    /// Bit 24: Conceal VMX from PT
    pub conceal_vmx_from_pt, set_conceal_vmx_from_pt: 24;

    /// Bit 25: Clear IA32_RTIT_CTL on VM exit
    pub clear_ia32_rtit_ctl, set_clear_ia32_rtit_ctl: 25;

    /// Bit 26: Clear IA32_LBR_CTL on VM exit
    pub clear_ia32_lbr_ctl, set_clear_ia32_lbr_ctl: 26;

    /// Bit 27: Clear UINV on VM exit
    pub clear_uinv, set_clear_uinv: 27;

    /// Bit 28: Load IA32_CET state on VM exit
    pub load_ia32_cet_state, set_load_ia32_cet_state: 28;

    /// Bit 29: Load IA32_PKRS on VM exit
    pub load_ia32_pkrs, set_load_ia32_pkrs: 29;

    /// Bit 30: Save IA32_PERF_GLOBAL_CTL on VM exit
    pub save_ia32_perf_global_ctl, set_save_ia32_perf_global_ctl: 30;

    /// Bit 31: Activate secondary VM-exit controls
    pub activate_secondary_controls, set_activate_secondary_controls: 31;

    /// Bits 32-63: Reserved
    pub reserved6, _: 63, 32;
}

impl VmExitCtls {
    pub fn read() -> VmResult<Self> {
        let field = x86::vmx::vmcs::control::VMEXIT_CONTROLS;
        let value = unsafe { x86::bits64::vmx::vmread(field)? };

        Ok(Self(value))
    }

    pub fn write_adjust(&self) -> VmResult<()> {
        let vmx_basic = vmx::msr::VmxBasicMsr::read();
        let cap = if vmx_basic.true_controls() {
            vmx::msr::VmxTrueExitCtls::read().as_raw()
        } else {
            vmx::msr::VmxExitCtls::read().as_raw()
        };

        let allowed_0 = cap & 0xFFFFFFFF;
        let allowed_1 = cap >> 32;

        let mut value = self.as_raw();
        value |= allowed_0;
        value &= allowed_1;

        assert_eq!(value & allowed_0, allowed_0);

        let field = x86::vmx::vmcs::control::VMEXIT_CONTROLS;
        unsafe { x86::bits64::vmx::vmwrite(field, value)? };

        Ok(())
    }

    pub const fn from_raw(raw: u64) -> Self {
        Self(raw)
    }

    pub const fn as_raw(&self) -> u64 {
        self.0
    }
}

bitfield::bitfield! {
    #[repr(transparent)]
    #[derive(Clone, Copy, Debug, Default)]
    pub struct VmEntryCtls(u64);

    /// Bits 0-1: Reserved
    pub reserved1, _: 1, 0;

    /// Bit 2: Load debug controls (DR7 & IA32_DEBUGCTL)
    pub load_debug_controls, set_load_debug_controls: 2;

    /// Bits 3-8: Reserved
    pub reserved2, _: 8, 3;

    /// Bit 9: IA-32e mode guest (64-bit guest)
    pub ia32e_mode_guest, set_ia32e_mode_guest: 9;

    /// Bit 10: Entry to SMM
    pub entry_to_smm, set_entry_to_smm: 10;

    /// Bit 11: Deactivate dual-monitor treatment
    pub deactivate_dual_monitor_treatment, set_deactivate_dual_monitor_treatment: 11;

    /// Bit 12: Reserved
    pub reserved3, _: 12;

    /// Bit 13: Load IA32_PERF_GLOBAL_CTRL on VM entry
    pub load_ia32_perf_global_ctrl, set_load_ia32_perf_global_ctrl: 13;

    /// Bit 14: Load IA32_PAT on VM entry
    pub load_ia32_pat, set_load_ia32_pat: 14;

    /// Bit 15: Load IA32_EFER on VM entry
    pub load_ia32_efer, set_load_ia32_efer: 15;

    /// Bit 16: Load IA32_BNDCFGS on VM entry
    pub load_ia32_bndcfgs, set_load_ia32_bndcfgs: 16;

    /// Bit 17: Conceal VMX from PT
    pub conceal_vmx_from_pt, set_conceal_vmx_from_pt: 17;

    /// Bit 18: Load IA32_RTIT_CTL on VM entry
    pub load_ia32_rtit_ctl, set_load_ia32_rtit_ctl: 18;

    /// Bit 19: Load UINV on VM entry
    pub load_uinv, set_load_uinv: 19;

    /// Bit 20: Load CET state on VM entry
    pub load_cet_state, set_load_cet_state: 20;

    /// Bit 21: Load IA32_LBR_CTL on VM entry
    pub load_ia32_lbr_ctl, set_load_ia32_lbr_ctl: 21;

    /// Bit 22: Load IA32_PKRS on VM entry
    pub load_ia32_pkrs, set_load_ia32_pkrs: 22;

    /// Bit 23: Load FRED MSRs on VM entry
    pub load_fred, set_load_fred: 23;

    /// Bit 24: Reserved
    pub reserved4, _: 24;

    /// Bit 25: Allow SEAM guest telemetry
    pub allow_seam_guest_telemetry, set_allow_seam_guest_telemetry: 25;

    /// Bits 26-63: Reserved
    pub reserved5, _: 63, 26;
}

impl VmEntryCtls {
    pub fn read() -> VmResult<Self> {
        let field = x86::vmx::vmcs::control::VMENTRY_CONTROLS;
        let value = unsafe { x86::bits64::vmx::vmread(field)? };

        Ok(Self(value))
    }

    pub fn write_adjust(&self) -> VmResult<()> {
        let vmx_basic = vmx::msr::VmxBasicMsr::read();
        let cap = if vmx_basic.true_controls() {
            vmx::msr::VmxTrueEntryCtls::read().as_raw()
        } else {
            vmx::msr::VmxEntryCtls::read().as_raw()
        };

        let allowed_0 = cap & 0xFFFFFFFF;
        let allowed_1 = cap >> 32;

        let mut value = self.as_raw();
        value |= allowed_0;
        value &= allowed_1;

        assert_eq!(value & allowed_0, allowed_0);

        let field = x86::vmx::vmcs::control::VMENTRY_CONTROLS;
        let vmwrite = unsafe { x86::bits64::vmx::vmwrite(field, value) };

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

    pub const fn from_raw(raw: u64) -> Self {
        Self(raw)
    }

    pub const fn as_raw(&self) -> u64 {
        self.0
    }
}

#[repr(transparent)]
#[derive(Clone, Copy, Debug, Default)]
pub struct ExceptionBitmap(u32);

impl ExceptionBitmap {
    pub fn read() -> VmResult<Self> {
        let field = x86::vmx::vmcs::control::EXCEPTION_BITMAP;
        let value = unsafe { x86::bits64::vmx::vmread(field)? };

        Ok(Self(value as u32))
    }

    pub fn write(&self) -> VmResult<()> {
        let field = x86::vmx::vmcs::control::EXCEPTION_BITMAP;
        unsafe { x86::bits64::vmx::vmwrite(field, self.as_raw() as u64)? };

        Ok(())
    }

    pub const fn from_raw(raw: u32) -> Self {
        Self(raw)
    }

    pub const fn as_raw(&self) -> u32 {
        self.0
    }
}

// Page fault error code
#[derive(Debug, Default)]
pub struct Pfec {
    mask: u32,
    match_: u32,
}

impl Pfec {
    pub fn read() -> VmResult<Self> {
        let field = x86::vmx::vmcs::control::PAGE_FAULT_ERR_CODE_MASK;
        let mask_value = unsafe { x86::bits64::vmx::vmread(field)? };

        let field = x86::vmx::vmcs::control::PAGE_FAULT_ERR_CODE_MATCH;
        let match_value = unsafe { x86::bits64::vmx::vmread(field)? };

        Ok(Self {
            mask: mask_value as u32,
            match_: match_value as u32,
        })
    }

    pub fn write(&self) -> VmResult<()> {
        let field = x86::vmx::vmcs::control::PAGE_FAULT_ERR_CODE_MASK;
        unsafe { x86::bits64::vmx::vmwrite(field, self.raw_mask() as u64)? };

        let field = x86::vmx::vmcs::control::PAGE_FAULT_ERR_CODE_MATCH;
        unsafe { x86::bits64::vmx::vmwrite(field, self.raw_match() as u64)? };

        Ok(())
    }

    pub const fn raw_mask(&self) -> u32 {
        self.mask
    }

    pub const fn raw_match(&self) -> u32 {
        self.match_
    }
}

#[derive(Debug, Default)]
pub struct Cr0Shadow {
    mask: u64,
    shadow: u64,
}

impl Cr0Shadow {
    pub fn read() -> VmResult<Self> {
        let field = x86::vmx::vmcs::control::CR0_GUEST_HOST_MASK;
        let mask_value = unsafe { x86::bits64::vmx::vmread(field)? };

        let field = x86::vmx::vmcs::control::CR0_READ_SHADOW;
        let shadow_value = unsafe { x86::bits64::vmx::vmread(field)? };

        Ok(Self {
            mask: mask_value,
            shadow: shadow_value,
        })
    }

    pub fn write(&self) -> VmResult<()> {
        let field = x86::vmx::vmcs::control::CR0_GUEST_HOST_MASK;
        unsafe { x86::bits64::vmx::vmwrite(field, self.raw_mask())? };

        let field = x86::vmx::vmcs::control::CR0_READ_SHADOW;
        unsafe { x86::bits64::vmx::vmwrite(field, self.raw_shadow())? };

        Ok(())
    }

    pub fn read_shadow() -> VmResult<u64> {
        let field = x86::vmx::vmcs::control::CR0_READ_SHADOW;
        Ok(unsafe { x86::bits64::vmx::vmread(field)? })
    }

    pub fn write_shadow(value: u64) -> VmResult<()> {
        let field = x86::vmx::vmcs::control::CR0_READ_SHADOW;
        unsafe { x86::bits64::vmx::vmwrite(field, value)? };
        Ok(())
    }

    pub const fn raw_mask(&self) -> u64 {
        self.mask
    }

    pub const fn raw_shadow(&self) -> u64 {
        self.shadow
    }

    pub fn set_raw_mask(&mut self, mask: u64) {
        self.mask = mask;
    }

    pub fn set_raw_shadow(&mut self, shadow: u64) {
        self.shadow = shadow;
    }
}

#[derive(Debug, Default)]
pub struct Cr4Shadow {
    mask: u64,
    shadow: u64,
}

impl Cr4Shadow {
    pub fn read() -> VmResult<Self> {
        let field = x86::vmx::vmcs::control::CR4_GUEST_HOST_MASK;
        let mask_value = unsafe { x86::bits64::vmx::vmread(field)? };

        let field = x86::vmx::vmcs::control::CR4_READ_SHADOW;
        let shadow_value = unsafe { x86::bits64::vmx::vmread(field)? };

        Ok(Self {
            mask: mask_value,
            shadow: shadow_value,
        })
    }

    pub fn write(&self) -> VmResult<()> {
        let field = x86::vmx::vmcs::control::CR4_GUEST_HOST_MASK;
        unsafe { x86::bits64::vmx::vmwrite(field, self.raw_mask())? };

        let field = x86::vmx::vmcs::control::CR4_READ_SHADOW;
        unsafe { x86::bits64::vmx::vmwrite(field, self.raw_shadow())? };

        Ok(())
    }

    pub fn read_shadow() -> VmResult<u64> {
        let field = x86::vmx::vmcs::control::CR4_READ_SHADOW;
        Ok(unsafe { x86::bits64::vmx::vmread(field)? })
    }

    pub fn write_shadow(flags: u64) -> VmResult<()> {
        let field = x86::vmx::vmcs::control::CR4_READ_SHADOW;
        unsafe { x86::bits64::vmx::vmwrite(field, flags)? };
        Ok(())
    }

    pub const fn raw_mask(&self) -> u64 {
        self.mask
    }

    pub const fn raw_shadow(&self) -> u64 {
        self.shadow
    }

    pub fn set_raw_mask(&mut self, mask: u64) {
        self.mask = mask;
    }

    pub fn set_raw_shadow(&mut self, shadow: u64) {
        self.shadow = shadow;
    }
}

// Intel sdm vol3 27.6.7
const CR3_TARGET_MAX_COUNT: usize = 4;
pub struct Cr3Target {
    count: u32,
    values: [u64; CR3_TARGET_MAX_COUNT],
}

impl Cr3Target {
    pub fn new() -> Self {
        Self {
            count: 0,
            values: [0; CR3_TARGET_MAX_COUNT],
        }
    }

    pub fn add(&mut self, value: u64) {
        if self.count as usize >= CR3_TARGET_MAX_COUNT {
            panic!("CR3-target count is greater than 4.");
        }
        self.values[self.count as usize] = value;
        self.count += 1;
    }

    pub fn write(&self) -> VmResult<()> {
        let field = x86::vmx::vmcs::control::CR3_TARGET_COUNT;
        unsafe { x86::bits64::vmx::vmwrite(field, self.count as u64)? };

        let field = x86::vmx::vmcs::control::CR3_TARGET_VALUE0;
        unsafe { x86::bits64::vmx::vmwrite(field, self.values[0])? };

        let field = x86::vmx::vmcs::control::CR3_TARGET_VALUE1;
        unsafe { x86::bits64::vmx::vmwrite(field, self.values[1])? };

        let field = x86::vmx::vmcs::control::CR3_TARGET_VALUE2;
        unsafe { x86::bits64::vmx::vmwrite(field, self.values[2])? };

        let field = x86::vmx::vmcs::control::CR3_TARGET_VALUE3;
        unsafe { x86::bits64::vmx::vmwrite(field, self.values[3])? };

        Ok(())
    }
}

#[repr(transparent)]
#[derive(Clone, Copy, Debug, Default)]
pub struct MsrBitmap(PhysAddr);

impl MsrBitmap {
    pub fn write(&self) -> VmResult<()> {
        let field = x86::vmx::vmcs::control::MSR_BITMAPS_ADDR_FULL;
        unsafe { x86::bits64::vmx::vmwrite(field, self.0.as_u64())? };

        Ok(())
    }

    pub const fn from_phys(raw: PhysAddr) -> Self {
        Self(raw)
    }
}

#[repr(transparent)]
#[derive(Clone, Copy, Debug, Default)]
pub struct Vpid(u64);

impl Vpid {
    pub fn read() -> VmResult<Self> {
        let field = x86::vmx::vmcs::control::VPID;
        let value = unsafe { x86::bits64::vmx::vmread(field)? };

        Ok(Self(value))
    }

    pub fn write(&self) -> VmResult<()> {
        let field = x86::vmx::vmcs::control::VPID;
        unsafe { x86::bits64::vmx::vmwrite(field, self.as_raw())? };

        Ok(())
    }

    pub const fn from_raw(raw: u16) -> Self {
        Self(raw as u64)
    }

    pub const fn as_raw(&self) -> u64 {
        self.0
    }
}

#[cfg(feature = "vmx-ept")]
#[repr(transparent)]
#[derive(Clone, Copy, Debug, Default)]
pub struct Eptp(vmx::ept::Eptp);

#[cfg(feature = "vmx-ept")]
impl Eptp {
    pub fn write(&self) -> VmResult<()> {
        let field = x86::vmx::vmcs::control::EPTP_FULL;
        unsafe { x86::bits64::vmx::vmwrite(field, self.0.as_raw())? };

        Ok(())
    }

    pub const fn from_eptp(eptp: vmx::ept::Eptp) -> Self {
        Self(eptp)
    }
}

/////////////////// VM EXIT /////////////////////
#[derive(Debug, Default, Clone, Copy)]
pub struct VmexitMsrStore {
    msr_store_count: usize,
    msr_store_phys: u64,
}

impl VmexitMsrStore {
    pub fn write(&self) -> VmResult<()> {
        let field = x86::vmx::vmcs::control::VMEXIT_MSR_STORE_COUNT;
        unsafe { x86::bits64::vmx::vmwrite(field, self.msr_store_count as u64)? };

        let field = x86::vmx::vmcs::control::VMEXIT_MSR_STORE_ADDR_FULL;
        unsafe { x86::bits64::vmx::vmwrite(field, self.msr_store_phys)? };

        Ok(())
    }
}

#[derive(Debug, Default, Clone, Copy)]
pub struct VmexitMsrLoad {
    msr_load_count: usize,
    msr_load_phys: u64,
}

impl VmexitMsrLoad {
    pub fn set_count(&mut self, count: usize) {
        self.msr_load_count = count;
    }

    pub fn set_address(&mut self, phys_addr: u64) {
        self.msr_load_phys = phys_addr;
    }

    pub fn write(&self) -> VmResult<()> {
        let field = x86::vmx::vmcs::control::VMEXIT_MSR_LOAD_COUNT;
        unsafe { x86::bits64::vmx::vmwrite(field, self.msr_load_count as u64)? };

        let field = x86::vmx::vmcs::control::VMEXIT_MSR_LOAD_ADDR_FULL;
        unsafe { x86::bits64::vmx::vmwrite(field, self.msr_load_phys)? };

        Ok(())
    }
}

/////////////////// VM ENTRY /////////////////////
#[derive(Debug, Default, Clone, Copy)]
pub struct VmentryMsrLoad {
    msr_load_count: usize,
    msr_load_phys: u64,
}

impl VmentryMsrLoad {
    pub fn set_count(&mut self, count: usize) {
        self.msr_load_count = count;
    }

    pub fn set_address(&mut self, phys_addr: u64) {
        self.msr_load_phys = phys_addr;
    }

    pub fn write(&self) -> VmResult<()> {
        let field = x86::vmx::vmcs::control::VMENTRY_MSR_LOAD_COUNT;
        unsafe { x86::bits64::vmx::vmwrite(field, self.msr_load_count as u64)? };

        let field = x86::vmx::vmcs::control::VMENTRY_MSR_LOAD_ADDR_FULL;
        unsafe { x86::bits64::vmx::vmwrite(field, self.msr_load_phys)? };

        Ok(())
    }
}

bitfield::bitfield! {
    /// VM-Entry Interruption-Information Field (32 bits)
    ///
    #[repr(transparent)]
    #[derive(Clone, Copy, Default, Debug)]
    pub struct InterruptInformation(u32);

    /// [Bits 7:0] Vector of interrupt or exception.
    /// Determines which entry in the IDT is used or which other event is injected.
    pub u8, vector, set_vector: 7, 0;

    /// [Bits 10:8] Interruption type.
    /// Determines details of how the injection is performed.
    pub u8, interruption_type, set_interruption_type: 10, 8;

    /// [Bit 11] Deliver error code (false = do not deliver; true = deliver).
    /// Determines whether delivery pushes an error code on the guest stack.
    pub bool, deliver_error_code, set_deliver_error_code: 11;

    /// [Bits 30:12] Reserved (19 bits).
    pub u32, reserved1, set_reserved1: 30, 12;

    /// [Bit 31] Valid.
    /// VM entry injects an event if and only if the valid bit is 1.
    /// The valid bit in this field is cleared on every VM exit.
    pub bool, valid, set_valid: 31;
}

impl InterruptInformation {
    pub fn write(&self) -> VmResult<()> {
        let field = x86::vmx::vmcs::control::VMENTRY_INTERRUPTION_INFO_FIELD;
        unsafe { x86::bits64::vmx::vmwrite(field, self.0 as u64)? };
        Ok(())
    }

    pub fn write_error_code(&self, error: u32) -> VmResult<()> {
        let field = x86::vmx::vmcs::control::VMENTRY_EXCEPTION_ERR_CODE;
        unsafe { x86::bits64::vmx::vmwrite(field, error as u64)? };
        Ok(())
    }
}

/// VM-entry 指令长度。
///
/// 重要：只有在注入软件异常时这个字段才有架构意义。`INT3` 是 trap 语义：
/// CPU 在 VM exit 之前不会推进 guest RIP，因此注入时必须告诉它构造异常帧时
/// 跳过多少字节（否则 handler 保存的 RIP 仍指向 `INT3` 本身，会无限重复执行）。
#[repr(transparent)]
#[derive(Debug, Default, Clone, Copy)]
pub struct VmentryInstructionLength(u32);

impl VmentryInstructionLength {
    pub fn write(value: u32) -> VmResult<()> {
        let field = x86::vmx::vmcs::control::VMENTRY_INSTRUCTION_LEN;
        unsafe { x86::bits64::vmx::vmwrite(field, value as u64)? };

        Ok(())
    }
}

#[derive(Debug, Default, Clone, Copy)]
pub struct InterruptInjection {
    interruption_info: u32,
    error_code: u32,
    instruction_length: u32,
}

impl InterruptInjection {
    pub fn enable_interruption(&mut self, int: u32) {
        self.interruption_info |= 1 << int;
    }

    pub fn set_error_code(&mut self, error_code: u32) {
        self.error_code = error_code;
    }

    pub fn set_inst_len(&mut self, inst_len: u32) {
        self.instruction_length = inst_len;
    }

    pub fn write(&self) -> VmResult<()> {
        let field = x86::vmx::vmcs::control::VMENTRY_INTERRUPTION_INFO_FIELD;
        unsafe { x86::bits64::vmx::vmwrite(field, self.interruption_info as u64)? };

        let field = x86::vmx::vmcs::control::VMENTRY_EXCEPTION_ERR_CODE;
        unsafe { x86::bits64::vmx::vmwrite(field, self.error_code as u64)? };

        let field = x86::vmx::vmcs::control::VMENTRY_INSTRUCTION_LEN;
        unsafe { x86::bits64::vmx::vmwrite(field, self.instruction_length as u64)? };

        Ok(())
    }
}
