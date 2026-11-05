use x86::msr::{self, rdmsr, wrmsr};

/// IA32_U_CET.
pub const IA32_U_CET: u32 = 0x0000_06a0;

/// IA32_S_CET.
pub const IA32_S_CET: u32 = 0x0000_06a2;

/// IA32_PL0_SSP.
pub const IA32_PL0_SSP: u32 = 0x0000_06a4;

/// IA32_PL1_SSP.
pub const IA32_PL1_SSP: u32 = 0x0000_06a5;

/// IA32_PL2_SSP.
pub const IA32_PL2_SSP: u32 = 0x0000_06a6;

/// IA32_PL3_SSP.
pub const IA32_PL3_SSP: u32 = 0x0000_06a7;

/// IA32_INTERRUPT_SSP_TABLE_ADDR.
pub const IA32_INTERRUPT_SSP_TABLE_ADDR: u32 = 0x0000_06a8;

bitfield::bitfield! {
    #[repr(transparent)]
    #[derive(Debug)]
    pub struct Ia32FeatureControl(u64);

    pub lock_bit, set_lock_bit: 0;
    pub enable_vmx_inside_smx, set_enable_vmx_inside_smx: 1;
    pub enable_vmx_outside_smx, set_enable_vmx_outside_smx: 2;
    pub reserved1, _: 7, 3;
    pub senter_local_function_enables, set_senter_local_function_enables: 14, 8;
    pub senter_global_enable, set_senter_global_enable: 15;
    pub reserved2, _: 16, 16;
    pub sgx_launch_control_enable, set_sgx_launch_control_enable: 17;
    pub sgx_global_enable, set_sgx_global_enable: 18;
    pub reserved3, _: 19, 19;
    pub lmce_on, set_lmce_on: 20;
    pub reserved4, _: 63, 21;
}

impl Ia32FeatureControl {
    #[inline]
    pub fn read() -> Self {
        let raw = unsafe { rdmsr(msr::IA32_FEATURE_CONTROL) };
        Self(raw)
    }

    #[inline]
    pub fn write(&self) {
        unsafe { wrmsr(msr::IA32_FEATURE_CONTROL, self.as_raw()) }
    }

    #[inline]
    pub fn as_raw(&self) -> u64 {
        self.0
    }
}

#[repr(transparent)]
#[derive(Debug)]
pub struct FsBase(u64);
impl FsBase {
    pub fn read() -> Self {
        let raw = unsafe { rdmsr(msr::IA32_FS_BASE) };
        Self(raw)
    }

    pub fn as_raw(&self) -> u64 {
        self.0
    }
}

#[repr(transparent)]
#[derive(Debug)]
pub struct GsBase(u64);
impl GsBase {
    pub fn read() -> Self {
        let raw = unsafe { rdmsr(msr::IA32_GS_BASE) };
        Self(raw)
    }

    pub fn as_raw(&self) -> u64 {
        self.0
    }
}

#[repr(transparent)]
#[derive(Debug)]
pub struct Ia32SysenterCs(u64);
impl Ia32SysenterCs {
    pub fn read() -> Self {
        let raw = unsafe { rdmsr(msr::IA32_SYSENTER_CS) };
        Self(raw)
    }

    pub fn as_raw(&self) -> u64 {
        self.0
    }
}

#[repr(transparent)]
#[derive(Debug)]
pub struct Ia32SysenterEsp(u64);
impl Ia32SysenterEsp {
    pub fn read() -> Self {
        let raw = unsafe { rdmsr(msr::IA32_SYSENTER_ESP) };
        Self(raw)
    }

    pub fn as_raw(&self) -> u64 {
        self.0
    }
}

#[repr(transparent)]
#[derive(Debug)]
pub struct Ia32SysenterEip(u64);
impl Ia32SysenterEip {
    pub fn read() -> Self {
        let raw = unsafe { rdmsr(msr::IA32_SYSENTER_EIP) };
        Self(raw)
    }

    pub fn as_raw(&self) -> u64 {
        self.0
    }
}

#[repr(transparent)]
#[derive(Debug)]
pub struct Ia32Debugctl(u64);
impl Ia32Debugctl {
    pub fn read() -> Self {
        let raw = unsafe { rdmsr(msr::IA32_DEBUGCTL) };
        Self(raw)
    }

    pub fn as_raw(&self) -> u64 {
        self.0
    }
}

#[repr(transparent)]
#[derive(Debug)]
pub struct Ia32Pat(u64);
impl Ia32Pat {
    pub fn read() -> Self {
        let raw = unsafe { rdmsr(msr::IA32_PAT) };
        Self(raw)
    }

    pub fn as_raw(&self) -> u64 {
        self.0
    }
}

#[repr(transparent)]
#[derive(Debug)]
pub struct Ia32PerfGlobalCtrl(u64);
impl Ia32PerfGlobalCtrl {
    pub fn read() -> Self {
        let raw = unsafe { rdmsr(msr::IA32_PERF_GLOBAL_CTRL) };
        Self(raw)
    }

    pub fn as_raw(&self) -> u64 {
        self.0
    }
}
