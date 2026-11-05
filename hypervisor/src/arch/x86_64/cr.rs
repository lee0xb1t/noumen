bitfield::bitfield! {
    #[repr(transparent)]
    #[derive(Clone, Copy, Default, Debug)]
    pub struct Cr0(u64);

    pub protection_enable, set_protection_enable: 0;
    pub monitor_coprocessor, set_monitor_coprocessor: 1;
    pub emulate_fpu, set_emulate_fpu: 2;
    pub task_switched, set_task_switched: 3;
    pub extension_type, set_extension_type: 4;
    pub numeric_error, set_numeric_error: 5;

    pub reserved1, set_reserved1: 15, 6;

    pub write_protect, set_write_protect: 16;

    pub reserved2, set_reserved2: 17, 17;

    pub alignment_mask, set_alignment_mask: 18;

    pub reserved3, set_reserved3: 28, 19;

    pub not_write_through, set_not_write_through: 29;
    pub cache_disable, set_cache_disable: 30;
    pub paging_enable, set_paging_enable: 31;

    pub reserved4, set_reserved4: 63, 32;
}

impl Cr0 {
    pub const CR0_TASK_SWITCHED_FLAG: u64 = 0x08;
    pub const CR0_CACHE_DISABLE_FLAG: u64 = 0x40000000;
    pub const CR0_WRITE_PROTECT_FLAG: u64 = 0x10000;

    #[inline]
    pub fn read() -> Self {
        let raw: u64;
        unsafe {
            core::arch::asm!("mov {}, cr0", out(reg) raw, options(nomem, nostack, preserves_flags));
        }
        Self(raw)
    }

    #[inline]
    pub fn write(&self) {
        unsafe {
            core::arch::asm!("mov cr0, {}", in(reg) self.0, options(nostack, preserves_flags));
        }
    }

    #[inline]
    pub fn fixed0() -> Self {
        let fixed0 = unsafe { x86::msr::rdmsr(x86::msr::IA32_VMX_CR0_FIXED0) };
        Self(fixed0)
    }

    #[inline]
    pub fn fixed1() -> Self {
        let fixed0 = unsafe { x86::msr::rdmsr(x86::msr::IA32_VMX_CR0_FIXED1) };
        Self(fixed0)
    }

    pub const fn from_bits(flags: u64) -> Self {
        Self(flags)
    }

    pub const fn bits(&self) -> u64 {
        self.0
    }
}

bitfield::bitfield! {
    #[repr(transparent)]
    #[derive(Clone, Copy, Default, Debug)]
    pub struct Cr4(u64);

    pub virtual_mode_extensions, set_virtual_mode_extensions: 0;
    pub protected_mode_virtual_interrupts, set_protected_mode_virtual_interrupts: 1;
    pub timestamp_disable, set_timestamp_disable: 2;
    pub debugging_extensions, set_debugging_extensions: 3;
    pub page_size_extensions, set_page_size_extensions: 4;
    pub physical_address_extension, set_physical_address_extension: 5;
    pub machine_check_enable, set_machine_check_enable: 6;
    pub page_global_enable, set_page_global_enable: 7;
    pub performance_monitoring_counter_enable, set_performance_monitoring_counter_enable: 8;
    pub os_fxsave_fxrstor_support, set_os_fxsave_fxrstor_support: 9;
    pub os_xmm_exception_support, set_os_xmm_exception_support: 10;
    pub usermode_instruction_prevention, set_usermode_instruction_prevention: 11;
    pub linear_addresses_57_bit, set_linear_addresses_57_bit: 12;
    pub vmx_enable, set_vmx_enable: 13;
    pub smx_enable, set_smx_enable: 14;

    pub reserved1, _: 15;

    pub fsgsbase_enable, set_fsgsbase_enable: 16;
    pub pcid_enable, set_pcid_enable: 17;
    pub os_xsave, set_os_xsave: 18;
    pub key_locker_enable, set_key_locker_enable: 19;
    pub smep_enable, set_smep_enable: 20;
    pub smap_enable, set_smap_enable: 21;
    pub protection_key_enable, set_protection_key_enable: 22;
    pub control_flow_enforcement_enable, set_control_flow_enforcement_enable: 23;
    pub protection_key_for_supervisor_mode_enable, set_protection_key_for_supervisor_mode_enable: 24;
    pub user_interrupts_enable, set_user_interrupts_enable: 25;

    pub reserved2, _: 26;

    pub linear_address_space_seperation, set_linear_address_space_seperation: 27;
    pub supervisor_lam_enable, set_supervisor_lam_enable: 28;

    pub reserved3, _: 31, 29;

    pub fred_transition_enable, set_fred_transition_enable: 32;

    pub reserved4, _: 63, 33;
}

impl Cr4 {
    /// 必须用裸 `mov` 读：`x86::controlregs::cr4()` 会 `from_bits_truncate`，
    /// 把 flag 里没定义的位（例如 CET）静默清掉。
    #[inline]
    pub fn read() -> Self {
        let raw: u64;
        unsafe {
            core::arch::asm!("mov {}, cr4", out(reg) raw, options(nomem, nostack, preserves_flags));
        }
        Self(raw)
    }

    #[inline]
    pub fn write(&self) {
        unsafe {
            core::arch::asm!("mov cr4, {}", in(reg) self.0, options(nostack, preserves_flags));
        }
    }

    #[inline]
    pub fn fixed0() -> Self {
        let fixed0 = unsafe { x86::msr::rdmsr(x86::msr::IA32_VMX_CR4_FIXED0) };
        Self(fixed0)
    }

    #[inline]
    pub fn fixed1() -> Self {
        let fixed0 = unsafe { x86::msr::rdmsr(x86::msr::IA32_VMX_CR4_FIXED1) };
        Self(fixed0)
    }

    pub const fn from_bits(flags: u64) -> Self {
        Self(flags)
    }

    #[inline]
    pub const fn bits(&self) -> u64 {
        self.0
    }
}

bitfield::bitfield! {
    #[repr(transparent)]
    #[derive(Clone, Copy, Default, Debug)]
    pub struct Xcr0(u64);

    pub x87, set_x87: 0;
    pub sse, set_sse: 1;
    pub avx, set_avx: 2;
    pub bndreg, set_bndreg: 3;
    pub bndcsr, set_bndcsr: 4;
    pub opmask, set_opmask: 5;
    pub zmm_hi256, set_zmm_hi256: 6;
    pub zmm_hi16, set_zmm_hi16: 7;

    reserved1, _: 8;

    pub pkru, set_pkru: 9;

    reserved2, _: 63, 10;
}

impl Xcr0 {
    pub const fn from_bits(flags: u64) -> Self {
        Self(flags)
    }

    pub const fn bits(&self) -> u64 {
        self.0
    }
}

/// Reads SSP (`RDSSPQ`). Requires `CR4.CET = 1`.
pub fn read_ssp() -> u64 {
    let value: u64;

    unsafe {
        core::arch::asm!(
            ".byte 0xf3, 0x0f, 0x1e, 0xc8",
            out("rax") value,
            options(nostack, preserves_flags)
        );
    }

    value
}
