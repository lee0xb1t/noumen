//! Hide the hypervisor's kernel image pages from the guest (HideSelf).

use core::sync::atomic::{AtomicU32, Ordering};

use crate::{
    arch::x86_64::{
        addr::PhysAddr,
        memory::{self, PAGE_SIZE},
        paging::{self, DIRECT_MAP_SIZE},
        svm::{hook::HookError, npt::NptView, state::SvmVcpu},
    },
    hv,
};

/// `0` means HideSelf has never run.
static GENERATION: AtomicU32 = AtomicU32::new(0);

const MAX_KERNEL_PFNS: u32 = 0x0010_0000;

fn kernel_pfns() -> Option<&'static [u64]> {
    let args = hv::GLOBAL_ARGS.get();

    if args.kernel_pfn_base == 0
        || args.kernel_pfn_count == 0
        || args.kernel_pfn_count > MAX_KERNEL_PFNS
    {
        return None;
    }

    // The driver's array is a kernel virtual address; the host page table maps
    // the kernel, so it is readable here.
    Some(unsafe {
        core::slice::from_raw_parts(
            args.kernel_pfn_base as *const u64,
            args.kernel_pfn_count as usize,
        )
    })
}

/// Hide every image page on this vCPU. Returns the number of pages hidden.
pub fn hide(svm: &mut SvmVcpu) -> Result<u32, HookError> {
    let hidden = apply(svm)?;
    let generation = GENERATION.fetch_add(1, Ordering::AcqRel) + 1;

    svm.hide_self_generation = generation;

    Ok(hidden)
}

/// Apply the latest HideSelf generation on this vCPU.
///
/// Fast path is one generation compare; inline it into the exit path.
#[inline]
pub fn sync(svm: &mut SvmVcpu) {
    let generation = GENERATION.load(Ordering::Acquire);

    if generation == 0 || svm.hide_self_generation == generation {
        return;
    }

    if apply(svm).is_ok() {
        svm.hide_self_generation = generation;
    }
}

fn apply(svm: &mut SvmVcpu) -> Result<u32, HookError> {
    let pfns = kernel_pfns().ok_or(HookError::InvalidAddress)?;

    if svm.hide_zero.virt().as_u64() == 0 {
        svm.hide_zero = memory::alloc::<[u8; PAGE_SIZE]>().map_err(|_| HookError::NoMemory)?;
    }

    svm.hide_zero.fill(0);
    let zero_pfn = svm.hide_zero.pfn();
    let mut hidden = 0u32;

    for &pfn in pfns {
        if pfn == 0 {
            continue;
        }

        let gpa = pfn << 12;

        if gpa + PAGE_SIZE as u64 > DIRECT_MAP_SIZE {
            continue;
        }

        // 只改表、最后统一失效一次；`TLB_CONTROL` 在下一次 VMRUN 生效。
        if svm
            .npt
            .apply_view(PhysAddr::new(gpa), NptView::ShadowNx, zero_pfn)
            .is_ok()
        {
            hidden += 1;
        }
    }

    if hidden > 0 {
        crate::arch::x86_64::svm::tlb::flush_asid(&mut svm.vmcb);
    }

    Ok(hidden)
}

/// Read image page `index` through the direct map (the real hypervisor page).
pub fn read_root(index: u32, destination: &mut [u8; PAGE_SIZE]) -> bool {
    let Some(pfn) = pfn_at(index) else {
        return false;
    };

    read_physical(pfn, destination)
}

/// Read image page `index` through the current vCPU's NPT leaf. After HideSelf
/// this is the zero page, which is what the guest would observe.
pub fn read_ept(svm: &mut SvmVcpu, index: u32, destination: &mut [u8; PAGE_SIZE]) -> bool {
    let Some(pfn) = pfn_at(index) else {
        return false;
    };

    let gpa = pfn << 12;

    let leaf_pfn = match svm.npt.pte_raw(PhysAddr::new(gpa)) {
        Ok(Some(pte)) => (pte >> 12) & super::NptEntry::PFN_MAX,
        // Still a 2 MiB mapping: the leaf is the real page.
        Ok(None) => pfn,
        Err(_) => return false,
    };

    read_physical(leaf_pfn, destination)
}

fn pfn_at(index: u32) -> Option<u64> {
    kernel_pfns()?.get(index as usize).copied()
}

fn read_physical(pfn: u64, destination: &mut [u8; PAGE_SIZE]) -> bool {
    let gpa = pfn << 12;

    if gpa + PAGE_SIZE as u64 > DIRECT_MAP_SIZE {
        return false;
    }

    let source = paging::phys_to_virt(PhysAddr::new(gpa)).as_ptr::<u8>();

    // SAFETY: the direct map covers [gpa, gpa + PAGE_SIZE); `destination` is a
    // PAGE_SIZE array.
    unsafe {
        core::ptr::copy_nonoverlapping(source, destination.as_mut_ptr(), PAGE_SIZE);
    }

    true
}
