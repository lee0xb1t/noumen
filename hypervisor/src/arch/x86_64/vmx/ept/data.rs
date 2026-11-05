//! EPT tables and the identity map used to virtualize the guest.

use super::types::{EptPde, EptPde2mb, EptPdpte, EptPml4e, EptPte, Eptp};
use crate::{
    arch::x86_64::{
        addr::PhysAddr,
        memory::{self, Page},
        mtrr::MtrrData,
        paging::{self, MemoryType},
        vmx::instruction::Invept,
    },
    error::{HvError, HvResult},
    klog,
};

pub const EPT_ENTRY_COUNT: usize = 512;
/// 4-level EPT: 512 PML4E × 512 × 1 GiB = 256 TiB of guest-physical space.
pub const EPT_PML4_COUNT: usize = 512;

const EPT_1GB_SHIFT: u32 = 30;
const EPT_2MB_SHIFT: u32 = 21;
const EPT_PML4_SHIFT: u32 = 39;
const EPT_1GB_SIZE: u64 = 1 << EPT_1GB_SHIFT;
const EPT_2MB_SIZE: u64 = 1 << EPT_2MB_SHIFT;
const EPT_PML4_SIZE: u64 = 1 << EPT_PML4_SHIFT;
const EPT_LARGE_PAGE: u64 = 1 << 7;

/// One vCPU's EPT paging structures: PML4 → 512 PDPTs of 1 GiB pages.
pub struct EptData {
    pml4: Page<[EptPml4e; EPT_ENTRY_COUNT]>,
    pdpts: [Page<[EptPdpte; EPT_ENTRY_COUNT]>; EPT_PML4_COUNT],
    /// HideSelf 把内核镜像页映射到这一个全 0 页；每核一份，避免跨核写互相可见。
    dummy: Page<[u8; memory::PAGE_SIZE]>,
    eptp: Eptp,
    /// Filled PML4 entries, from MAXPHYADDR.
    pdpt_used: usize,
    /// 批量改表期间把 INVEPT 推迟到 `end_batch()`。
    defer_invalidate: bool,
    pending_invalidate: bool,
}

impl EptData {
    pub const fn new() -> Self {
        Self {
            pml4: Page::empty(),
            pdpts: [Page::empty(); EPT_PML4_COUNT],
            dummy: Page::empty(),
            eptp: Eptp::new(),
            pdpt_used: 0,
            defer_invalidate: false,
            pending_invalidate: false,
        }
    }

    pub fn init(&mut self) -> HvResult<()> {
        self.pml4 = memory::alloc()?;
        self.dummy = memory::alloc()?;
        self.dummy.fill(0);

        Ok(())
    }

    /// Builds the identity map with 1 GiB pages, typed from the MTRRs.
    ///
    /// Covers `1 << MAXPHYADDR`, rounded up to whole 512 GiB PML4 entries.
    pub fn prepare(&mut self) {
        if crate::hv::ept_1gb_pages() {
            self.prepare_1gb();
        } else {
            self.prepare_2mb();
        }

        self.eptp = Eptp::from_pfn(self.pml4.pfn());

        klog!(
            "EPT: ready, pml4 pfn={:#x}, eptp={:#x}",
            self.eptp.pfn(),
            self.eptp.as_raw()
        );
    }

    fn prepare_1gb(&mut self) {
        self.pdpt_used = pml4_entries_needed();

        klog!(
            "EPT: preparing identity map ({} GiB, {} PML4E x 512 GiB, 1 GiB pages)",
            self.pdpt_used * 512,
            self.pdpt_used
        );

        let mut mixed = 0u32;

        self.pml4.fill(EptPml4e::new());

        let mtrrs = MtrrData::read();

        for i in 0..self.pdpt_used {
            let pdpt = match memory::alloc::<[EptPdpte; EPT_ENTRY_COUNT]>() {
                Ok(page) => page,
                Err(_) => {
                    klog!("EPT: out of memory for the identity map");
                    return;
                }
            };

            self.pdpts[i] = pdpt;
            self.pdpts[i].fill(EptPdpte::new());

            let pml4e = &mut self.pml4[i];
            pml4e.set_read(true);
            pml4e.set_write(true);
            pml4e.set_execute(true);
            pml4e.set_user_mode_execute(true);
            pml4e.set_pfn(pdpt.pfn());

            for j in 0..EPT_ENTRY_COUNT {
                let addr = ((i as u64) << EPT_PML4_SHIFT) | ((j as u64) << EPT_1GB_SHIFT);

                // 1 GiB 叶只能有一个内存类型；段内有 MTRR 边界就拆成 2 MiB 叶。
                if mtrrs.is_uniform_1gb(addr) {
                    self.pdpts[i][j] =
                        large_entry(addr, mtrrs.memory_type(addr, EPT_1GB_SIZE) as u8);
                    continue;
                }

                mixed += 1;

                let page = match memory::alloc::<[EptPde2mb; EPT_ENTRY_COUNT]>() {
                    Ok(page) => page,
                    Err(_) => {
                        klog!("EPT: out of memory for the identity map");
                        return;
                    }
                };

                {
                    let pdes = unsafe { &mut *page.as_mut_ptr() };

                    for (k, pde) in pdes.iter_mut().enumerate() {
                        let sub = addr + ((k as u64) << EPT_2MB_SHIFT);

                        let mut leaf = EptPde2mb::new();
                        leaf.set_read(true);
                        leaf.set_write(true);
                        leaf.set_execute(true);
                        leaf.set_large_page(true);
                        leaf.set_user_mode_execute(true);
                        leaf.set_memory_type(mtrrs.memory_type(sub, EPT_2MB_SIZE) as u8);
                        leaf.set_pfn(sub >> EPT_2MB_SHIFT);

                        *pde = leaf;
                    }
                }

                let mut table = EptPde::new();
                table.set_read(true);
                table.set_write(true);
                table.set_execute(true);
                table.set_user_mode_execute(true);
                table.set_pfn(page.pfn());

                self.pdpts[i][j] = EptPdpte::from_raw(table.as_raw());
            }
        }

        klog!("EPT: {} mixed 1 GiB region(s) split to 2 MiB leaves", mixed);
    }

    pub fn eptp(&self) -> Eptp {
        self.eptp
    }

    /// 批量修改页表：期间不发 INVEPT，`end_batch()` 统一发一次。
    pub fn begin_batch(&mut self) {
        self.defer_invalidate = true;
    }

    pub fn end_batch(&mut self) {
        self.defer_invalidate = false;

        if self.pending_invalidate {
            self.pending_invalidate = false;
            self.invalidate();
        }
    }

    fn invalidate(&mut self) {
        if self.defer_invalidate {
            self.pending_invalidate = true;
            return;
        }

        let eptp = self.eptp.as_raw();

        if !Invept::single(eptp) {
            klog!("diag: INVEPT(single) failed eptp={eptp:#x}; using all-contexts");

            if !Invept::all(eptp) {
                klog!("diag: INVEPT(all) also failed eptp={eptp:#x}");
            }
        }
    }

    /// Sets the memory type of every mapped page.
    pub fn set_memory_type(&mut self, memory_type: MemoryType) {
        let mut walker = MtrrWalker::constant(self, memory_type as u64);
        walker.run();
    }

    /// Recomposes every entry's memory type from the current MTRRs.
    pub fn update_memory_type(&mut self) {
        let mut walker = MtrrWalker::from_mtrrs(self);
        walker.run();
    }

    /// Returns the 4 KiB PTE covering `phys`, splitting large pages when asked.
    ///
    /// `force_split == false` 不修改任何表，没有 PTE 就返回 `None`。
    pub fn get_pte(&mut self, phys: PhysAddr, force_split: bool) -> HvResult<Option<&mut EptPte>> {
        let index = AddressIndex::new(phys);

        if index.pml4 >= self.pdpt_used {
            return Ok(None);
        }

        if !force_split {
            let entry = self.pdpts[index.pml4][index.pdpt].as_raw();

            // 没有映射的 1 GiB 段：既不是大页，也没有 PD 表。
            if entry == 0 {
                return Ok(None);
            }

            // 1 GiB 叶子、或它下面的 2 MiB 叶子，都还没有 PTE。
            if entry & EPT_LARGE_PAGE != 0 {
                return Ok(None);
            }

            let pdes = unsafe { table_mut::<EptPde2mb>(self.pdpts[index.pml4][index.pdpt].pfn()) };
            if pdes[index.pd].large_page() {
                return Ok(None);
            }
        }

        self.split_pde(phys)?;

        let pdes = unsafe { table_mut::<EptPde2mb>(self.pdpts[index.pml4][index.pdpt].pfn()) };
        let pt = unsafe { table_mut::<EptPte>(pdes[index.pd].as_table().pfn()) };

        Ok(Some(&mut pt[index.pt]))
    }

    /// 覆盖 `phys` 的 4 KiB PTE 原始值；该区域仍由大页映射时返回 `None`。
    pub fn pte_raw(&mut self, phys: PhysAddr) -> HvResult<Option<u64>> {
        Ok(self.get_pte(phys, false)?.map(|pte| pte.as_raw()))
    }

    /// 覆盖 `phys` 的 4 KiB PTE，并使缓存翻译失效；必要时先拆大页。
    pub fn set_pte(&mut self, phys: PhysAddr, value: u64) -> HvResult<()> {
        {
            let pte = self.get_pte(phys, true)?.ok_or(HvError::InvalidPrameters)?;

            *pte = EptPte::from_raw(value);
        }

        self.invalidate();

        Ok(())
    }

    /// 把这个 PFN 的 guest 视图映射到本核的全 0 dummy 页。
    pub fn hide_physical_page(&mut self, pfn: u64) -> HvResult<()> {
        let phys = PhysAddr::new(pfn << 12);

        self.dummy.fill(0);

        self.split_pde(phys)?;

        let current = self.pte_raw(phys)?.ok_or(HvError::InvalidPrameters)?;
        let mut pte = EptPte::from_raw(current);

        pte.set_read(true);
        pte.set_write(true);
        pte.set_execute(true);
        pte.set_pfn(self.dummy.pfn());
        self.set_pte(phys, pte.as_raw())?;

        Ok(())
    }

    /// Splits the large page covering `phys` down to a 4 KiB PT.
    pub fn split_pde(&mut self, phys: PhysAddr) -> HvResult<()> {
        let index = AddressIndex::new(phys);

        if index.pml4 >= self.pdpt_used {
            return Err(HvError::InvalidPrameters);
        }

        let pdes = self.pd_table(index.pml4, index.pdpt)?;        let pde = &mut pdes[index.pd];

        if !pde.large_page() {
            return Ok(());
        }

        let page = memory::alloc::<[EptPte; EPT_ENTRY_COUNT]>()?;
        let pt = unsafe { &mut *page.as_mut_ptr() };

        for (i, pte) in pt.iter_mut().enumerate() {
            pte.set_read(pde.read());
            pte.set_write(pde.write());
            pte.set_execute(pde.execute());
            pte.set_memory_type(pde.memory_type());
            pte.set_ignore_pat(pde.ignore_pat());
            pte.set_accessed(pde.accessed());
            pte.set_dirty(pde.dirty());
            pte.set_user_mode_execute(pde.user_mode_execute());
            pte.set_verify_guest_paging(pde.verify_guest_paging());
            pte.set_paging_write_access(pde.paging_write_access());
            pte.set_supervisor_shadow_stack(pde.supervisor_shadow_stack());
            pte.set_suppress_ve(pde.suppress_ve());
            pte.set_pfn((pde.pfn() << 9) + i as u64);
        }

        let mut table = EptPde::new();
        table.set_read(true);
        table.set_write(true);
        table.set_execute(true);
        table.set_user_mode_execute(true);
        table.set_pfn(page.pfn());

        *pde = EptPde2mb::from_raw(table.as_raw());

        // 2 MiB → 4 KiB 是结构变更：不刷的话 CPU 还会用缓存的 2 MiB 翻译。
        self.invalidate();

        Ok(())
    }

    fn prepare_2mb(&mut self) {
        let bytes = crate::hv::ept_coverage().max(64 << 20);
        let pdptes = (bytes.div_ceil(EPT_1GB_SIZE) as usize).clamp(1, EPT_ENTRY_COUNT * EPT_PML4_COUNT);

        self.pdpt_used = pdptes.div_ceil(EPT_ENTRY_COUNT);

        klog!(
            "EPT: preparing identity map ({} GiB = {} PML4E x {} PD tables, 2 MiB pages)",
            bytes >> 30,
            self.pdpt_used,
            pdptes
        );

        self.pml4.fill(EptPml4e::new());

        let mtrrs = MtrrData::read();
        let mut left = pdptes;

        for i in 0..self.pdpt_used {
            let pdpt = match memory::alloc::<[EptPdpte; EPT_ENTRY_COUNT]>() {
                Ok(page) => page,
                Err(_) => {
                    klog!("EPT: out of memory for the identity map");
                    return;
                }
            };

            self.pdpts[i] = pdpt;
            self.pdpts[i].fill(EptPdpte::new());

            let pml4e = &mut self.pml4[i];
            pml4e.set_read(true);
            pml4e.set_write(true);
            pml4e.set_execute(true);
            pml4e.set_user_mode_execute(true);
            pml4e.set_pfn(pdpt.pfn());

            let count = left.min(EPT_ENTRY_COUNT);
            left -= count;

            for j in 0..count {
                let page = match memory::alloc::<[EptPde2mb; EPT_ENTRY_COUNT]>() {
                    Ok(page) => page,
                    Err(_) => {
                        klog!("EPT: out of memory for the identity map");
                        return;
                    }
                };

                {
                    let pdes = unsafe { &mut *page.as_mut_ptr() };
                    let base = ((i as u64) << EPT_PML4_SHIFT) | ((j as u64) << EPT_1GB_SHIFT);

                    for (k, pde) in pdes.iter_mut().enumerate() {
                        let addr = base + ((k as u64) << EPT_2MB_SHIFT);

                        let mut large = EptPde2mb::new();
                        large.set_read(true);
                        large.set_write(true);
                        large.set_execute(true);
                        large.set_large_page(true);
                        large.set_user_mode_execute(true);
                        large.set_memory_type(mtrrs.memory_type(addr, EPT_2MB_SIZE) as u8);
                        large.set_pfn(addr >> EPT_2MB_SHIFT);

                        *pde = large;
                    }
                }

                let mut table = EptPde::new();
                table.set_read(true);
                table.set_write(true);
                table.set_execute(true);
                table.set_user_mode_execute(true);
                table.set_pfn(page.pfn());

                self.pdpts[i][j] = EptPdpte::from_raw(table.as_raw());
            }
        }
    }

    /// Returns the 2 MiB PDE table for one 1 GiB region, splitting it first.
    fn pd_table(
        &mut self,
        pml4: usize,
        pdpt: usize,
    ) -> HvResult<&'static mut [EptPde2mb; EPT_ENTRY_COUNT]> {
        let entry = self.pdpts[pml4][pdpt].as_raw();

        if entry == 0 {
            return Err(HvError::InvalidPrameters);
        }

        if entry & EPT_LARGE_PAGE != 0 {
            let page = memory::alloc::<[EptPde2mb; EPT_ENTRY_COUNT]>()?;
            let base = ((pml4 as u64) << EPT_PML4_SHIFT) | ((pdpt as u64) << EPT_1GB_SHIFT);
            let mtrrs = MtrrData::read();

            {
                let pdes = unsafe { &mut *page.as_mut_ptr() };

                for (i, pde) in pdes.iter_mut().enumerate() {
                    let addr = base + ((i as u64) << EPT_2MB_SHIFT);

                    let mut large = EptPde2mb::new();
                    large.set_read(true);
                    large.set_write(true);
                    large.set_execute(true);
                    large.set_large_page(true);
                    large.set_user_mode_execute(true);
                    large.set_memory_type(mtrrs.memory_type(addr, EPT_2MB_SIZE) as u8);
                    large.set_pfn(addr >> EPT_2MB_SHIFT);

                    *pde = large;
                }
            }

            let mut table = EptPde::new();
            table.set_read(true);
            table.set_write(true);
            table.set_execute(true);
            table.set_user_mode_execute(true);
            table.set_pfn(page.pfn());

            self.pdpts[pml4][pdpt] = EptPdpte::from_raw(table.as_raw());

            self.invalidate();
        }

        let pfn = self.pdpts[pml4][pdpt].pfn();

        Ok(unsafe { table_mut::<EptPde2mb>(pfn) })
    }
}

/// 1 GiB EPT leaf entry. Bits 51:30 hold the physical address; 29:12 are
/// reserved and must stay zero.
fn large_entry(addr: u64, memory_type: u8) -> EptPdpte {
    let mut raw = EptPdpte::new().as_raw();

    raw |= 0b111;
    raw |= (memory_type as u64 & 7) << 3;
    raw |= EPT_LARGE_PAGE;
    raw |= 1 << 10;
    raw |= addr & 0x000F_FFFF_C000_0000;

    EptPdpte::from_raw(raw)
}

fn set_1gb_memory_type(entry: &mut EptPdpte, memory_type: u64) {
    *entry = EptPdpte::from_raw((entry.as_raw() & !(7 << 3)) | ((memory_type & 7) << 3));
}

fn pml4_entries_needed() -> usize {
    let entries = crate::hv::ept_coverage().div_ceil(EPT_PML4_SIZE);

    entries.clamp(1, EPT_PML4_COUNT as u64) as usize
}

/// 统一的遍历器：按需拆 1 GiB 页，重刷所有叶子的内存类型。
struct MtrrWalker<'a> {
    data: &'a mut EptData,
    mtrrs: Option<MtrrData>,
    constant: Option<u64>,
}

impl<'a> MtrrWalker<'a> {
    fn from_mtrrs(data: &'a mut EptData) -> Self {
        Self {
            data,
            mtrrs: Some(MtrrData::read()),
            constant: None,
        }
    }

    fn constant(data: &'a mut EptData, memory_type: u64) -> Self {
        Self {
            data,
            mtrrs: None,
            constant: Some(memory_type),
        }
    }

    fn memory_type(&self, addr: u64, size: u64) -> u8 {
        match self.constant {
            Some(memory_type) => memory_type as u8,
            None => self
                .mtrrs
                .as_ref()
                .map_or(MemoryType::Uncacheable as u8, |mtrrs| {
                    mtrrs.memory_type(addr, size) as u8
                }),
        }
    }

    fn run(&mut self) {
        let used = self.data.pdpt_used;

        for i in 0..used {
            for j in 0..EPT_ENTRY_COUNT {
                let base = ((i as u64) << EPT_PML4_SHIFT) | ((j as u64) << EPT_1GB_SHIFT);
                let raw = self.data.pdpts[i][j].as_raw();

                if raw & EPT_LARGE_PAGE != 0 {
                    let memory_type = self.memory_type(base, EPT_1GB_SIZE);

                    set_1gb_memory_type(&mut self.data.pdpts[i][j], memory_type as u64);
                    continue;
                }

                if raw == 0 {
                    continue;
                }

                let pdes = unsafe { table_mut::<EptPde2mb>(self.data.pdpts[i][j].pfn()) };

                for (k, pde) in pdes.iter_mut().enumerate() {
                    let addr = base + ((k as u64) << EPT_2MB_SHIFT);

                    if pde.large_page() {
                        pde.set_memory_type(self.memory_type(addr, EPT_2MB_SIZE));
                        continue;
                    }

                    let pt = unsafe { table_mut::<EptPte>(pde.as_table().pfn()) };

                    for (m, pte) in pt.iter_mut().enumerate() {
                        let addr = addr + ((m as u64) << 12);
                        pte.set_memory_type(
                            self.memory_type(addr, memory::PAGE_SIZE as u64),
                        );
                    }
                }
            }
        }
    }
}

struct AddressIndex {
    pml4: usize,
    pdpt: usize,
    pd: usize,
    pt: usize,
}

impl AddressIndex {
    fn new(phys: PhysAddr) -> Self {
        let addr = phys.as_u64();

        Self {
            pml4: ((addr >> 39) & 0x1ff) as usize,
            pdpt: ((addr >> 30) & 0x1ff) as usize,
            pd: ((addr >> 21) & 0x1ff) as usize,
            pt: ((addr >> 12) & 0x1ff) as usize,
        }
    }
}

unsafe fn table_mut<T>(pfn: u64) -> &'static mut [T; EPT_ENTRY_COUNT] {
    let virt = paging::phys_to_virt(PhysAddr::new(pfn << 12));

    unsafe { &mut *virt.as_mut_ptr::<[T; EPT_ENTRY_COUNT]>() }
}
