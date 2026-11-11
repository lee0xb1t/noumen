use crate::{
    arch::x86_64::{
        cr, msr,
        safe_routine::{self, HostExceptionInfo},
        vmx::vmcs::{self, control},
    },
    error::{HvError, HvResult},
    hv, klog, klog_verbose,
};

use super::{CetMsrs, VmxVcpu};

impl VmxVcpu {
    pub(super) fn setup_vmcs(&mut self) -> HvResult<()> {
        self.configure_execution_controls()?;
        self.configure_exit_controls()?;
        self.configure_entry_controls()?;
        self.configure_cet_state()?;
        self.configure_exception_bitmap()?;
        self.configure_cr_shadows()?;
        self.configure_address_space()?;
        self.configure_vmexit_msr()?;
        self.configure_injection()?;
        Ok(())
    }

    fn configure_execution_controls(&mut self) -> HvResult<()> {
        let mut pin_based_ctrls = vmcs::control::PinbasedCtls::default();
        pin_based_ctrls.set_virtual_nmi(cfg!(feature = "native-bp"));
        pin_based_ctrls.set_nmi_exiting(cfg!(feature = "native-bp"));
        if let Err(e) = pin_based_ctrls.write_adjust() {
            return Err(HvError::PinbasedCtrlsFail(e));
        }

        let mut proc_based_ctrls = control::ProcBasedCtls::default();
        // IMPORTANT: 不要拦 `mov cr3`. VPID 打开后, CR3 的标签和失效都由硬件
        // 自己维护; 拦下来只会把每次写 CR3 变成「一次退出 + 一次 INVVPID」,
        // 实测 11 万次/s, 整个 TLB 被反复清空, 系统直接卡死.
        // 真要开(CR3 introspection), 必须同时在 `emulate_mov_to_cr3` 里补
        // INVVPID, 否则 guest 会继续用已失效的翻译.
        proc_based_ctrls.set_cr3_load_exiting(false);
        proc_based_ctrls.set_use_msr_bitmaps(true);
        // TSC 物理透传：per-vCPU offset 会导致多核时钟漂移（Windows HAL 0x5C），不使用。
        proc_based_ctrls.set_use_tsc_offsetting(false);
        // RDTSC/RDTSCP 不虚拟化；MSR bitmap 不拦 IA32_TSC。
        proc_based_ctrls.set_rdtsc_exiting(false);
        proc_based_ctrls.set_activate_secondary_controls(true);
        if let Err(e) = proc_based_ctrls.write_adjust() {
            return Err(HvError::ProcbasedCtrlsFail(e));
        }

        // 读回确认 RDTSC exiting 没有被 CPU/外层 VMM 强制打开。
        let proc_readback =
            control::ProcBasedCtls::read().map_err(HvError::ProcbasedCtrlsFail)?;
        klog_verbose!("VMX: proc_ctls={:#x}", proc_readback.as_raw());
        if proc_readback.rdtsc_exiting() {
            klog!("VMX: rdtsc_exiting is forced on by CPU/outer VMM; RDTSC will VM-exit");
        }

        let mut proc_based_ctrls2 = control::ProcBasedCtls2::default();
        proc_based_ctrls2.set_enable_ept(cfg!(feature = "vmx-ept"));
        proc_based_ctrls2.set_enable_rdtscp(true);
        // VPID and unrestricted guest are EPT-dependent VM-entry controls:
        // setting either without EPT makes VMLAUNCH fail with "invalid control
        // fields" (error 7).
        proc_based_ctrls2.set_enable_vpid(cfg!(feature = "vmx-ept"));
        proc_based_ctrls2.set_enable_invpcid(true);
        proc_based_ctrls2.set_enable_xsaves(true);
        proc_based_ctrls2.set_enable_user_wait_pause(true);
        proc_based_ctrls2.set_conceal_vmx_from_pt(true);
        // CET 的硬件行为依赖 unrestricted guest（KVM 在
        // vmx_set_cpu_caps 里也是这个条件），跟着打开。
        proc_based_ctrls2.set_unrestricted_guest(cfg!(feature = "vmx-ept"));
        if let Err(e) = proc_based_ctrls2.write_adjust() {
            return Err(HvError::ProcbasedCtrls2Fail(e));
        }

        // IMPORTANT: `write_adjust()` 会静默清掉 CPU(或外层 VMM)不允许的控制位.
        // EPT 被清掉时 EPTP 照样写进 VMCS, 但 guest 会跑在宿主页表上 -- 看起来
        // “能用”, 实际内存虚拟化整个没生效. 必须在进 guest 之前发现它.
        let readback = control::ProcBasedCtls2::read().map_err(HvError::ProcbasedCtrls2Fail)?;

        if cfg!(feature = "vmx-ept") && !readback.enable_ept() {
            klog!("EPT: enable_ept was cleared by the CPU/outer VMM, refusing to continue");
            return Err(HvError::UnsupportedCpu);
        }

        // 不拦 `mov cr3` 之后, CR3 写全靠 VPID 的标签语义; 没有它 INVVPID 也
        // 成了空操作, guest 会命中废弃翻译. 只有 EPT 构建才要求 VPID;
        // no-EPT 构建本来就把 VPID 关掉, readback=false 是预期结果.
        if cfg!(feature = "vmx-ept") && !readback.enable_vpid() {
            klog!("VMX: enable_vpid was cleared by the CPU/outer VMM, refusing to continue");
            return Err(HvError::UnsupportedCpu);
        }

        // TSC 无虚拟化：guest 直接读物理 TSC。

        Ok(())
    }

    fn configure_exit_controls(&mut self) -> HvResult<()> {
        let mut exit_ctrls = control::VmExitCtls::default();
        exit_ctrls.set_save_debug_controls(true);
        exit_ctrls.set_host_address_space_size(true);
        exit_ctrls.set_save_ia32_pat(true);
        exit_ctrls.set_load_ia32_pat(true);
        exit_ctrls.set_load_ia32_perf_global_ctrl(false);
        exit_ctrls.set_conceal_vmx_from_pt(true);

        if self.cached.cet_supported {
            exit_ctrls.set_load_ia32_cet_state(true);
        }

        if let Err(e) = exit_ctrls.write_adjust() {
            return Err(HvError::ExitCtrlsFail(e));
        }
        Ok(())
    }

    fn configure_entry_controls(&mut self) -> HvResult<()> {
        let mut entry_ctrls = control::VmEntryCtls::default();
        entry_ctrls.set_ia32e_mode_guest(true);
        entry_ctrls.set_load_debug_controls(true);
        entry_ctrls.set_load_ia32_pat(true);
        entry_ctrls.set_load_ia32_perf_global_ctrl(false);
        entry_ctrls.set_conceal_vmx_from_pt(true);

        if self.cached.cet_supported {
            entry_ctrls.set_load_cet_state(true);
        }

        if let Err(e) = entry_ctrls.write_adjust() {
            return Err(HvError::EntryCtrlsFail(e));
        }
        Ok(())
    }

    /// 初始化 CET VMCS 字段；只有加载时 CET 已开才暴露。
    fn configure_cet_state(&mut self) -> HvResult<()> {
        self.cached.cet_virtualized = false;

        let real_cr4 = cr::Cr4::read().bits();
        self.cached.cet_active_at_setup = real_cr4 & (1 << 23) != 0;

        if !self.cached.cet_supported {
            return Ok(());
        }

        self.cet = CetMsrs::read();

        let entry = control::VmEntryCtls::read().map_err(HvError::EntryCtrlsFail)?;
        let exit = control::VmExitCtls::read().map_err(HvError::ExitCtrlsFail)?;

        if !entry.load_cet_state() || !exit.load_ia32_cet_state() {
            klog!("CET: VMX load-CET controls unavailable, CET hidden from guest");
            return Ok(());
        }

        let mut exception_info = HostExceptionInfo::new();
        let s_cet = safe_routine::rdmsr_safe(&mut exception_info, msr::IA32_S_CET).unwrap_or(0);
        let intr_ssp_table =
            safe_routine::rdmsr_safe(&mut exception_info, msr::IA32_INTERRUPT_SSP_TABLE_ADDR)
                .unwrap_or(0);

        let ssp = if self.cached.cet_active_at_setup {
            cr::read_ssp()
        } else {
            0
        };

        vmcs::host::Ia32SCet::from_raw(0)
            .write()
            .map_err(HvError::SetupHostFieldsFail)?;
        vmcs::host::Ssp::from_raw(0)
            .write()
            .map_err(HvError::SetupHostFieldsFail)?;
        vmcs::host::Ia32InterruptSspTable::from_raw(0)
            .write()
            .map_err(HvError::SetupHostFieldsFail)?;

        vmcs::guest::Ia32SCet::from_raw(s_cet)
            .write()
            .map_err(HvError::SetupGuestFieldsFail)?;
        vmcs::guest::Ssp::from_raw(ssp)
            .write()
            .map_err(HvError::SetupGuestFieldsFail)?;
        vmcs::guest::Ia32InterruptSspTable::from_raw(intr_ssp_table)
            .write()
            .map_err(HvError::SetupGuestFieldsFail)?;

        // 加载时 CET 未开就不暴露：之后才打开会让已运行的线程失配。
        self.cached.cet_virtualized = self.cached.cet_active_at_setup;

        klog!(
            "CET: virtualized={} active_at_setup={}",
            self.cached.cet_virtualized,
            self.cached.cet_active_at_setup
        );

        Ok(())
    }

    fn configure_exception_bitmap(&mut self) -> HvResult<()> {
        // 异常位图。
        //
        // 编译进 EPT hook 引擎时始终拦截 #BP（vector 3）：隐藏断点是 hook
        // 私有页副本里的一个 0xCC 字节，要瞒住 guest，只能在这里消费产生的
        // #BP 并单步执行原指令。guest 自己代码触发的 #BP 会原样注入回去。
        // #CP 也拦：注入前记录 guest CET 状态。
        let mut bits = 0u32;
        if cfg!(feature = "native-bp") {
            bits |= 1 << 3;
            if self.cached.cet_supported {
                bits |= 1 << 21;
            }
        }
        let exception_bitmap = control::ExceptionBitmap::from_raw(bits);

        if let Err(e) = exception_bitmap.write() {
            return Err(HvError::ExceptionBitmapCtrlFail(e));
        }

        // page fualt error code
        let pfec = control::Pfec::default();
        if let Err(e) = pfec.write() {
            return Err(HvError::PfecCtrlFail(e));
        }
        Ok(())
    }

    fn configure_cr_shadows(&mut self) -> HvResult<()> {
        // mask 位是 host-owned；全 1 会把 guest CR4 钉死在 HOST_CR4。
        let cr0 = cr::Cr0::read().bits();

        let mut cr0_shadow = control::Cr0Shadow::default();
        cr0_shadow.set_raw_mask(
            self.cached.vmx_cr0_fixed0
                | !self.cached.vmx_cr0_fixed1
                | cr::Cr0::CR0_CACHE_DISABLE_FLAG
                | cr::Cr0::CR0_WRITE_PROTECT_FLAG,
        );
        cr0_shadow.set_raw_shadow(cr0 as u64);

        if let Err(e) = cr0_shadow.write() {
            return Err(HvError::Cr0ReadCtrlFail(e));
        }

        // cr4 shadow
        let cr4 = cr::Cr4::read().bits();

        let mut cr4_shadow = control::Cr4Shadow::default();
        cr4_shadow.set_raw_mask(self.cached.vmx_cr4_fixed0 | !self.cached.vmx_cr4_fixed1);
        cr4_shadow.set_raw_shadow((cr4 & !0x2000) as u64);

        if let Err(e) = cr4_shadow.write() {
            return Err(HvError::Cr4ReadCtrlFail(e));
        }

        Ok(())
    }

    fn configure_address_space(&mut self) -> HvResult<()> {
        // cr3 target 只在 `cr3_load_exiting` 打开时才有意义。
        let system_cr3 = hv::system_cr3();
        let mut cr3_target = control::Cr3Target::new();
        cr3_target.add(system_cr3 as u64);
        if let Err(e) = cr3_target.write() {
            return Err(HvError::Cr3TargetCtrlFail(e));
        }

        // msr bitmap
        let msr_bitmap_pa = self.msr_bitmap.phys();
        let msr_bitmap = control::MsrBitmap::from_phys(msr_bitmap_pa);
        if let Err(e) = msr_bitmap.write() {
            return Err(HvError::MsrBitmapCtrlFail(e));
        }

        klog_verbose!("msr bitmap: pa={:#x}", msr_bitmap_pa.as_u64());

        // ept pointer
        #[cfg(feature = "vmx-ept")]
        {
            let eptp = control::Eptp::from_eptp(self.ept.eptp());
            if let Err(e) = eptp.write() {
                return Err(HvError::EptpCtrlFail(e));
            }

            klog!("EPT: enabled in VMCS, EPTP={:#x}", self.ept.eptp().as_raw());
        }

        // vpid
        let vpid = control::Vpid::from_raw(self.vpid);
        if let Err(e) = vpid.write() {
            return Err(HvError::VpidCtrlFail(e));
        }
        Ok(())
    }

    fn configure_vmexit_msr(&mut self) -> HvResult<()> {
        /////////// VM EXIT ///////////
        // 没有 MSR 装载区: 计数显式写 0, 不依赖 VMCS 页的零初值.
        let vmexit_msr_load = control::VmexitMsrLoad::default();
        if let Err(e) = vmexit_msr_load.write() {
            return Err(HvError::VmexitMsrLoadCtrlFail(e));
        }

        // VM-exit MSR-store 不配置：count/address 写 0，避免继承 VMCS 的旧值。
        let vmexit_msr_store = control::VmexitMsrStore::default();
        if let Err(e) = vmexit_msr_store.write() {
            return Err(HvError::VmexitMsrStoreCtrlFail(e));
        }

        let vmentry_msr_load = control::VmentryMsrLoad::default();
        if let Err(e) = vmentry_msr_load.write() {
            return Err(HvError::VmentryMsrLoadCtrlFail(e));
        }
        Ok(())
    }

    fn configure_injection(&mut self) -> HvResult<()> {
        let interrupt_injection = vmcs::control::InterruptInjection::default();
        if let Err(e) = interrupt_injection.write() {
            return Err(HvError::InterruptInjectionFail(e));
        }
        Ok(())
    }
}
