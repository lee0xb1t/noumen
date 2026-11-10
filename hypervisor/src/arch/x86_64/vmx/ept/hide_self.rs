//! 隐藏 hypervisor 自身的内核镜像页。
//!
//! 驱动在 `KernelArgs` 里给出镜像每一页的 PFN。HideSelf 把这些 PFN 的 guest
//! EPT leaf 指向本核的全 0 dummy 页；root 仍通过宿主页表直接访问真实页，因此
//! hypervisor 自己继续正常执行，而 guest 读到的镜像页全是 0。
//!
//! EPT 是每核的；hypercall 只立即处理当前核，其它核在下次 VM exit 的
//! [`sync`] 里应用同一个 generation。

use core::sync::atomic::{AtomicU32, Ordering};

use super::hook::HookError;
use crate::{
    arch::x86_64::{
        addr::PhysAddr,
        paging,
        vmx::state::VmxVcpu,
    },
    hv,
};

/// `0` 表示还没调用过 HideSelf。
static GENERATION: AtomicU32 = AtomicU32::new(0);

const MAX_KERNEL_PFNS: u32 = 0x100000;

fn kernel_pfns() -> Option<&'static [u64]> {
    let args = hv::GLOBAL_ARGS.get();

    if args.kernel_pfn_base == 0
        || args.kernel_pfn_count == 0
        || args.kernel_pfn_count > MAX_KERNEL_PFNS
    {
        return None;
    }

    // 驱动给出的数组是内核虚拟地址；hypervisor 的宿主页表映射了内核。
    Some(unsafe {
        core::slice::from_raw_parts(
            args.kernel_pfn_base as *const u64,
            args.kernel_pfn_count as usize,
        )
    })
}

/// 把当前 vCPU 的 EPT 里所有内核镜像页映射到 dummy 页。
pub fn hide(vmx: &mut VmxVcpu, cpu_id: u32) -> Result<u32, HookError> {
    let hidden = apply(vmx, cpu_id)?;
    let generation = GENERATION.fetch_add(1, Ordering::AcqRel) + 1;

    crate::arch::x86_64::vmx::state::bump_exit_sync_generation();
    vmx.hide_self_generation = generation;

    Ok(hidden)
}

/// 在每次 VM exit 的分发前把 HideSelf 的 generation 应用到本核。
///
/// 快路径只做一次 generation 比较；`#[inline]` 让它在 exit 桩里展开，
/// 避免每次退出都为一个“已同步”判断付出函数调用开销。
#[inline]
pub fn sync(cpu_id: u32, vmx: &mut VmxVcpu) {
    let generation = GENERATION.load(Ordering::Acquire);

    if generation == 0 || vmx.hide_self_generation == generation {
        return;
    }

    if apply(vmx, cpu_id).is_ok() {
        vmx.hide_self_generation = generation;
    }
}

/// 只应用，不推进 generation：否则每个核 sync 都会再 +1，永远追不上。
fn apply(vmx: &mut VmxVcpu, cpu_id: u32) -> Result<u32, HookError> {
    let _ = cpu_id;

    let Some(pfns) = kernel_pfns() else {
        return Err(HookError::InvalidAddress);
    };

    let mut hidden = 0u32;

    // 几千页逐页 INVEPT 会让整机停顿数百毫秒：批量改完只刷一次。
    vmx.ept.begin_batch();

    for &pfn in pfns {
        if pfn == 0 {
            continue;
        }

        if vmx.ept.hide_physical_page(pfn).is_err() {
            vmx.ept.end_batch();
            return Err(HookError::NoMemory);
        }

        hidden += 1;
    }

    vmx.ept.end_batch();

    Ok(hidden)
}

/// 镜像页 PFN；`index` 越界返回 `None`。
pub fn pfn_at(index: u32) -> Option<u64> {
    kernel_pfns()?.get(index as usize).copied()
}

/// 从 root 的直接映射读一页。HideSelf 之后这里仍然是真实的 hypervisor 页。
pub fn read_root(index: u32, destination: &mut [u8]) -> Option<()> {
    let pfn = pfn_at(index)?;

    if pfn == 0 {
        return None;
    }

    read_physical(pfn, destination)
}

/// 从当前 vCPU 的 EPT leaf 指向的物理页读一页。HideSelf 之后 leaf 指向
/// dummy 页，因此读到的是 0。
pub fn read_ept(vmx: &mut VmxVcpu, index: u32, destination: &mut [u8]) -> Option<()> {
    let pfn = pfn_at(index)?;
    let gpa = pfn << 12;

    let leaf_pfn = match vmx.ept.pte_raw(PhysAddr::new(gpa)) {
        Ok(Some(pte)) => (pte >> 12) & 0x000F_FFFF_FFFF,
        // 2 MiB 叶子还没有 split：guest 看到的就是真实 PFN。
        Ok(None) => pfn,
        Err(_) => return None,
    };

    read_physical(leaf_pfn, destination)
}

fn read_physical(pfn: u64, destination: &mut [u8]) -> Option<()> {
    if destination.len() < 4096 {
        return None;
    }

    let source = paging::phys_to_virt(PhysAddr::new(pfn << 12)).as_ptr::<u8>();

    unsafe {
        core::ptr::copy_nonoverlapping(source, destination.as_mut_ptr(), 4096);
    }

    Some(())
}
