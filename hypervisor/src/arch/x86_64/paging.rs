use core::ops::Index;
use core::ops::IndexMut;
use core::usize;

use x86_64::structures::paging::{PageTable, PageTableFlags};

use crate::arch::x86_64::addr::PhysAddr;
use crate::arch::x86_64::addr::VirtAddr;
use crate::arch::x86_64::memory::{self, PAGE_SHIFT, Page};
use crate::error::HvResult;
use crate::{klog, klog_verbose};

/// One PDPT entry covers 1 GiB. 512 of them fill one PML4 entry (512 GiB), which
/// is also the largest identity map the host direct map can build: raising this
/// past 512 needs a second PML4 slot and a second PDPT.
pub const MAX_PDPT_COUNT: usize = 512;
pub const MAX_PDT_COUNT: usize = 512;

const _: () = assert!(MAX_PDPT_COUNT <= 512);

pub const DIRECT_PML4_INDEX: usize = 255;
#[allow(dead_code)]
pub const PAGE_TABLE_FIRST: usize = 0;

/// Host virtual address of physical address 0 in the identity direct map.
pub const DIRECT_MAP_BASE: VirtAddr = VirtAddr::new((DIRECT_PML4_INDEX as u64) << 39);
#[cfg_attr(not(feature = "vmx-ept"), allow(dead_code))]
pub const DIRECT_MAP_SIZE: u64 = (MAX_PDPT_COUNT as u64) * (MAX_PDT_COUNT as u64) * 0x20_0000;

#[repr(C, align(4096))]
struct SubPageTable<const COUNT: usize> {
    tables: [PageTable; COUNT],
}

impl<const COUNT: usize> Index<usize> for SubPageTable<COUNT> {
    type Output = PageTable;

    #[inline]
    fn index(&self, index: usize) -> &Self::Output {
        &self.tables[index]
    }
}

impl<const COUNT: usize> IndexMut<usize> for SubPageTable<COUNT> {
    #[inline]
    fn index_mut(&mut self, index: usize) -> &mut Self::Output {
        &mut self.tables[index]
    }
}

pub struct HostPageTable {
    pml4: Page<PageTable>,
    pdpt: Page<PageTable>,
    pdts: Page<SubPageTable<MAX_PDPT_COUNT>>,
}

impl HostPageTable {
    pub const fn new() -> Self {
        Self {
            pml4: Page::empty(),
            pdpt: Page::empty(),
            pdts: Page::empty(),
        }
    }

    pub fn init(&mut self) -> HvResult<()> {
        self.pml4 = memory::alloc()?;
        self.pdpt = memory::alloc()?;
        self.pdts = memory::alloc()?;

        Ok(())
    }

    pub fn map_identity(&mut self) {
        let flags = PageTableFlags::PRESENT | PageTableFlags::WRITABLE;

        self.pml4[DIRECT_PML4_INDEX]
            .set_addr(x86_64::PhysAddr::new(self.pdpt.pfn() << PAGE_SHIFT), flags);

        for i in 0..MAX_PDPT_COUNT {
            self.pdpt[i].set_addr(
                x86_64::PhysAddr::new(self.pdts.pfn_at(i) << PAGE_SHIFT),
                flags,
            );

            let pdt = &mut self.pdts[i];
            for (j, pde) in pdt.iter_mut().enumerate() {
                pde.set_addr(
                    x86_64::PhysAddr::new((((i << 9) + j) as u64) << 21),
                    flags | PageTableFlags::HUGE_PAGE,
                );
            }
        }

        klog!("HostPageTable::map_identity done");
        klog_verbose!(
            "direct map: {:#x}..{:#x}",
            DIRECT_MAP_BASE.as_u64(),
            DIRECT_MAP_BASE.as_u64() + DIRECT_MAP_SIZE
        );
    }

    pub fn get_cr3(&self) -> u64 {
        self.pml4.pfn() << PAGE_SHIFT
    }

    // Copy all system kernel space memory mapping
    pub fn map_kernel(&mut self, system_cr3: VirtAddr) {
        let system_page = unsafe { &*system_cr3.as_ptr::<PageTable>() };

        for i in 256..512 {
            let system_entry = system_page.index(i);
            self.pml4[i].set_addr(system_entry.addr(), system_entry.flags());
        }
    }
}

/// Memory type as encoded in the PAT, the EPT paging structures and `VMX_BASIC`.
#[repr(u8)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MemoryType {
    Uncacheable = 0,
    WriteCombining = 1,
    WriteThrough = 4,
    WriteProtected = 5,
    WriteBack = 6,
    UncacheableMinus = 7,
    Invalid = 0xff,
}

impl TryFrom<u8> for MemoryType {
    type Error = &'static str;

    fn try_from(value: u8) -> Result<Self, Self::Error> {
        match value {
            0 => Ok(MemoryType::Uncacheable),
            1 => Ok(MemoryType::WriteCombining),
            4 => Ok(MemoryType::WriteThrough),
            5 => Ok(MemoryType::WriteProtected),
            6 => Ok(MemoryType::WriteBack),
            7 => Ok(MemoryType::UncacheableMinus),
            0xff => Ok(MemoryType::Invalid),
            _ => Err("invalid memory type value"),
        }
    }
}

impl From<MemoryType> for u8 {
    fn from(mt: MemoryType) -> Self {
        mt as u8
    }
}

/// Strong Uncacheable (UC)
pub const MEMORY_TYPE_UNCACHEABLE: u32 = MemoryType::Uncacheable as u32;
/// Write Combining (WC)
#[allow(dead_code)]
pub const MEMORY_TYPE_WRITE_COMBINING: u32 = MemoryType::WriteCombining as u32;
/// Write-through (WT)
pub const MEMORY_TYPE_WRITE_THROUGH: u32 = MemoryType::WriteThrough as u32;
/// Write protected (WP)
#[allow(dead_code)]
pub const MEMORY_TYPE_WRITE_PROTECTED: u32 = MemoryType::WriteProtected as u32;
/// Write-back (WB)
pub const MEMORY_TYPE_WRITE_BACK: u32 = MemoryType::WriteBack as u32;
/// Uncacheable (UC-)
pub const MEMORY_TYPE_UNCACHEABLE_MINUS: u32 = MemoryType::UncacheableMinus as u32;
/// Invalid
#[allow(dead_code)]
pub const MEMORY_TYPE_INVALID: u32 = MemoryType::Invalid as u32;

bitfield::bitfield! {
    #[derive(Debug, Clone, Copy)]
    pub struct PatRegister(u64);

    pub u8, pa0, set_pa0: 2, 0;
    pub u8, pa1, set_pa1: 10, 8;
    pub u8, pa2, set_pa2: 18, 16;
    pub u8, pa3, set_pa3: 26, 24;
    pub u8, pa4, set_pa4: 34, 32;
    pub u8, pa5, set_pa5: 42, 40;
    pub u8, pa6, set_pa6: 50, 48;
    pub u8, pa7, set_pa7: 58, 56;
}

impl Default for PatRegister {
    fn default() -> Self {
        let mut s = Self(0);
        s.set_pa0(MEMORY_TYPE_WRITE_BACK as u8);
        s.set_pa1(MEMORY_TYPE_WRITE_THROUGH as u8);
        s.set_pa2(MEMORY_TYPE_UNCACHEABLE_MINUS as u8);
        s.set_pa3(MEMORY_TYPE_UNCACHEABLE as u8);
        s.set_pa4(MEMORY_TYPE_WRITE_BACK as u8);
        s.set_pa5(MEMORY_TYPE_WRITE_THROUGH as u8);
        s.set_pa6(MEMORY_TYPE_UNCACHEABLE_MINUS as u8);
        s.set_pa7(MEMORY_TYPE_UNCACHEABLE as u8);

        s
    }
}

impl PatRegister {
    pub const fn from_raw(raw: u64) -> Self {
        Self(raw)
    }

    pub const fn as_raw(&self) -> u64 {
        self.0
    }
}

/// Translates a physical address through the direct map.
///
/// Only valid while the host page table is active: the direct map lives in
/// [`DIRECT_PML4_INDEX`] of the host page table, not of the guest's.
#[cfg_attr(not(feature = "vmx-ept"), allow(dead_code))]
pub fn phys_to_virt(phys: PhysAddr) -> VirtAddr {
    assert!(
        phys.as_u64() < DIRECT_MAP_SIZE,
        "physical address 0x{:x} is outside the direct map",
        phys.as_u64()
    );

    VirtAddr::new(DIRECT_MAP_BASE.as_u64() + phys.as_u64())
}
