use core::error::Error;
use core::fmt::{self, Debug, Display};

#[derive(Debug)]
#[repr(u32)]
pub enum VmFail {
    VmFailValid,
    VmFailInvalid,
}

impl Display for VmFail {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            VmFail::VmFailValid => write!(f, "VmFailValid"),
            VmFail::VmFailInvalid => write!(f, "VmFailInvalid"),
        }
    }
}

impl From<x86::vmx::VmFail> for VmFail {
    fn from(e: x86::vmx::VmFail) -> Self {
        match e {
            x86::vmx::VmFail::VmFailInvalid => Self::VmFailInvalid,
            x86::vmx::VmFail::VmFailValid => Self::VmFailValid,
        }
    }
}

#[repr(u32)]
#[derive(Debug)]
pub enum HvError {
    InvalidPrameters,
    UnsupportedCpu,
    NotEnoughMemory,
    VmxIsAlreadyUse,
    LoadVmxonFail(VmFail),
    LoadVmcsFail(VmFail),
    PinbasedCtrlsFail(VmFail),
    ProcbasedCtrlsFail(VmFail),
    ProcbasedCtrls2Fail(VmFail),
    ExitCtrlsFail(VmFail),
    EntryCtrlsFail(VmFail),
    ExceptionBitmapCtrlFail(VmFail),
    PfecCtrlFail(VmFail),
    Cr0ReadCtrlFail(VmFail),
    Cr4ReadCtrlFail(VmFail),
    Cr3TargetCtrlFail(VmFail),
    MsrBitmapCtrlFail(VmFail),
    #[cfg(feature = "vmx-ept")]
    EptpCtrlFail(VmFail),

    VmexitMsrStoreCtrlFail(VmFail),
    VmexitMsrLoadCtrlFail(VmFail),

    VmentryMsrLoadCtrlFail(VmFail),
    InterruptInjectionFail(VmFail),

    SetupHostFieldsFail(VmFail),
    SetupGuestFieldsFail(VmFail),

    SerialInitFail,
    VmlaunchFail,

    VpidCtrlFail(VmFail),

    InvalidCpuId(u32),

    VcpuNotInitialized,

    /// `KernelArgs.Version` 与当前 hypervisor 不匹配（布局无法确定）。
    UnsupportedKargs,
    /// `KernelArgs.phys_map_base/count` 非法：0 指针、超出上限。
    BadPhysMap,

    /// 另一个 hypervisor 已经设置了 `EFER.SVME`。
    #[allow(dead_code)]
    SvmAlreadyEnabled,
    /// 固件通过 `VM_CR.SVMDIS` 禁用了 SVM。
    #[allow(dead_code)]
    SvmDisabledByBios,
    /// SVM 初始化/进入尚未完成。
    #[allow(dead_code)]
    SvmSetupFail,
    /// `VMRUN` 路径尚未完成。
    #[allow(dead_code)]
    SvmLaunchFail,
}

impl Display for HvError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            HvError::InvalidPrameters => write!(f, "Invalid parameters"),
            HvError::UnsupportedCpu => write!(f, "Unsupported CPU"),
            HvError::NotEnoughMemory => write!(f, "Not enough memory"),
            HvError::VmxIsAlreadyUse => write!(f, "VMX is already in use"),
            HvError::LoadVmxonFail(e) => write!(f, "Failed to load VMXON region: {}", e),
            HvError::LoadVmcsFail(e) => write!(f, "Failed to load VMCS region: {}", e),
            HvError::PinbasedCtrlsFail(e) => write!(f, "Failed to set pin-based controls: {}", e),
            HvError::ProcbasedCtrlsFail(e) => {
                write!(f, "Failed to set processor-based controls: {}", e)
            }
            HvError::ProcbasedCtrls2Fail(e) => {
                write!(f, "Failed to set secondary processor-based controls: {}", e)
            }
            HvError::ExitCtrlsFail(e) => write!(f, "Failed to set VM-exit controls: {}", e),
            HvError::EntryCtrlsFail(e) => write!(f, "Failed to set VM-entry controls: {}", e),
            HvError::ExceptionBitmapCtrlFail(e) => {
                write!(f, "Failed to set exception bitmap: {}", e)
            }
            HvError::PfecCtrlFail(e) => {
                write!(f, "Failed to set page-fault error code mask: {}", e)
            }
            HvError::Cr0ReadCtrlFail(e) => write!(f, "Failed to set CR0 read ctrl: {}", e),
            HvError::Cr4ReadCtrlFail(e) => write!(f, "Failed to set CR4 read ctrl: {}", e),
            HvError::Cr3TargetCtrlFail(e) => write!(f, "Failed to set CR3 target list: {}", e),
            HvError::MsrBitmapCtrlFail(e) => write!(f, "Failed to set MSR bitmap: {}", e),
            #[cfg(feature = "vmx-ept")]
            HvError::EptpCtrlFail(e) => write!(f, "Failed to set EPT pointer: {}", e),
            HvError::VmexitMsrStoreCtrlFail(e) => {
                write!(f, "Failed to set VM-exit MSR store area: {}", e)
            }
            HvError::VmexitMsrLoadCtrlFail(e) => {
                write!(f, "Failed to set VM-exit MSR load area: {}", e)
            }
            HvError::VmentryMsrLoadCtrlFail(e) => {
                write!(f, "Failed to set VM-entry MSR load area: {}", e)
            }
            HvError::InterruptInjectionFail(e) => {
                write!(f, "Failed to set VM-entry interrupt injection field: {}", e)
            }
            HvError::SetupHostFieldsFail(e) => {
                write!(f, "Failed to setup host field: {}", e)
            }
            HvError::SetupGuestFieldsFail(e) => {
                write!(f, "Failed to setup guest field: {}", e)
            }
            HvError::SerialInitFail => {
                write!(f, "Failed to init serial")
            }
            HvError::VmlaunchFail => {
                write!(f, "Failed to vmlaunch")
            }
            HvError::VpidCtrlFail(e) => {
                write!(f, "Failed to setup vpid: {}", e)
            }
            HvError::InvalidCpuId(v) => {
                write!(f, "Invalid cpuid number: {}", v)
            }
            HvError::VcpuNotInitialized => {
                write!(f, "Vcpu not initialized")
            }
            HvError::UnsupportedKargs => {
                write!(f, "Unsupported KernelArgs version")
            }
            HvError::BadPhysMap => {
                write!(f, "Bad physical memory map in KernelArgs")
            }
            HvError::SvmAlreadyEnabled => {
                write!(f, "SVM is already enabled (EFER.SVME)")
            }
            HvError::SvmDisabledByBios => {
                write!(f, "SVM is disabled by firmware (VM_CR.SVMDIS)")
            }
            HvError::SvmSetupFail => {
                write!(f, "SVM setup is not implemented yet")
            }
            HvError::SvmLaunchFail => {
                write!(f, "SVM launch is not implemented yet")
            }
        }
    }
}

impl Error for HvError {}

impl HvError {
    pub fn as_u32(&self) -> u32 {
        match self {
            HvError::InvalidPrameters => 1,
            HvError::UnsupportedCpu => 2,
            HvError::NotEnoughMemory => 3,
            HvError::VmxIsAlreadyUse => 4,
            HvError::LoadVmxonFail(_) => 7,
            HvError::LoadVmcsFail(_) => 8,
            HvError::PinbasedCtrlsFail(_) => 9,
            HvError::ProcbasedCtrlsFail(_) => 10,
            HvError::ProcbasedCtrls2Fail(_) => 11,
            HvError::ExitCtrlsFail(_) => 12,
            HvError::EntryCtrlsFail(_) => 13,
            HvError::ExceptionBitmapCtrlFail(_) => 14,
            HvError::PfecCtrlFail(_) => 15,
            // 16 已退役(原 `TscOffsetCtrlFail`); 编号保持不变, 别重排.
            HvError::Cr0ReadCtrlFail(_) => 17,
            HvError::Cr4ReadCtrlFail(_) => 18,
            HvError::Cr3TargetCtrlFail(_) => 19,
            HvError::MsrBitmapCtrlFail(_) => 20,
            HvError::VmexitMsrStoreCtrlFail(_) => 21,
            HvError::VmexitMsrLoadCtrlFail(_) => 22,
            HvError::VmentryMsrLoadCtrlFail(_) => 23,
            HvError::InterruptInjectionFail(_) => 24,
            HvError::SetupHostFieldsFail(_) => 25,
            HvError::SetupGuestFieldsFail(_) => 26,
            HvError::SerialInitFail => 27,
            HvError::VmlaunchFail => 28,
            HvError::VpidCtrlFail(_) => 29,
            HvError::InvalidCpuId(_) => 30,
            HvError::VcpuNotInitialized => 31,
            #[cfg(feature = "vmx-ept")]
            HvError::EptpCtrlFail(_) => 32,
            HvError::UnsupportedKargs => 33,
            HvError::BadPhysMap => 34,
            HvError::SvmAlreadyEnabled => 35,
            HvError::SvmDisabledByBios => 36,
            HvError::SvmSetupFail => 37,
            HvError::SvmLaunchFail => 38,
        }
    }
}

pub type VmResult<T> = core::result::Result<T, VmFail>;
pub type HvResult<T> = core::result::Result<T, HvError>;
