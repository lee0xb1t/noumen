//! NPT paging structures and the identity map.

use crate::{
    arch::x86_64::{
        addr::PhysAddr,
        memory::{self, Page},
        paging,
    },
    error::{HvError, HvResult},
};

use super::{state::view_pte, types::NptEntry};

pub const ENTRY_COUNT: usize = 512;
/// 1 GiB 模式最多 512 个 PML4E（256 TiB）；2 MiB 模式最多 64 个 1 GiB 段。
pub const MAX_PDPT: usize = 512;
pub const MAX_PD_TABLES: usize = 64;

/// One vCPU's NPT structures: identity map of `min(MAXPHYADDR, 64 GiB)`, or of
/// the whole physical address space when 1 GiB NPT pages are available.
pub struct NptData {
    pml4: Page<[NptEntry; ENTRY_COUNT]>,
    pdpt: Page<[NptEntry; ENTRY_COUNT]>,
    /// 1 GiB 模式：每个 PML4E 一个 PDPT；2 MiB 模式：每个 1 GiB 段一张 PD 表。
    tables: [Page<[NptEntry; ENTRY_COUNT]>; MAX_PDPT],
    used: usize,
    use_1gb: bool,
    ncr3: u64,
}

impl NptData {
    pub const fn new() -> Self {
        Self {
            pml4: Page::empty(),
            pdpt: Page::empty(),
            tables: [Page::empty(); MAX_PDPT],
            used: 0,
            use_1gb: false,
            ncr3: 0,
        }
    }

    pub fn init(&mut self) -> HvResult<()> {
        self.pml4 = memory::alloc()?;
        self.pdpt = memory::alloc()?;

        Ok(())
    }

    /// Build the identity map: 1 GiB or 2 MiB pages, RWX, user bit set.
    pub fn prepare(&mut self) {
        let bits = (x86::cpuid::cpuid!(0x8000_0008).eax & 0xff).clamp(32, 52);
        let max_phyaddr = 1u64 << bits;

        self.use_1gb = has_1gb_pages();
        let bytes = if self.use_1gb {
            max_phyaddr.min(1u64 << 40)
        } else {
            max_phyaddr.min(64u64 << 30)
        };
        self.pml4.fill(NptEntry::new());

        if self.use_1gb {
            let entries = (bytes.div_ceil(1 << 39) as usize).clamp(1, MAX_PDPT);

            for i in 0..entries {
                let mut page = match memory::alloc::<[NptEntry; ENTRY_COUNT]>() {
                    Ok(page) => page,
                    Err(_) => break,
                };

                page.fill(NptEntry::new());
                self.tables[i] = page;
                self.pml4[i] = NptEntry::table(page.phys().as_u64());
                self.used = i + 1;

                for j in 0..ENTRY_COUNT {
                    let phys = ((i as u64) << 39) | ((j as u64) << 30);
                    self.tables[i][j] = NptEntry::leaf_1g(phys, true, false);
                }
            }
        } else {
            let tables = (bytes.div_ceil(1 << 30) as usize).clamp(1, MAX_PD_TABLES);

            self.pdpt.fill(NptEntry::new());
            self.pml4[0] = NptEntry::table(self.pdpt.phys().as_u64());

            for k in 0..tables {
                let mut page = match memory::alloc::<[NptEntry; ENTRY_COUNT]>() {
                    Ok(page) => page,
                    Err(_) => break,
                };

                page.fill(NptEntry::new());
                self.tables[k] = page;
                self.pdpt[k] = NptEntry::table(page.phys().as_u64());
                self.used = k + 1;

                for j in 0..ENTRY_COUNT {
                    let phys = (((k << 9) + j) as u64) << 21;
                    self.tables[k][j] = NptEntry::leaf_2m(phys, true, false);
                }
            }
        }

        self.ncr3 = self.pml4.phys().as_u64();
    }

    /// Physical address of the NPT root (VMCB `nested_cr3`).
    pub const fn ncr3(&self) -> u64 {
        self.ncr3
    }

    /// 4 KiB entry covering `phys`, splitting large pages when asked.
    pub fn get_pte(&mut self, phys: PhysAddr, force_split: bool) -> HvResult<Option<&mut NptEntry>> {
        let index = AddressIndex::new(phys);

        if !self.in_range(&index) {
            return Ok(None);
        }

        if !force_split {
            if self.use_1gb && self.tables[index.pml4][index.pdpt].large_page() {
                return Ok(None);
            }

            if !self.use_1gb && self.tables[index.pdpt][index.pd].large_page() {
                return Ok(None);
            }
        }

        self.split_pde(phys)?;

        let pdes = self.pd_table_entry(index.pml4, index.pdpt);
        let pfn = pdes[index.pd].pfn_4k();
        let table = unsafe { table_mut::<NptEntry>(pfn) };

        Ok(Some(&mut table[index.pt]))
    }

    /// Raw 4 KiB entry value, without splitting.
    pub fn pte_raw(&mut self, phys: PhysAddr) -> HvResult<Option<u64>> {
        Ok(self.get_pte(phys, false)?.map(|entry| entry.as_raw()))
    }

    /// Whether the leaf covering `phys` currently allows execution.
    pub fn executable(&self, phys: PhysAddr) -> HvResult<bool> {
        let index = AddressIndex::new(phys);

        if !self.in_range(&index) {
            return Ok(false);
        }

        if self.use_1gb {
            let entry = self.tables[index.pml4][index.pdpt];

            if entry.large_page() {
                return Ok(!entry.nx());
            }

            if !entry.present() {
                return Ok(false);
            }
        }

        let pdes = self.pd_table_entry(index.pml4, index.pdpt);
        let pde = pdes[index.pd];

        if pde.large_page() {
            return Ok(!pde.nx());
        }

        if !pde.present() {
            return Ok(false);
        }

        let table = unsafe { table_mut::<NptEntry>(pde.pfn_4k()) };

        Ok(!table[index.pt].nx())
    }

    /// Point the 4 KiB leaf covering `phys` at `view` over page frame `pfn`.
    ///
    /// The caller must request a TLB flush afterwards; see
    /// [`crate::arch::x86_64::svm::tlb`].
    pub fn apply_view(
        &mut self,
        phys: PhysAddr,
        view: super::state::NptView,
        pfn: u64,
    ) -> HvResult<()> {
        let entry = self
            .get_pte(phys, true)?
            .ok_or(HvError::InvalidPrameters)?;

        *entry = NptEntry::from_raw(view_pte(view, pfn));

        Ok(())
    }

    /// Split the large page covering `phys` down to a 4 KiB PT.
    ///
    /// The caller must request a TLB flush (`tlb_control = 3`) afterwards: the
    /// leaf level changed and the old translation may still be cached.
    pub fn split_pde(&mut self, phys: PhysAddr) -> HvResult<()> {
        let index = AddressIndex::new(phys);

        if !self.in_range(&index) {
            return Err(HvError::InvalidPrameters);
        }

        if self.use_1gb {
            self.split_1gb(index.pml4, index.pdpt)?;
        }

        let pdes = self.pd_table_entry(index.pml4, index.pdpt);
        let pde = pdes[index.pd];

        if !pde.large_page() {
            return Ok(());
        }

        let page = memory::alloc::<[NptEntry; ENTRY_COUNT]>()?;
        let table = unsafe { &mut *page.as_mut_ptr() };
        let base_pfn = pde.pfn_2m();

        for (i, entry) in table.iter_mut().enumerate() {
            let mut leaf = NptEntry::new();
            leaf.set_present(pde.present());
            leaf.set_write(pde.write());
            leaf.set_user(true);
            leaf.set_pwt(pde.pwt());
            leaf.set_pcd(pde.pcd());
            leaf.set_accessed(pde.accessed());
            leaf.set_dirty(pde.dirty());
            leaf.set_global(pde.global());
            leaf.set_nx(pde.nx());
            leaf.set_pfn_4k((base_pfn << 9) + i as u64);
            *entry = leaf;
        }

        pdes[index.pd] = NptEntry::table(page.phys().as_u64());

        Ok(())
    }

    /// Replace a 1 GiB leaf with a 512-entry 2 MiB PD table.
    fn split_1gb(&mut self, pml4: usize, pdpt: usize) -> HvResult<()> {
        let entry = self.tables[pml4][pdpt];

        if !entry.large_page() {
            return Ok(());
        }

        let page = memory::alloc::<[NptEntry; ENTRY_COUNT]>()?;
        let table = unsafe { &mut *page.as_mut_ptr() };
        let base = entry.pfn_1g() << 30;

        for (i, leaf) in table.iter_mut().enumerate() {
            *leaf = NptEntry::leaf_2m(base + ((i as u64) << 21), entry.write(), entry.nx());
        }

        self.tables[pml4][pdpt] = NptEntry::table(page.phys().as_u64());

        Ok(())
    }

    fn pd_table_entry(&self, pml4: usize, pdpt: usize) -> &'static mut [NptEntry; ENTRY_COUNT] {
        let pfn = if self.use_1gb {
            self.tables[pml4][pdpt].pfn_4k()
        } else {
            self.pdpt[pdpt].pfn_4k()
        };

        unsafe { table_mut::<NptEntry>(pfn) }
    }

    fn in_range(&self, index: &AddressIndex) -> bool {
        if self.use_1gb {
            index.pml4 < self.used
        } else {
            index.pml4 == 0 && index.pdpt < self.used
        }
    }
}

fn has_1gb_pages() -> bool {
    let edx = x86::cpuid::cpuid!(0x8000_0001).edx;

    edx & (1 << 26) != 0
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

unsafe fn table_mut<T>(pfn: u64) -> &'static mut [T; ENTRY_COUNT] {
    let virt = paging::phys_to_virt(PhysAddr::new(pfn << 12));

    unsafe { &mut *virt.as_mut_ptr::<[T; ENTRY_COUNT]>() }
}
