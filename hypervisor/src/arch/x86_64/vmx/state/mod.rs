use crate::{
    arch::x86_64::{
        host_state::HostState,
        memory,
        vcpu::Vcpu,
        vmx::{instruction, vmcs},
    },
    error::{HvError, HvResult},
    klog,
};

#[cfg(feature = "vmx-ept")]
use crate::arch::x86_64::paging;

#[cfg(debug_assertions)]
use crate::{arch::x86_64::vmx, klog_verbose};

#[cfg(feature = "vmx-ept")]
use crate::arch::x86_64::vmx::ept;

mod controls;
mod guest;
mod host;
mod init;
pub mod perf;
pub mod selective_tsc;

/// TSC 虚拟化策略（写在文档里，代码里没有开关）：
///
/// 硬件 `use_tsc_offsetting` 已经**不再使用**。`TSC_OFFSET` 是 per-vCPU 的，
/// 12 个核各自扣减 host 时间会累积时钟差，Windows 的 HAL 定时器跨核比较时
/// 直接 `0x5C`。现在只做**选择性隐藏**（见 [`selective_tsc`]）：拦 `RDTSC`，
/// 只对目标进程/模块的读取做减法，其它读取原样返回物理 TSC（天然跨核一致）。

/// APERF/MPERF 虚拟化开关：MSR bitmap 拦截 + `VirtualPerfCounters`。
///
/// 现在是**按需同步**（只在 guest 读时累加），退出路径零开销，所以默认开。
pub const ENABLE_PERF: bool = true;

#[derive(Debug)]
pub struct VmxCachedData {
    pub xcr0_unsupported_mask: u64,
    /// Guest-visible `IA32_FEATURE_CONTROL`. Captured before `enable_vmx()`
    /// touched the real MSR and spoofed so the guest sees BIOS having locked
    /// the MSR with VT-x and SMX disabled (see `emulate_rdmsr`/`emulate_vmxon`).
    pub feature_control: u64,
    pub vmx_misc: u64,

    pub vmx_cr0_fixed0: u64,
    pub vmx_cr0_fixed1: u64,
    pub vmx_cr4_fixed0: u64,
    pub vmx_cr4_fixed1: u64,

    pub nr_phys_addr_bits: u32,

    /// CPUID.01H:ECX[6] (SMX), used by the `MOV to CR4` #GP check.
    pub smx_supported: bool,

    /// `IA32_VMX_PROCBASED_CTLS` 允许 monitor trap flag。
    ///
    /// EPT 断点需要在重新 armed 之前，用真实页把原指令执行一次。没有 MTF 时
    /// 断点要么死循环，要么悄悄失效，因此 CPU 不允许该控制位时直接拒绝安装。
    pub monitor_trap_flag: bool,

    /// CPUID.07H:0:ECX[7] (CET_SS).
    pub cet_supported: bool,

    /// VMX load-CET controls are active.
    pub cet_virtualized: bool,

    /// CET was already enabled (CR4.CET=1) when this vCPU was set up.
    pub cet_active_at_setup: bool,
}

/// 没有 VMCS 字段的 CET MSR；写操作同时镜像到硬件。
#[derive(Clone, Copy, Debug, Default)]
pub struct CetMsrs {
    pub u_cet: u64,
    pub pl0_ssp: u64,
    pub pl1_ssp: u64,
    pub pl2_ssp: u64,
    pub pl3_ssp: u64,
}

impl CetMsrs {
    pub const fn new() -> Self {
        Self {
            u_cet: 0,
            pl0_ssp: 0,
            pl1_ssp: 0,
            pl2_ssp: 0,
            pl3_ssp: 0,
        }
    }

    pub fn read() -> Self {
        let mut info = crate::arch::x86_64::safe_routine::HostExceptionInfo::new();
        let mut msr = |index: u32| {
            crate::arch::x86_64::safe_routine::rdmsr_safe(&mut info, index).unwrap_or(0)
        };

        Self {
            u_cet: msr(crate::arch::x86_64::msr::IA32_U_CET),
            pl0_ssp: msr(crate::arch::x86_64::msr::IA32_PL0_SSP),
            pl1_ssp: msr(crate::arch::x86_64::msr::IA32_PL1_SSP),
            pl2_ssp: msr(crate::arch::x86_64::msr::IA32_PL2_SSP),
            pl3_ssp: msr(crate::arch::x86_64::msr::IA32_PL3_SSP),
        }
    }

    /// Value of a CET MSR, keyed by MSR index.
    pub fn get(&self, index: u32) -> Option<u64> {
        use crate::arch::x86_64::msr;

        match index {
            msr::IA32_U_CET => Some(self.u_cet),
            msr::IA32_PL0_SSP => Some(self.pl0_ssp),
            msr::IA32_PL1_SSP => Some(self.pl1_ssp),
            msr::IA32_PL2_SSP => Some(self.pl2_ssp),
            msr::IA32_PL3_SSP => Some(self.pl3_ssp),
            _ => None,
        }
    }

    /// Stores a CET MSR and returns whether `index` is one of them.
    pub fn set(&mut self, index: u32, value: u64) -> bool {
        use crate::arch::x86_64::msr;

        match index {
            msr::IA32_U_CET => self.u_cet = value,
            msr::IA32_PL0_SSP => self.pl0_ssp = value,
            msr::IA32_PL1_SSP => self.pl1_ssp = value,
            msr::IA32_PL2_SSP => self.pl2_ssp = value,
            msr::IA32_PL3_SSP => self.pl3_ssp = value,
            _ => return false,
        }

        true
    }
}

impl VmxCachedData {
    pub const fn new() -> Self {
        Self {
            xcr0_unsupported_mask: 0,
            feature_control: 0,
            vmx_misc: 0,
            vmx_cr0_fixed0: 0,
            vmx_cr0_fixed1: 0,
            vmx_cr4_fixed0: 0,
            vmx_cr4_fixed1: 0,
            nr_phys_addr_bits: 0,
            smx_supported: false,
            monitor_trap_flag: false,
            cet_supported: false,
            cet_virtualized: false,
            cet_active_at_setup: false,
        }
    }
}

pub const GUEST_VPID: u64 = 1;

/// Per-CPU VMX state.
///
/// NOTE: keep this (and its owner) non-`Clone`/`Copy` and never move it after
/// `setup_vmcs()`: the VMCS holds physical addresses of the regions owned by
/// `vmxon`, `vmcs`, `msr_bitmap` and `ept`.
pub struct VmxVcpu {
    vmxon: memory::Page<vmcs::types::Vmxon>,
    vmcs: memory::Page<vmcs::types::Vmcs>,
    pub(super) msr_bitmap: memory::Page<vmcs::types::VmxMsrBitmap>,
    /// VM-exit MSR-store 区域：硬件每次退出把 `IA32_TSC` 写在这里，
    /// 作为 TSC 隐藏账本的「退出时刻」，见 [`selective_tsc::MsrStoreArea`]。
    pub msr_store: memory::Page<selective_tsc::MsrStoreArea>,
    #[cfg(feature = "vmx-ept")]
    pub ept: ept::EptData,
    pub cached: VmxCachedData,
    pub vpid: u16,
    /// CET MSRs without a VMCS field, see [`CetMsrs`].
    pub cet: CetMsrs,
    /// Guest-visible APERF/MPERF, see [`perf::VirtualPerfCounters`].
    pub perf: perf::VirtualPerfCounters,
    /// 选择性 TSC 隐藏：每核的 host 时间账本，见 [`selective_tsc::SelectiveTsc`]。
    pub selective_tsc: selective_tsc::SelectiveTsc,
    /// 本核 EPT 已经同步到的代数, 见 [`ept::memory_type_generation`].
    #[cfg(feature = "vmx-ept")]
    pub ept_generation: u32,
    /// 本核 EPT 已经应用到的 HideSelf generation。
    #[cfg(feature = "vmx-ept")]
    pub hide_self_generation: u32,
    /// 本核断点 owner generation；owner 变化只影响断点页。
    #[cfg(feature = "vmx-ept")]
    pub breakpoint_generation: u32,
    /// 正在被 MTF 单步放行的 hook 页 (EPT 断点命中后临时恢复原页).
    ///
    /// IMPORTANT: 只有本核能看到这个状态, 而 MTF 也只在本核的 VMCS 里,
    /// 所以它必须和 `ept` 存在同一个 `VmxVcpu` 上.
    #[cfg(feature = "vmx-ept")]
    pub hook_pending: Option<ept::hook::HookPending>,
    /// 隐藏页数据态的私有全 0 页；每核一份，避免多核写互相可见。
    #[cfg(feature = "vmx-ept")]
    pub hide_zero: memory::Page<[u8; memory::PAGE_SIZE]>,
    /// 隐藏页自访问单步用的私有页；每核一份，MTF 期间只属于本核。
    #[cfg(feature = "vmx-ept")]
    pub hide_shadow: memory::Page<[u8; memory::PAGE_SIZE]>,
}

impl VmxVcpu {
    pub const fn new() -> Self {
        Self {
            vmxon: memory::Page::empty(),
            vmcs: memory::Page::empty(),
            msr_bitmap: memory::Page::empty(),
            msr_store: memory::Page::empty(),
            #[cfg(feature = "vmx-ept")]
            ept: ept::EptData::new(),
            cached: VmxCachedData::new(),
            vpid: 0,
            cet: CetMsrs::new(),
            perf: perf::VirtualPerfCounters::new(),
            selective_tsc: selective_tsc::SelectiveTsc::new(),
            #[cfg(feature = "vmx-ept")]
            ept_generation: 0,
            #[cfg(feature = "vmx-ept")]
            hide_self_generation: 0,
            #[cfg(feature = "vmx-ept")]
            breakpoint_generation: 0,
            #[cfg(feature = "vmx-ept")]
            hook_pending: None,
            #[cfg(feature = "vmx-ept")]
            hide_zero: memory::Page::empty(),
            #[cfg(feature = "vmx-ept")]
            hide_shadow: memory::Page::empty(),
        }
    }

    pub fn init(&mut self) -> HvResult<()> {
        self.vmxon = memory::alloc()?;
        self.vmcs = memory::alloc()?;
        self.msr_bitmap = memory::alloc()?;
        self.msr_store = memory::alloc()?;
        #[cfg(feature = "vmx-ept")]
        {
            self.ept.init()?;
            self.hide_zero = memory::alloc()?;
            self.hide_shadow = memory::alloc()?;
            self.hide_zero.fill(0);
            self.hide_shadow.fill(0);
        }

        Ok(())
    }

    /// 其它核改过 MTRR / CR0.CD 时, 把本核的 EPT 类型补上并刷掉旧翻译.
    ///
    /// IMPORTANT: 每次 VM-exit 都会调用; 代数没变时只是一次原子读, 别在这里
    /// 做别的事. 真正重算只在别的核改过 MTRR 之后发生一次.
    #[cfg(feature = "vmx-ept")]
    #[inline]
    pub fn sync_ept_memory_types(&mut self) {
        let generation = ept::memory_type_generation();
        if self.ept_generation == generation {
            return;
        }

        let cache_disable = vmcs::guest::Cr0::read_effective()
            .map(|cr0| cr0.cache_disable())
            .unwrap_or(false);

        if cache_disable {
            self.ept.set_memory_type(paging::MemoryType::Uncacheable);
        } else {
            self.ept.update_memory_type();
        }

        instruction::Invept::single(self.ept.eptp().as_raw());
        self.ept_generation = generation;
    }

    pub fn setup(&mut self, host: &HostState, vcpu: *const Vcpu) -> HvResult<()> {
        #[cfg(debug_assertions)]
        klog_verbose!("{:#?}\n", vmx::msr::VmxBasicMsr::read());

        self.prepare()?;
        self.load_regions()?;
        self.setup_vmcs()?;
        self.setup_host(host, vcpu)?;
        self.setup_guest()?;

        Ok(())
    }

    pub fn vmlaunch(&self) -> HvResult<()> {
        use crate::arch::x86_64::vmx::stub;
        let vm_launch = unsafe { stub::vm_launch() };
        if !vm_launch {
            let inst_err = instruction::InstructionError::read();
            klog!(
                "VMLAUNCH failed. Instruction error = 0x{:x}.",
                inst_err.unwrap().as_raw()
            );
            instruction::vmxoff().unwrap();
            return Err(HvError::VmlaunchFail);
        }

        Ok(())
    }
}
