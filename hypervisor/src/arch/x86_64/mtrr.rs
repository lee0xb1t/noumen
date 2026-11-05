//! Memory type range registers.

use core::fmt;

use crate::arch::x86_64::paging::MemoryType;

pub const FIXED_MTRR_COUNT: usize = 11;
pub const MAX_VARIABLE_RANGES: usize = 64;

pub const FIXED_MTRR_MSRS: [u32; FIXED_MTRR_COUNT] = [
    x86::msr::IA32_MTRR_FIX64K_00000,
    x86::msr::IA32_MTRR_FIX16K_80000,
    x86::msr::IA32_MTRR_FIX16K_A0000,
    x86::msr::IA32_MTRR_FIX4K_C0000,
    x86::msr::IA32_MTRR_FIX4K_C8000,
    x86::msr::IA32_MTRR_FIX4K_D0000,
    x86::msr::IA32_MTRR_FIX4K_D8000,
    x86::msr::IA32_MTRR_FIX4K_E0000,
    x86::msr::IA32_MTRR_FIX4K_E8000,
    x86::msr::IA32_MTRR_FIX4K_F0000,
    x86::msr::IA32_MTRR_FIX4K_F8000,
];

const FIXED_MTRR_END: u64 = 0x10_0000;

bitfield::bitfield! {
    #[repr(transparent)]
    #[derive(Clone, Copy, Debug)]
    pub struct MtrrCap(u64);

    pub u8, variable_range_count, _: 7, 0;
    pub fixed_range_supported, _: 8;
}

impl MtrrCap {
    pub fn read() -> Self {
        Self(unsafe { x86::msr::rdmsr(x86::msr::IA32_MTRRCAP) })
    }
}

bitfield::bitfield! {
    #[repr(transparent)]
    #[derive(Clone, Copy, Debug)]
    struct MtrrDefType(u64);

    pub u8, memory_type, _: 2, 0;
    pub fixed_range_mtrr_enable, _: 10;
    pub mtrr_enable, _: 11;
}

impl MtrrDefType {
    fn read() -> Self {
        Self(unsafe { x86::msr::rdmsr(x86::msr::IA32_MTRR_DEF_TYPE) })
    }
}

bitfield::bitfield! {
    #[repr(transparent)]
    #[derive(Clone, Copy, Debug)]
    struct MtrrPhysBase(u64);

    pub u8, memory_type, _: 2, 0;
    pub page_frame, _: 51, 12;
}

bitfield::bitfield! {
    #[repr(transparent)]
    #[derive(Clone, Copy, Debug)]
    struct MtrrPhysMask(u64);

    pub valid, _: 11;
    pub page_frame, _: 51, 12;
}

#[derive(Clone, Copy)]
struct MemoryTypeRange {
    /// `PHYSBASE` 的 PFN，已按 `mask_pfn` 对齐。
    base_pfn: u64,
    /// `PHYSMASK` 的 PFN。
    mask_pfn: u64,
    memory_type: MemoryType,
}

impl MemoryTypeRange {
    const EMPTY: Self = Self {
        base_pfn: 0,
        mask_pfn: 0,
        memory_type: MemoryType::Uncacheable,
    };

    fn new(base: MtrrPhysBase, mask: MtrrPhysMask) -> Self {
        let mask_pfn = mask.page_frame();

        Self {
            base_pfn: base.page_frame() & mask_pfn,
            mask_pfn,
            memory_type: memory_type(base.memory_type()),
        }
    }

    /// 与硬件完全相同的判定：`(phys & mask) == (base & mask)`。
    ///
    /// IMPORTANT: 不能用「start + size」。`PHYSMASK` 的 PFN 字段只到
    /// `MAXPHYADDR-1`（本机 39 位），bit 51:39 恒为 0；按 `~mask + 1` 反推尺寸
    /// 会得到 PB 级区间，让整段 RAM 变成 uncached。用掩码本身比较则与
    /// `MAXPHYADDR` 无关。
    fn contains(&self, address: u64) -> bool {
        let pfn = address >> 12;

        (pfn & self.mask_pfn) == self.base_pfn
    }

    /// 覆盖的起始物理地址（仅用于日志）。
    fn start(&self) -> u64 {
        self.base_pfn << 12
    }

    /// 覆盖的字节数（仅用于日志）：`mask` 的最低位就是 size 边界。
    fn size(&self) -> u64 {
        let pages = if self.mask_pfn == 0 {
            // 掩码为 0 表示覆盖整个物理地址空间。
            1u64 << 40
        } else {
            1u64 << self.mask_pfn.trailing_zeros()
        };

        pages << 12
    }
}

impl fmt::Debug for MemoryTypeRange {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "{:#x}..{:#x} {:?} mask={:#x}",
            self.start(),
            self.start() + self.size(),
            self.memory_type,
            self.mask_pfn
        )
    }
}

pub struct MtrrData {
    variable: [MemoryTypeRange; MAX_VARIABLE_RANGES],
    variable_count: usize,
    fixed: [u64; FIXED_MTRR_COUNT],
    default_type: MemoryType,
    enabled: bool,
    fixed_enabled: bool,
}

impl MtrrData {
    pub fn read() -> Self {
        let cap = MtrrCap::read();
        let def_type = MtrrDefType::read();

        let mut variable = [MemoryTypeRange::EMPTY; MAX_VARIABLE_RANGES];
        let mut variable_count = 0;

        for i in 0..cap.variable_range_count() as u32 {
            if variable_count == MAX_VARIABLE_RANGES {
                break;
            }

            let mask =
                MtrrPhysMask(unsafe { x86::msr::rdmsr(x86::msr::IA32_MTRR_PHYSMASK0 + i * 2) });

            if !mask.valid() {
                continue;
            }

            let base =
                MtrrPhysBase(unsafe { x86::msr::rdmsr(x86::msr::IA32_MTRR_PHYSBASE0 + i * 2) });

            variable[variable_count] = MemoryTypeRange::new(base, mask);
            variable_count += 1;
        }

        let fixed_enabled = cap.fixed_range_supported() && def_type.fixed_range_mtrr_enable();

        let mut fixed = [0; FIXED_MTRR_COUNT];
        if fixed_enabled {
            for (value, msr) in fixed.iter_mut().zip(FIXED_MTRR_MSRS) {
                *value = unsafe { x86::msr::rdmsr(msr) };
            }
        }

        Self {
            variable,
            variable_count,
            fixed,
            default_type: memory_type(def_type.memory_type()),
            enabled: def_type.mtrr_enable(),
            fixed_enabled,
        }
    }

    /// Worst memory type used by any page in `[address, address + size)`.
    ///
    /// IMPORTANT: 不要逐 4 KiB 页扫。一个 2 MiB 窗口要 512 次 × 区间数，32768
    /// 个 PDE 就是 1.3 亿次（实测 ~40 ms，而且跑在 VM-exit 里、root 的 IF=0）。
    /// 窗口的类型只取决于「哪些区间与它相交」，所以直接按区间求交：每项 ≤
    /// 区间数 次比较，同样的活降到 ~0.1 ms。
    ///
    /// 等价性依赖 `default_type` 不比任何区间类型更差 —— Windows 上是
    /// `WriteBack` + UC 区间，成立。若将来要处理「默认 UC + 只覆盖窗口一部分
    /// 的 WB 区间」，这里还得加一次「窗口是否被完全覆盖」的判定。
    /// `[addr, addr + 1 GiB)` 内没有 MTRR 边界 → 可以用 1 GiB 大页。
    /// 只比较区间边界，不逐 2 MiB 采样。
    pub fn is_uniform_1gb(&self, addr: u64) -> bool {
        const GIB: u64 = 1 << 30;

        if !self.enabled {
            return true;
        }

        let end_addr = addr + GIB;

        // fixed MTRR 只覆盖低 1 MiB（边界在 512 KiB / 640 KiB / 1 MiB）。
        if self.fixed_enabled && addr < 0x10_0000 && end_addr > 0x8_0000 {
            return false;
        }

        self.variable[..self.variable_count].iter().all(|range| {
            let start = range.start();
            let end = start + range.size();

            // 只有边界落在段内部才算混合；整段被区间覆盖是均匀的。
            !(start > addr && start < end_addr) && !(end > addr && end < end_addr)
        })
    }

    pub fn memory_type(&self, address: u64, size: u64) -> MemoryType {
        let start = address & !0xfff;
        let end = (address + size + 0xfff) & !0xfff;

        let mut worst = MemoryType::Invalid;

        // 0–1 MiB：固定 MTRR 的粒度是 4/16/64 KiB，只能逐页（只有窗口 0 会走到）。
        let fixed_end = FIXED_MTRR_END.min(end);
        let mut page = start;
        while page < fixed_end {
            let page_type = self.page_type(page);

            if page_type == MemoryType::Uncacheable {
                return page_type;
            }

            worst = worse(worst, page_type);
            page += 0x1000;
        }

        // ≥1 MiB：只看与窗口相交的 variable range。
        if start.max(FIXED_MTRR_END) < end {
            for range in &self.variable[..self.variable_count] {
                let range_start = range.start();
                let range_end = range_start + range.size();

                if range_end <= start || range_start >= end {
                    continue;
                }

                if range.memory_type == MemoryType::Uncacheable {
                    return MemoryType::Uncacheable;
                }

                worst = worse(worst, range.memory_type);
            }
        }

        if worst == MemoryType::Invalid {
            self.default_type
        } else {
            worst
        }
    }

    fn page_type(&self, address: u64) -> MemoryType {
        if !self.enabled {
            return MemoryType::Uncacheable;
        }

        let mut current = None;

        if self.fixed_enabled && address < FIXED_MTRR_END {
            let fixed_type = self.fixed_type(address);

            if fixed_type == MemoryType::Uncacheable {
                return fixed_type;
            }

            current = Some(fixed_type);
        }

        for range in &self.variable[..self.variable_count] {
            if !range.contains(address) {
                continue;
            }

            if range.memory_type == MemoryType::Uncacheable {
                return range.memory_type;
            }

            current = Some(match current {
                None => range.memory_type,
                Some(current) => worse(current, range.memory_type),
            });
        }

        current.unwrap_or(self.default_type)
    }

    fn fixed_type(&self, address: u64) -> MemoryType {
        let (msr, index) = if address < 0x8_0000 {
            (self.fixed[0], (address >> 16) & 7)
        } else if address < 0xa_0000 {
            (self.fixed[1], ((address - 0x8_0000) >> 14) & 7)
        } else if address < 0xc_0000 {
            (self.fixed[2], ((address - 0xa_0000) >> 14) & 7)
        } else {
            let block = ((address - 0xc_0000) >> 15) as usize;
            (self.fixed[3 + block], ((address - 0xc_0000) >> 12) & 7)
        };

        memory_type(((msr >> (index * 8)) & 0xff) as u8)
    }
}

impl fmt::Debug for MtrrData {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("MtrrData")
            .field("enabled", &self.enabled)
            .field("fixed_enabled", &self.fixed_enabled)
            .field("default_type", &self.default_type)
            .field("variable_ranges", &&self.variable[..self.variable_count])
            .finish()
    }
}

fn memory_type(raw: u8) -> MemoryType {
    match raw {
        0 => MemoryType::Uncacheable,
        1 => MemoryType::WriteCombining,
        4 => MemoryType::WriteThrough,
        5 => MemoryType::WriteProtected,
        6 => MemoryType::WriteBack,
        _ => MemoryType::Uncacheable,
    }
}

fn worse(a: MemoryType, b: MemoryType) -> MemoryType {
    if (a as u8) < (b as u8) { a } else { b }
}
