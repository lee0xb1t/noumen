//! Guest virtual -> physical translation for hypercall buffers.
//!
//! Walks the guest's 4-level page tables through the host direct map and copies
//! with `memcpy_safe`, so a bad buffer becomes a guest `#PF`/`#GP` instead of a
//! host fault. Assumes identity EPT and physical addresses below the 512 GiB
//! direct map; does not consult EPT permissions or update A/D bits.

use core::mem::size_of;

use crate::arch::x86_64::{
    addr::PhysAddr,
    paging::{self, DIRECT_MAP_SIZE},
    safe_routine::{self, HostExceptionInfo},
};

const PAGE_SIZE: usize = 0x1000;

const PRESENT: u64 = 1 << 0;
const WRITABLE: u64 = 1 << 1;
const USER: u64 = 1 << 2;
const LARGE_PAGE: u64 = 1 << 7;

const ADDRESS_MASK: u64 = 0x000f_ffff_ffff_f000;
const ADDRESS_MASK_1G: u64 = 0x000f_ffff_c000_0000;
const ADDRESS_MASK_2M: u64 = 0x000f_ffff_ffe0_0000;

/// Guest page-fault details.
#[allow(dead_code)] // not every build consumes every fault field
#[derive(Debug, Clone, Copy)]
pub struct PageFault {
    pub address: u64,
    pub present: bool,
    pub write: bool,
    pub user: bool,
}

/// Why a guest buffer access failed.
#[allow(dead_code)] // ABI detail shared by the VMX and SVM paths
#[derive(Debug, Clone, Copy)]
pub enum MemoryFault {
    NonCanonical,
    Page(PageFault),
}

/// Copies `source` to a guest VA. On failure a prefix may be written and the
/// caller must not consume ring state.
pub fn copy_to_guest(
    cr3: u64,
    destination: u64,
    source: &[u8],
    user: bool,
    write_protect: bool,
) -> Result<(), MemoryFault> {
    let mut offset = 0;

    while offset < source.len() {
        let address = destination
            .checked_add(offset as u64)
            .ok_or(MemoryFault::NonCanonical)?;

        if !is_canonical(address) {
            return Err(MemoryFault::NonCanonical);
        }

        let physical = translate(cr3, address, user, true, write_protect)?;
        let page_offset = (address & (PAGE_SIZE as u64 - 1)) as usize;
        let chunk = (source.len() - offset).min(PAGE_SIZE - page_offset);

        if !write_physical(physical, &source[offset..offset + chunk]) {
            // Host write failed (MMIO hole, disappearing RAM); report not-present.
            return Err(page_fault(address, false, user, true));
        }

        offset += chunk;
    }

    Ok(())
}

/// 把 guest 虚拟地址翻译成 guest 物理地址。
///
/// 通过宿主 direct map 走 guest 自己的页表，因此在 root 模式下也安全，且不会
/// 在宿主上触发缺页。`user` 选择访问特权级，`write` 选择访问类型，
/// `write_protect` 为 supervisor 写强制 `CR0.WP`；权限失败以
/// [`MemoryFault::Page`] 返回，和 guest 缺页完全一样。
///
/// 这里不查询 EPT：它回答的是“guest 页表把哪个物理页映射到这个地址”，
/// 这正是 EPT hook 引擎从虚拟地址找页所需要的。
pub fn translate_guest(
    cr3: u64,
    address: u64,
    user: bool,
    write: bool,
    write_protect: bool,
) -> Result<u64, MemoryFault> {
    if !is_canonical(address) {
        return Err(MemoryFault::NonCanonical);
    }

    translate(cr3, address, user, write, write_protect)
}

fn translate(
    cr3: u64,
    address: u64,
    user: bool,
    write: bool,
    write_protect: bool,
) -> Result<u64, MemoryFault> {
    let pml4e = read_entry(cr3 & ADDRESS_MASK, (address >> 39) & 0x1ff, address, user, write)?;
    check_entry(pml4e, address, user, write, write_protect)?;
    if pml4e & LARGE_PAGE != 0 {
        return Err(page_fault(address, false, user, write));
    }

    let pdpte = read_entry(
        pml4e & ADDRESS_MASK,
        (address >> 30) & 0x1ff,
        address,
        user,
        write,
    )?;
    check_entry(pdpte, address, user, write, write_protect)?;
    if pdpte & LARGE_PAGE != 0 {
        return Ok((pdpte & ADDRESS_MASK_1G) | (address & 0x3fff_ffff));
    }

    let pde = read_entry(
        pdpte & ADDRESS_MASK,
        (address >> 21) & 0x1ff,
        address,
        user,
        write,
    )?;
    check_entry(pde, address, user, write, write_protect)?;
    if pde & LARGE_PAGE != 0 {
        return Ok((pde & ADDRESS_MASK_2M) | (address & 0x1f_ffff));
    }

    let pte = read_entry(
        pde & ADDRESS_MASK,
        (address >> 12) & 0x1ff,
        address,
        user,
        write,
    )?;
    check_entry(pte, address, user, write, write_protect)?;

    // Bit 7 is PAT here, not PS; the mask already drops it.
    Ok((pte & ADDRESS_MASK) | (address & 0xfff))
}

fn check_entry(
    entry: u64,
    address: u64,
    user: bool,
    write: bool,
    write_protect: bool,
) -> Result<(), MemoryFault> {
    if entry & PRESENT == 0 {
        return Err(page_fault(address, false, user, write));
    }

    // U/S applies only to user accesses.
    if user && entry & USER == 0 {
        return Err(page_fault(address, true, user, write));
    }

    // 写访问要求每一级都可写。CR0.WP 把这条规则扩展到 supervisor 写；
    // 用户写始终要求可写。
    if write && (user || write_protect) && entry & WRITABLE == 0 {
        return Err(page_fault(address, true, user, write));
    }

    Ok(())
}

fn read_entry(
    table: u64,
    index: u64,
    address: u64,
    user: bool,
    write: bool,
) -> Result<u64, MemoryFault> {
    let entry_address = table.wrapping_add(index * size_of::<u64>() as u64);

    read_physical_u64(entry_address).ok_or_else(|| page_fault(address, false, user, write))
}

/// Reads a physical qword through the direct map; `None` on fault.
fn read_physical_u64(physical: u64) -> Option<u64> {
    let end = physical.checked_add(size_of::<u64>() as u64)?;
    if end > DIRECT_MAP_SIZE {
        return None;
    }

    let source = paging::phys_to_virt(PhysAddr::new(physical)).as_ptr::<u8>();

    let mut value = 0u64;
    let mut exception_info = HostExceptionInfo::new();

    let copied = safe_routine::memcpy_safe(
        &mut exception_info,
        (&mut value as *mut u64).cast::<u8>(),
        source,
        size_of::<u64>(),
    );

    copied.then_some(value)
}

/// Writes through the direct map; `false` on fault.
fn write_physical(physical: u64, bytes: &[u8]) -> bool {
    if physical >= DIRECT_MAP_SIZE {
        return false;
    }

    let Some(end) = physical.checked_add(bytes.len() as u64) else {
        return false;
    };

    if end > DIRECT_MAP_SIZE {
        return false;
    }

    let destination = paging::phys_to_virt(PhysAddr::new(physical)).as_mut_ptr::<u8>();
    let mut exception_info = HostExceptionInfo::new();

    safe_routine::memcpy_safe(
        &mut exception_info,
        destination,
        bytes.as_ptr(),
        bytes.len(),
    )
}

/// 48 位 canonical 形式（`MOV to CR4` 已拒绝 LA57）。
pub fn is_canonical(address: u64) -> bool {
    let bit47 = (address >> 47) & 1;
    let upper = address >> 48;

    (bit47 == 0 && upper == 0) || (bit47 == 1 && upper == 0xffff)
}

fn page_fault(address: u64, present: bool, user: bool, write: bool) -> MemoryFault {
    MemoryFault::Page(PageFault {
        address,
        present,
        write,
        user,
    })
}
