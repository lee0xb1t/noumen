#[cfg(feature = "vmx-ept")]
pub mod ept;
pub mod exit_handler;
pub mod hypercall;
pub mod exit_info;
pub mod injection;
pub mod instruction;
pub mod msr;
pub mod state;
pub mod stub;
pub mod vmcs;

pub use state::VmxVcpu;

use crate::arch::x86_64::cr;
use crate::error::{HvError, HvResult};
use crate::{
    arch::x86_64::{msr::Ia32FeatureControl, paging::MemoryType},
    klog, klog_verbose,
};

pub fn check_vmx_features() -> HvResult<()> {
    use x86::cpuid::CpuId;

    let cpuid: CpuId = CpuId::new();
    let feat_info = cpuid.get_feature_info();
    match feat_info {
        Some(f) => {
            if !f.has_vmx() {
                return Err(HvError::UnsupportedCpu);
            }
        }
        None => {
            return Err(HvError::UnsupportedCpu);
        }
    }

    let vmx_basic = msr::VmxBasicMsr::read();

    if vmx_basic.vmcs_size_in_bytes() > 4096
        || vmx_basic.memory_type() != MemoryType::WriteBack as u64
        || !vmx_basic.true_controls()
    {
        klog!("Unsupported VMX_BASIC: 0x{:x}", vmx_basic.as_raw());
        return Err(HvError::UnsupportedCpu);
    }

    klog_verbose!("VMX_BASIC: {:#x}", vmx_basic.as_raw());

    let vmx_ept_vpid = msr::VmxEptVpidCap::read();

    // 恒等映射粒度：支持 1 GiB 页就铺满整个物理地址空间；否则用 2 MiB 页，
    // 覆盖 min(MAXPHYADDR, 64 GiB)——64 GiB 是页池能撑住的上限。
    #[cfg(feature = "vmx-ept")]
    {
        let bits = (x86::cpuid::cpuid!(0x8000_0008).eax & 0xff).clamp(32, 52);
        let max_phyaddr = 1u64 << bits;

        let use_1gb = vmx_ept_vpid.pdpte_1gb_pages();
        // 1 GiB 页给大内存机器留 1 TiB 的 MMIO 余量（2 张 PDPT/核）；
        // 2 MiB 页每 1 GiB 要一张 PD 表，只能覆盖 64 GiB。
        let coverage = if use_1gb {
            max_phyaddr.min(1u64 << 40)
        } else {
            max_phyaddr.min(64u64 << 30)
        };

        crate::hv::set_ept_1gb_pages(use_1gb);
        crate::hv::set_ept_coverage(coverage);

        klog_verbose!(
            "EPT: 1gb={} maxphyaddr={:#x} coverage={:#x}",
            use_1gb,
            max_phyaddr,
            coverage
        );
    }

    #[cfg(feature = "vmx-ept")]
    if !vmx_ept_vpid.page_walk_length_4()
        || !vmx_ept_vpid.memory_type_write_back()
        || !vmx_ept_vpid.invept()
        || !vmx_ept_vpid.pde_2mb_pages()
        || !vmx_ept_vpid.execute_only_pages()
        || !vmx_ept_vpid.invept_all_contexts()
    {
        klog!(
            "Unsupported VMX_EPT_VPID_CAP: 0x{:x}",
            vmx_ept_vpid.as_raw()
        );
        return Err(HvError::UnsupportedCpu);
    }

    if !vmx_ept_vpid.invvpid() {
        klog!(
            "Unsupported VMX_EPT_VPID_CAP: 0x{:x}",
            vmx_ept_vpid.as_raw()
        );
        return Err(HvError::UnsupportedCpu);
    }

    klog_verbose!("VMX_EPT_VPID_CAP: {:#x}", vmx_ept_vpid.as_raw());

    let feature_control = Ia32FeatureControl::read();
    if feature_control.lock_bit() && feature_control.enable_vmx_inside_smx() {
        klog!(
            "Unsupported FEATURE_CONTROL: 0x{:x}",
            feature_control.as_raw()
        );
        return Err(HvError::UnsupportedCpu);
    }

    Ok(())
}

pub fn is_enabled() -> bool {
    cr::Cr4::read().vmx_enable()
}
