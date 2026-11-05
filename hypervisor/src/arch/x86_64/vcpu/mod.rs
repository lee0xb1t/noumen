pub mod backend;

use core::cell::UnsafeCell;
use core::sync::atomic::{AtomicBool, Ordering};

use crate::{
    arch::x86_64::{
        host_state::HostState,
        vcpu::backend::VirtualizationBackend,
        vendor::Vendor,
        vmx,
    },
    error::{HvError, HvResult},
    klog,
};

use crate::arch::x86_64::svm;

pub const MAX_CPUS: usize = 32;

pub enum Backend {
    Vmx(vmx::VmxVcpu),
    Svm(svm::SvmVcpu),
}

impl Backend {
    /// Compile-time placeholder value.
    ///
    /// The storage always contains the VMX layout; [`Backend::init`] is what
    /// checks the vendor at runtime and rejects non-Intel CPUs.
    pub const fn new() -> Self {
        Self::Vmx(vmx::VmxVcpu::new())
    }

    /// Runtime part of the backend initialisation.
    pub fn init(&mut self) -> HvResult<()> {
        match Vendor::detect() {
            Vendor::Intel => match self {
                Backend::Vmx(v) => {
                    v.init()?;
                    v.vpid = vmx::state::GUEST_VPID as u16;
                    Ok(())
                }
                Backend::Svm(_) => Err(HvError::UnsupportedCpu),
            },
            Vendor::Amd => {
                // Allocation only: enabling SVM happens in `setup`, so a later
                // failure cannot leave the CPU with EFER.SVME set and no guest.
                let mut backend = svm::SvmVcpu::new();
                backend.init()?;
                *self = Backend::Svm(backend);
                Ok(())
            }
            Vendor::Other => Err(HvError::UnsupportedCpu),
        }
    }

    pub fn as_vmx(&self) -> &vmx::VmxVcpu {
        match self {
            Backend::Vmx(v) => v,
            Backend::Svm(_) => unreachable!("VMX-only accessor used on the SVM backend"),
        }
    }

    /// VMX-only accessor; unused in SVM-only builds.
    #[allow(dead_code)]
    pub fn as_vmx_mut(&mut self) -> &mut vmx::VmxVcpu {
        match self {
            Backend::Vmx(v) => v,
            Backend::Svm(_) => unreachable!("VMX-only accessor used on the SVM backend"),
        }
    }

    pub fn as_svm(&self) -> &svm::SvmVcpu {
        match self {
            Backend::Svm(v) => v,
            Backend::Vmx(_) => unreachable!("SVM-only accessor used on the VMX backend"),
        }
    }

    pub fn as_svm_mut(&mut self) -> &mut svm::SvmVcpu {
        match self {
            Backend::Svm(v) => v,
            Backend::Vmx(_) => unreachable!("SVM-only accessor used on the VMX backend"),
        }
    }
}

impl VirtualizationBackend for Backend {
    fn setup(&mut self, host: &HostState, vcpu: *const Vcpu) -> HvResult<()> {
        match self {
            Backend::Vmx(v) => v.setup(host, vcpu),
            Backend::Svm(v) => v.setup(host, vcpu),
        }
    }

    fn launch(&self) -> HvResult<()> {
        match self {
            Backend::Vmx(v) => v.vmlaunch(),
            Backend::Svm(v) => v.launch(),
        }
    }
}

/// Per-CPU state. Its address is programmed into the VMCS as the host FS/GS
/// base and read back by [`Vcpu::current`], and the host GDTR/IDTR/TR bases are
/// taken from the pages that `host` holds, so it must never be `Clone`/`Copy`
/// and must not be moved after [`Vcpu::setup`].
/// `#[repr(C)]` 是必须的：`vmx/stub.S` 用 `%fs:0`/`%fs:4` 直接写 `tsc_entry`。
#[repr(C)]
pub struct Vcpu {
    /// 最近一次 VM-entry 前的物理 TSC，由 `vmx/stub.S` 在 `vmresume` 前写入。
    ///
    /// **必须是第一个字段（偏移 0）**：汇编用 `%fs:0`/`%fs:4` 直接写高低 32 位。
    pub tsc_entry: u64,
    pub cpu_id: u32,
    pub host: HostState,
    pub backend: Backend,
}

/// `vmx/stub.S` 用 `%fs:0`/`%fs:4` 写 `tsc_entry`，偏移必须真的是 0。
const _: () = assert!(core::mem::offset_of!(Vcpu, tsc_entry) == 0);

impl Vcpu {
    /// Compile-time initial value.
    ///
    /// Runtime pages and counters are filled in by [`Vcpu::init`].
    pub const fn new() -> Self {
        Self {
            tsc_entry: 0,
            cpu_id: 0,
            host: HostState::new(),
            backend: Backend::new(),
        }
    }

    /// Runtime part of the initialisation: the values that are only known once
    /// the loader calls in for a specific CPU.
    fn init(&mut self, cpu_id: u32) -> HvResult<()> {
        self.host.init()?;
        self.cpu_id = cpu_id;
        self.backend.init()?;

        // 首次 `VMLAUNCH` 不经过 stub 的 entry 采样，先给一个合理的起点。
        self.tsc_entry = unsafe { core::arch::x86_64::_rdtsc() };

        Ok(())
    }

    fn setup(&mut self) -> HvResult<()> {
        self.host.prepare_gdt();
        self.host.prepare_idt();

        let self_ptr: *const Vcpu = self;
        self.backend.setup(&self.host, self_ptr)?;

        // Only a CPU that completed `setup` can later hold a stale EPT view.
        // `HOOK_RETIRED` uses this list, so single-core mode does not wait for
        // the processors that were never virtualized.
        #[cfg(feature = "vmx-ept")]
        if matches!(self.backend, Backend::Vmx(_)) {
            vmx::ept::hook::mark_online(self.cpu_id);
        }

        Ok(())
    }

    fn launch(&self) -> HvResult<()> {
        self.backend.launch()?;

        Ok(())
    }

    #[inline]
    pub unsafe fn current() -> &'static mut Self {
        let fs_base = unsafe { x86::bits64::segmentation::rdfsbase() };
        unsafe { &mut *(fs_base as *mut Self) }
    }
}

/// Storage for one CPU's [`Vcpu`].
///
/// The value is initialised at compile time (`Vcpu::new` is `const`), so unlike
/// the earlier `MaybeUninit` design there is no in-place construction step at
/// all; `GlobalVcpu::init` only patches the runtime fields.
struct CpuSlot(UnsafeCell<Vcpu>);

// SAFETY: the slot is written once by `GlobalVcpu::init` on the CPU it belongs
// to and read afterwards through `vcpu_mut` under the same `cpu_id`.
unsafe impl Sync for CpuSlot {}

pub struct GlobalVcpu {
    slots: [CpuSlot; MAX_CPUS],
    initialized: [AtomicBool; MAX_CPUS],
}

impl GlobalVcpu {
    pub const fn new() -> Self {
        Self {
            slots: [const { CpuSlot(UnsafeCell::new(Vcpu::new())) }; MAX_CPUS],
            initialized: [const { AtomicBool::new(false) }; MAX_CPUS],
        }
    }

    pub fn init(&self, cpu_id: u32) -> HvResult<()> {
        let idx = cpu_id as usize;
        let slot = self.slots.get(idx).ok_or(HvError::InvalidCpuId(cpu_id))?;

        if self.initialized[idx].load(Ordering::Acquire) {
            return Err(HvError::InvalidPrameters);
        }

        // SAFETY: the flag above guarantees this slot has never been
        // initialised, and the slot lives in a static, so it never moves.
        let vcpu = unsafe { &mut *slot.0.get() };
        vcpu.init(cpu_id)?;

        self.initialized[idx].store(true, Ordering::Release);
        Ok(())
    }

    pub fn virtualize_core(&self, cpu_id: u32) -> HvResult<()> {
        klog!(
            "======================== VIRTUALIZE CORE(#{}) ========================",
            cpu_id
        );

        let vcpu = self.vcpu_mut(cpu_id)?;

        vcpu.setup()?;

        klog!("Launching VM on vCPU#{}...", cpu_id);
        vcpu.launch()?;

        klog!("Launched VM on vCPU#{}...", cpu_id);

        Ok(())
    }

    /// 按 CPU 号取 vCPU。guest 里不能靠 `Vcpu::current()`（那是 root 的 FS base）。
    pub fn vcpu_mut(&self, cpu_id: u32) -> HvResult<&mut Vcpu> {
        let idx = cpu_id as usize;
        let slot = self.slots.get(idx).ok_or(HvError::InvalidCpuId(cpu_id))?;

        if !self.initialized[idx].load(Ordering::Acquire) {
            return Err(HvError::VcpuNotInitialized);
        }

        // SAFETY: see `CpuSlot`; `initialized` is only set after a successful
        // initialisation and the slot is never moved.
        Ok(unsafe { &mut *slot.0.get() })
    }
}
