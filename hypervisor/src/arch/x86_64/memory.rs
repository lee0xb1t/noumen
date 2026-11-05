//! 4 KiB page pool shared by the EPT/NPT split tables and the per-CPU host state.

use core::marker::PhantomData;
use core::mem::{align_of, size_of};
use core::ops::{Deref, DerefMut};
use core::ptr;

use crate::arch::x86_64::addr::{PhysAddr, VirtAddr};
use crate::arch::x86_64::sync;
use crate::error::{HvError, HvResult};
use crate::klog_verbose;

pub const PAGE_SIZE: usize = 0x1000;
pub const PAGE_SHIFT: usize = 12;

/// 4 KiB pages reserved for the page pool.
///
/// EPT 恒等映射按内存大小选页粒度，PDPT 页按需分配（每用到的 PML4E 一页）：
///   1 GiB 页（RAM ≥ 64 GiB）: 1 PML4 + M PDPT + 1 dummy
///   2 MiB 页（RAM < 64 GiB）: 1 PML4 + 1 PDPT + N PD 表 + 1 dummy, N = ceil(RAM/1 GiB)
/// 加上每 vCPU 12 页（VMXON/VMCS/MSR bitmap/hide/host state）。
///
/// Budget with `MAX_CPUS == 32`, 64 GiB RAM (worst case, 2 MiB mode):
///   32 × 79 vCPU pages              =  2528
///   host page table (1+1+512)       =   514
///   internal log ring               =    20
///   --------------------------------------
///   static total                    =  3062  (12 MiB)
///   worst case hooks (32 × 32)      =  2048
///   --------------------------------------
///   peak                            =  5110  (20 MiB)
///
/// 8192 pages (32 MiB) leaves ~12 MiB of headroom. NPT (SVM) 覆盖
/// `min(MAXPHYADDR, 64 GiB)`，每 vCPU 约 66 页。
#[cfg(any(feature = "vmx-ept", feature = "svm-npt"))]
pub const POOL_PAGES: usize = 8192;
#[cfg(not(any(feature = "vmx-ept", feature = "svm-npt")))]
pub const POOL_PAGES: usize = 2048;

const POOL_BYTES: usize = POOL_PAGES * PAGE_SIZE;
const PFN_BYTES: usize = POOL_PAGES * size_of::<u64>();

core::arch::global_asm!(
    ".pushsection .noumen.pool,\"aw\",@nobits",
    ".balign {page_size}",
    ".globl noumen_pool",
    "noumen_pool:",
    ".space {pool_bytes}",
    ".popsection",
    ".pushsection .noumen.pfns,\"aw\",@nobits",
    ".balign 8",
    ".globl noumen_pfns",
    "noumen_pfns:",
    ".space {pfn_bytes}",
    ".popsection",
    page_size = const PAGE_SIZE,
    pool_bytes = const POOL_BYTES,
    pfn_bytes = const PFN_BYTES,
);

#[repr(C, align(0x1000))]
struct PageFrame {
    bytes: [u8; PAGE_SIZE],
}

const _: () = assert!(size_of::<PageFrame>() == PAGE_SIZE);
const _: () = assert!(align_of::<PageFrame>() == PAGE_SIZE);

unsafe extern "C" {
    static noumen_pool: [PageFrame; POOL_PAGES];
    static noumen_pfns: [u64; POOL_PAGES];
}

const fn pages_for<T>() -> usize {
    let size = size_of::<T>();

    if size == 0 {
        1
    } else {
        (size - 1) / PAGE_SIZE + 1
    }
}

pub struct Page<T> {
    virt: VirtAddr,
    index: usize,
    pages: usize,
    marker: PhantomData<T>,
}

impl<T> Page<T> {
    const fn new(virt: VirtAddr, index: usize, pages: usize) -> Self {
        Self {
            virt,
            index,
            pages,
            marker: PhantomData,
        }
    }

    pub const fn empty() -> Self {
        Self::new(VirtAddr::null(), 0, 0)
    }

    pub fn virt(&self) -> VirtAddr {
        self.virt
    }

    pub fn phys(&self) -> PhysAddr {
        PhysAddr::new(self.pfn() << PAGE_SHIFT)
    }

    pub fn pfn(&self) -> u64 {
        self.pfn_at(0)
    }

    pub fn pfn_at(&self, page: usize) -> u64 {
        debug_assert!(page < self.pages);

        unsafe { noumen_pfns[self.index + page] }
    }

    pub fn size(&self) -> usize {
        self.pages * PAGE_SIZE
    }

    pub fn as_ptr(&self) -> *const T {
        self.virt.as_ptr()
    }

    pub fn as_mut_ptr(&self) -> *mut T {
        self.virt.as_mut_ptr()
    }

    pub fn zero(&self) {
        unsafe { ptr::write_bytes(self.virt.as_mut_ptr::<u8>(), 0, self.size()) }
    }
}

impl<T> Clone for Page<T> {
    fn clone(&self) -> Self {
        *self
    }
}

impl<T> Copy for Page<T> {}

impl<T> Deref for Page<T> {
    type Target = T;

    fn deref(&self) -> &T {
        unsafe { &*self.as_ptr() }
    }
}

impl<T> DerefMut for Page<T> {
    fn deref_mut(&mut self) -> &mut T {
        unsafe { &mut *self.as_mut_ptr() }
    }
}

pub struct BumpAllocator {
    next: usize,
    initialized: bool,
}

impl BumpAllocator {
    pub const fn new() -> Self {
        Self {
            next: 0,
            initialized: false,
        }
    }

    pub fn init(&mut self) {
        self.next = 0;
        self.initialized = true;
    }

    pub fn alloc<T>(&mut self) -> HvResult<Page<T>> {
        self.alloc_pages::<T>(pages_for::<T>())
    }

    pub fn alloc_pages<T>(&mut self, count: usize) -> HvResult<Page<T>> {
        assert!(self.initialized, "page pool allocated before init()");

        if count == 0 || count < pages_for::<T>() {
            return Err(HvError::InvalidPrameters);
        }

        let end = self
            .next
            .checked_add(count)
            .ok_or(HvError::NotEnoughMemory)?;

        if end > POOL_PAGES {
            return Err(HvError::NotEnoughMemory);
        }

        let page = Page::<T>::new(
            VirtAddr::from_ptr(unsafe { &noumen_pool[self.next] }),
            self.next,
            count,
        );
        self.next = end;

        page.zero();

        Ok(page)
    }
}

pub static GLOBAL_PAGE_POOL: sync::Mutex<BumpAllocator> = sync::Mutex::new(BumpAllocator::new());

pub fn init() -> HvResult<()> {
    for i in 0..POOL_PAGES {
        if unsafe { noumen_pfns[i] } == 0 {
            return Err(HvError::InvalidPrameters);
        }
    }

    GLOBAL_PAGE_POOL.lock().init();

    klog_verbose!("page pool: {} pages, pfn0={:#x}", POOL_PAGES, unsafe {
        noumen_pfns[0]
    });

    Ok(())
}

pub fn alloc<T>() -> HvResult<Page<T>> {
    GLOBAL_PAGE_POOL.lock().alloc()
}

pub fn alloc_pages<T>(count: usize) -> HvResult<Page<T>> {
    GLOBAL_PAGE_POOL.lock().alloc_pages(count)
}

/// Whether [`init`] has already initialized the bump allocator. The internal log
/// ring uses it during boot to fail softly instead of tripping the allocator's
/// "allocated before init" assertion.
#[cfg(feature = "buffer-log")]
pub fn is_initialized() -> bool {
    GLOBAL_PAGE_POOL.lock().initialized
}

#[allow(dead_code)]
pub fn pool_pfns() -> &'static [u64] {
    unsafe { &noumen_pfns }
}
