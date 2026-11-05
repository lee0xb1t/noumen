use crate::{
    arch::x86_64::{
        cr, msr,
        vmx::{self, instruction},
    },
    error::{HvError, HvResult},
    klog, klog_verbose,
};

#[cfg(feature = "vmx-ept")]
use crate::arch::x86_64::mtrr;

use super::VmxVcpu;

impl VmxVcpu {
    pub(super) fn prepare(&mut self) -> HvResult<()> {
        self.cache_data()?;
        klog_verbose!("VmxVcpu cached data = {:#?}", self.cached);
        self.enable_vmx();
        Ok(())
    }

    pub(super) fn load_regions(&mut self) -> HvResult<()> {
        self.load_vmxon()?;
        self.load_vmcs()?;
        self.setup_msr_bitmap();

        #[cfg(feature = "vmx-ept")]
        self.ept.prepare();

        Ok(())
    }

    pub(super) fn setup_msr_bitmap(&mut self) {
        // The guest must observe the spoofed, locked IA32_FEATURE_CONTROL
        // cached in `cache_data()`, not the hardware MSR that `enable_vmx()`
        // left locked with VMXON-outside-SMX enabled.
        self.msr_bitmap
            .enable_exit_for_rdmsr(x86::msr::IA32_FEATURE_CONTROL);

        // VMX capability MSRs do not exist for a guest that has no VT-x. If a
        // detector ignores the CPUID/CR4 bits and probes them directly, route
        // the read to `emulate_rdmsr` and inject #GP instead of leaking the
        // host's capability set.
        for msr in 0x480..=0x491 {
            self.msr_bitmap.enable_exit_for_rdmsr(msr);
        }

        #[cfg(feature = "vmx-ept")]
        {
            self.msr_bitmap
                .enable_exit_for_wrmsr(x86::msr::IA32_MTRR_DEF_TYPE);

            let cap = mtrr::MtrrCap::read();

            if cap.fixed_range_supported() {
                for msr in mtrr::FIXED_MTRR_MSRS {
                    self.msr_bitmap.enable_exit_for_wrmsr(msr);
                }
            }

            for i in 0..cap.variable_range_count() as u32 {
                self.msr_bitmap
                    .enable_exit_for_wrmsr(x86::msr::IA32_MTRR_PHYSBASE0 + i * 2);
                self.msr_bitmap
                    .enable_exit_for_wrmsr(x86::msr::IA32_MTRR_PHYSMASK0 + i * 2);
            }

            klog_verbose!(
                "msr bitmap: IA32_FEATURE_CONTROL read exits, MTRR write exits ({} variable ranges, fixed={})",
                cap.variable_range_count(),
                cap.fixed_range_supported()
            );
        }

        #[cfg(not(feature = "vmx-ept"))]
        klog_verbose!("msr bitmap: IA32_FEATURE_CONTROL read exits");

        // 只有 S_CET/ISST 要拦写：它们每次 VM entry 都从 VMCS 字段重新装载。
        // U_CET/PL0..PL3_SSP 没有 VMCS 字段，硬件自己维护，透传即可——拦截会
        // 让 guest 每次 syscall 写 PL3_SSP 都触发 VM exit。
        if self.cached.cet_supported {
            use crate::arch::x86_64::msr;

            for msr in [msr::IA32_S_CET, msr::IA32_INTERRUPT_SSP_TABLE_ADDR] {
                self.msr_bitmap.enable_exit_for_wrmsr(msr);
            }
        }

        // APERF/MPERF 是物理计数器：读/写都拦截，guest 只看到自己的运行时间。
        if super::ENABLE_PERF {
            self.perf.init();

            if self.perf.supported {
                for msr in [x86::msr::IA32_APERF, x86::msr::IA32_MPERF] {
                    self.msr_bitmap.enable_exit_for_rdmsr(msr);
                    self.msr_bitmap.enable_exit_for_wrmsr(msr);
                }
            }
        }
    }

    pub(super) fn cache_data(&mut self) -> HvResult<()> {
        let cpuid_0d_00 = x86::cpuid::cpuid!(0xd, 0);
        self.cached.xcr0_unsupported_mask =
            !(((cpuid_0d_00.edx as u64) << 32) | cpuid_0d_00.eax as u64);

        self.cached.vmx_misc = vmx::msr::Ia32VmxMisc::read().as_raw();

        // Build the guest-visible IA32_FEATURE_CONTROL here, before
        // `enable_vmx()` locks the real MSR and enables VMXON-outside-SMX.
        // Keep every other firmware-programmed bit (SGX, LMCE, ...) so the
        // guest still sees the platform it booted on, but force the lock bit
        // and clear the VT-x/SMX enables: the same state `emulate_vmxon`
        // reports by injecting #GP(0).
        let mut feature_control = msr::Ia32FeatureControl::read();
        feature_control.set_lock_bit(true);
        feature_control.set_enable_vmx_inside_smx(false);
        feature_control.set_enable_vmx_outside_smx(false);
        feature_control.set_senter_global_enable(false);
        feature_control.set_senter_local_function_enables(0);
        self.cached.feature_control = feature_control.as_raw();

        self.cached.vmx_cr0_fixed0 = cr::Cr0::fixed0().bits();
        self.cached.vmx_cr0_fixed1 = cr::Cr0::fixed1().bits();
        self.cached.vmx_cr4_fixed0 = cr::Cr4::fixed0().bits();
        self.cached.vmx_cr4_fixed1 = cr::Cr4::fixed1().bits();

        let cpuid_eax_80000008 = x86::cpuid::cpuid!(0x80000008);
        self.cached.nr_phys_addr_bits = cpuid_eax_80000008.eax & 0xff;

        // Boot-time capability probe: `_start` already rejected CPUs without
        // VMX, so this only has to answer "does this CPU support SMX?".
        self.cached.smx_supported = x86::cpuid::CpuId::new()
            .get_feature_info()
            .is_some_and(|info| info.has_smx());

        // capability MSR 的高半部分报告允许置 1 的位。MTF 是可选的主控制位：
        // CPU 不允许时，EPT 断点会拒绝安装。
        let vmx_basic = vmx::msr::VmxBasicMsr::read();
        let proc_based = if vmx_basic.true_controls() {
            vmx::msr::VmxTrueProcBasedCtls::read().as_raw()
        } else {
            vmx::msr::VmxProcBasedCtls::read().as_raw()
        };
        self.cached.monitor_trap_flag = (proc_based >> 32) & (1 << 27) != 0;

        let cpuid_7_0 = x86::cpuid::cpuid!(7, 0);
        self.cached.cet_supported = (cpuid_7_0.ecx & (1 << 7)) != 0;

        Ok(())
    }

    pub(super) fn enable_vmx(&self) {
        unsafe {
            x86::irq::disable();
        }

        let mut feature_control = msr::Ia32FeatureControl::read();
        if !feature_control.lock_bit() {
            feature_control.set_enable_vmx_inside_smx(false);
            feature_control.set_enable_vmx_outside_smx(true);
            feature_control.set_lock_bit(true);
            feature_control.write();
        } else {
            if !feature_control.enable_vmx_outside_smx() {
                panic!(
                    "VMX is locked but disabled outside SMX, {:#?}",
                    feature_control
                );
            }
        }

        let mut cr0 = cr::Cr0::read();
        let cr0_fixed0 = cr::Cr0::fixed0();
        let cr0_fixed1 = cr::Cr0::fixed1();
        cr0 = cr::Cr0(cr0.0 | cr0_fixed0.0);
        cr0 = cr::Cr0(cr0.0 & cr0_fixed1.0);
        cr0.write();

        let mut cr4 = cr::Cr4::read();
        cr4.set_vmx_enable(true);

        let cr4_fixed0 = cr::Cr4::fixed0();
        let cr4_fixed1 = cr::Cr4::fixed1();
        cr4 = cr::Cr4(cr4.0 | cr4_fixed0.0);
        cr4 = cr::Cr4(cr4.0 & cr4_fixed1.0);
        cr4.write();

        unsafe {
            x86::irq::enable();
        }

        klog_verbose!("\nVMX is enabled.");

        klog_verbose!("feature_control={:#?}", msr::Ia32FeatureControl::read());

        klog_verbose!("cr0={:#?}", cr::Cr0::read());

        klog_verbose!("cr4={:#?}", cr::Cr4::read());
    }

    pub(super) fn load_vmxon(&mut self) -> HvResult<()> {
        let vmx_basic = vmx::msr::VmxBasicMsr::read();

        let vmxon_va = self.vmxon.virt();
        let vmxon_pa = self.vmxon.phys();

        self.vmxon
            .header
            .set_revision_id(vmx_basic.revision_id() as u32);

        if let Err(e) = instruction::vmxon(&vmxon_pa) {
            klog!(
                "Failed to load vmxon region, va=0x{:x} pa=0x{:x}",
                vmxon_va.as_u64(),
                vmxon_pa.as_u64()
            );
            return Err(HvError::LoadVmxonFail(e));
        }

        instruction::Invept::all(0);

        klog_verbose!(
            "vmxon: va={:#x} pa={:#x}",
            vmxon_va.as_u64(),
            vmxon_pa.as_u64()
        );

        Ok(())
    }

    pub(super) fn load_vmcs(&mut self) -> HvResult<()> {
        let vmx_basic = vmx::msr::VmxBasicMsr::read();

        let vmcs_pa = self.vmcs.phys();

        self.vmcs
            .header
            .set_revision_id(vmx_basic.revision_id() as u32);

        if let Err(e) = instruction::vmclear(&vmcs_pa) {
            return Err(HvError::LoadVmcsFail(e));
        }

        if let Err(e) = instruction::vmptrld(&vmcs_pa) {
            return Err(HvError::LoadVmcsFail(e));
        }

        klog_verbose!("vmcs: pa={:#x}", vmcs_pa.as_u64());

        Ok(())
    }
}
