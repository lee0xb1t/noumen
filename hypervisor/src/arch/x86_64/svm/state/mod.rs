use crate::{
    arch::x86_64::{
        guest_context::GuestContext,
        host_state::HostState,
        memory::Page,
        vcpu::Vcpu,
    },
    error::{HvError, HvResult},
    klog, klog_verbose,
};

#[cfg(feature = "svm-npt")]
use crate::arch::x86_64::{addr::PhysAddr, memory::PAGE_SIZE};
#[cfg(feature = "svm-npt")]
use super::{hook::HookPending, npt::{NptData, NptView}, tlb};
use super::{msrpm::MsrpmPage, stub, vmcb::Vmcb};

mod controls;
mod init;
mod setup;

/// `IA32_EFER`.
pub(crate) const MSR_EFER: u32 = 0xc000_0080;
/// `MSR_VM_CR`.
pub(crate) const MSR_VM_CR: u32 = 0xc001_0114;
/// `MSR_VM_HSAVE_PA`.
pub(crate) const MSR_VM_HSAVE_PA: u32 = 0xc001_0117;

pub(crate) const EFER_SVME: u64 = 1 << 12;
/// `VM_CR.LOCK`: writes to `VM_CR` are rejected.
pub(crate) const VM_CR_LOCK: u64 = 1 << 3;
/// `VM_CR.SVMDIS`: SVM is disabled by firmware.
pub(crate) const VM_CR_SVMDIS: u64 = 1 << 4;

/// ASID 0 belongs to the host; a single-guest hypervisor uses 1 everywhere.
pub const GUEST_ASID: u32 = 1;

/// Values captured once per CPU, before virtualization is enabled.
#[derive(Clone, Copy, Debug, Default)]
pub struct SvmCachedData {
    /// CPUID `8000_000A`:EAX.
    pub revision: u32,
    /// CPUID `8000_000A`:EBX, number of address space identifiers.
    pub nasid: u32,
    /// Nested paging supported (`EDX[0]`); required only when `svm-npt` is enabled.
    pub npt: bool,
    /// Next-RIP saved on #VMEXIT (`EDX[3]`).
    pub nrips: bool,
    /// Decode assists (`EDX[6]`).
    pub decode_assists: bool,
}

impl SvmCachedData {
    pub const fn new() -> Self {
        Self {
            revision: 0,
            nasid: 0,
            npt: false,
            nrips: false,
            decode_assists: false,
        }
    }

    fn probe() -> Self {
        let leaf_8000_000a = x86::cpuid::cpuid!(0x8000_000a);
        let edx = leaf_8000_000a.edx;

        Self {
            revision: leaf_8000_000a.eax,
            nasid: leaf_8000_000a.ebx,
            npt: edx & (1 << 0) != 0,
            nrips: edx & (1 << 3) != 0,
            decode_assists: edx & (1 << 6) != 0,
        }
    }
}

/// Per-CPU SVM backend state.
///
/// NOTE: keep this non-`Clone`/`Copy` and never move it after `setup`: the VMCB,
/// HSAVE and MSRPM physical addresses are handed to hardware.
pub struct SvmVcpu {
    pub vmcb: Page<Vmcb>,
    pub hsave: Page<Vmcb>,
    pub msrpm: Page<MsrpmPage>,
    /// I/O permission map: three zeroed pages (all ports passed through).
    pub iopm: Page<[u8; 0x3000]>,
    /// Host stack pointer captured at setup; VMRUN saves it into HSAVE.
    pub host_rsp: u64,
    /// Exit stub's register frame.
    pub ctx: Page<GuestContext>,
    pub cached: SvmCachedData,
    pub asid: u32,
    pub cpu_id: u32,
    #[cfg(feature = "svm-npt")]
    pub npt: NptData,
    /// Per-vCPU zero page used by HideSelf.
    #[cfg(feature = "svm-npt")]
    pub hide_zero: Page<[u8; PAGE_SIZE]>,
    #[cfg(feature = "svm-npt")]
    pub hide_self_generation: u32,
    /// Page currently served from its real frame because of a Hide hook.
    #[cfg(feature = "svm-npt")]
    pub hide_exec: Option<u64>,
    /// 上一次取指切到执行视图的 hook 页，以及把它切回读写视图所需的副本 PFN。
    ///
    /// NPT 的数据与取指共用同一个 leaf：只要它指向带补丁的影子页，数据读也
    /// 会看到补丁。下一次 VM exit 开头用它恢复 `ShadowNx`。
    #[cfg(feature = "svm-npt")]
    pub npt_shadow_view: Option<(u64, u64)>,
    /// Breakpoint single-step in progress on this vCPU.
    #[cfg(feature = "svm-npt")]
    pub hook_pending: Option<HookPending>,
}

impl SvmVcpu {
    pub const fn new() -> Self {
        Self {
            vmcb: Page::empty(),
            hsave: Page::empty(),
            msrpm: Page::empty(),
            iopm: Page::empty(),
            host_rsp: 0,
            ctx: Page::empty(),
            cached: SvmCachedData::new(),
            asid: GUEST_ASID,
            cpu_id: 0,
            #[cfg(feature = "svm-npt")]
            npt: NptData::new(),
            #[cfg(feature = "svm-npt")]
            hide_zero: Page::empty(),
            #[cfg(feature = "svm-npt")]
            hide_self_generation: 0,
            #[cfg(feature = "svm-npt")]
            hide_exec: None,
            #[cfg(feature = "svm-npt")]
            npt_shadow_view: None,
            #[cfg(feature = "svm-npt")]
            hook_pending: None,
        }
    }
}

impl SvmVcpu {
    pub fn setup(&mut self, host: &HostState, vcpu: *const Vcpu) -> HvResult<()> {
        self.capture_guest_state();
        self.enable()?;
        self.switch_root(vcpu);
        self.assign_asid(vcpu)?;
        self.setup_intercepts();
        self.finish_setup(host);
        Ok(())
    }

    /// 把一个 4 KiB NPT leaf 指向 `view` / `pfn`，并让本 vCPU 的 ASID 失效。
    #[cfg(feature = "svm-npt")]
    pub fn set_npt_view(&mut self, gpa: u64, view: NptView, pfn: u64) -> HvResult<()> {
        self.npt.apply_view(PhysAddr::new(gpa), view, pfn)?;
        tlb::flush_asid(&mut self.vmcb);

        Ok(())
    }

    /// Enter the guest. Returns only as the guest, via the launch trampoline.
    pub fn launch(&self) -> HvResult<()> {
        let vmcb_va = self.vmcb.virt().as_u64();
        let vmcb_pa = self.vmcb.phys().as_u64();
        let hsave_pa = self.hsave.phys().as_u64();
        let ctx: *mut GuestContext = self.ctx.as_mut_ptr();

        klog_verbose!(
            "SVM: vmrun vmcb={:#x} hsave={:#x} host_rsp={:#x}",
            vmcb_pa,
            hsave_pa,
            self.host_rsp
        );

        unsafe { stub::svm_vm_launch(vmcb_va, vmcb_pa, self.host_rsp, hsave_pa, ctx) };

        // Reached only as the guest: the launch trampoline returns into this
        // frame after `VMRUN` with the caller's stack.
        Ok(())
    }
}

/// Probe SVM support without touching architectural state.
pub fn check_features() -> HvResult<()> {
    let leaf_8000_0001 = x86::cpuid::cpuid!(0x8000_0001);
    if leaf_8000_0001.ecx & (1 << 2) == 0 {
        klog!("SVM: CPUID.8000_0001H:ECX[2] is clear");
        return Err(HvError::UnsupportedCpu);
    }

    let cached = SvmCachedData::probe();
    #[cfg(feature = "svm-npt")]
    if !cached.npt {
        klog!("SVM: nested paging is required");
        return Err(HvError::UnsupportedCpu);
    }

    if cached.nasid < 2 {
        klog!("SVM: NASID={} is too small", cached.nasid);
        return Err(HvError::UnsupportedCpu);
    }

    let vm_cr = unsafe { x86::msr::rdmsr(MSR_VM_CR) };
    if vm_cr & VM_CR_SVMDIS != 0 {
        klog!("SVM: disabled by firmware (VM_CR.SVMDIS=1)");
        return Err(HvError::SvmDisabledByBios);
    }

    let efer = unsafe { x86::msr::rdmsr(MSR_EFER) };
    if efer & EFER_SVME != 0 {
        klog!("SVM: EFER.SVME is already set");
        return Err(HvError::SvmAlreadyEnabled);
    }

    Ok(())
}

/// Whether SVM is currently enabled on this CPU.
pub fn is_enabled() -> bool {
    unsafe { x86::msr::rdmsr(MSR_EFER) & EFER_SVME != 0 }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cached_default_is_empty() {
        let cached = SvmCachedData::new();
        assert!(!cached.npt);
        assert_eq!(cached.nasid, 0);
    }

    #[test]
    fn guest_asid_is_not_zero() {
        assert_ne!(GUEST_ASID, 0);
    }
}
