use core::borrow::Borrow;

pub const MAX_HOOKS: usize = 32;

pub const MAX_BREAKPOINTS_PER_PAGE: usize = 8;

pub const PAGE_MASK: u64 = 0xFFF;

const EPT_READ: u64 = 1 << 0;
const EPT_WRITE: u64 = 1 << 1;
pub const EPT_EXECUTE: u64 = 1 << 2;
const EPT_PFN_MASK: u64 = 0x000F_FFFF_FFFF_F000;
const EPT_MEMORY_TYPE_MASK: u64 = 0xF8;

#[repr(u32)]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum HookError {
    InvalidAddress = 1,
    NotMapped = 2,
    Unsupported = 3,
    NoFreeSlot = 4,
    NoMemory = 5,
    AlreadyExists = 6,
    NotFound = 7,
    HypercallsDisabled = 8,
    WrongPageKind = 9,
    UnsupportedCpu = 10,
}

impl HookError {
    pub const fn as_u32(self) -> u32 {
        self as u32
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum HookKind {
    /// 隐藏 `0xCC` + 恢复原页 + MTF 单步（内核/用户地址都可以）。
    Breakpoint = 0,
    DetourInternal = 1,
    Hide = 2,
    DetourTrampoline = 3,
    /// 隐藏 `0xCC` + INT3 自旋等待调试器应答。只允许用户态（低半区）地址。
    UserBreakpoint = 4,
}

impl TryFrom<u32> for HookKind {
    type Error = HookError;

    fn try_from(value: u32) -> Result<Self, Self::Error> {
        match value {
            0 => Ok(Self::Breakpoint),
            1 => Ok(Self::DetourInternal),
            2 => Ok(Self::Hide),
            3 => Ok(Self::DetourTrampoline),
            4 => Ok(Self::UserBreakpoint),
            _ => Err(HookError::InvalidAddress),
        }
    }
}

impl HookKind {
    pub const fn has_detour_trampoline(self) -> bool {
        matches!(self, Self::DetourInternal)
    }

    /// 两种断点共用同一套页状态机（fake page + `0xCC`、owner、RealDataOnly/RealPassthrough）。
    pub const fn is_breakpoint(self) -> bool {
        matches!(self, Self::Breakpoint | Self::UserBreakpoint)
    }

    /// INT3 自旋等待只对用户态断点成立。
    pub const fn is_user_breakpoint(self) -> bool {
        matches!(self, Self::UserBreakpoint)
    }
}

/// EPT 叶子的状态（一个被 hook 的页在任意时刻只处于其中一种）。
///
/// 命名规则：**`<指向哪一页><允许 guest 做什么>`**，两个维度都写进名字，避免
/// “Real / Fake / Original”这种要靠上下文才能猜的名字。
///
/// * `RealDataOnly` —— leaf 指向**真实页**，`R+W=1`、`X=0`：
///   数据读写直接落到真实页；取指触发 EPT violation，切到 [`Self::CopyExecOnly`]
///   （藏 `0xCC`/跳转桩的副本页）。这是 EPT 断点的静止态。
/// * `CopyExecOnly` —— leaf 指向**私有副本页**，`X=1`、`R=W=0`：
///   只允许执行副本（里面有 `0xCC` 或跳转桩）；任何数据访问都会退出，从而看到
///   真实字节。
/// * `CopyDataOnly` —— leaf 指向**clean 副本页**（真实页的拷贝、不含 `0xCC`），
///   `R+W=1`、`X=0`：数据读到原字节、取指陷入。字节补丁模式的“数据视图”。
/// * `RealPassthrough` —— leaf 指向**真实页**且保留原来的 R/W/X（通常 RWX）：
///   完全不拦截。用于已移除的页、MTF 单步一条指令、以及非 owner 核。
///
/// 也就是说：`RealDataOnly` 和 `RealPassthrough` 都指向真实页，区别只在执行位；
/// `CopyExecOnly` 和 `CopyDataOnly` 都指向私有副本，区别在“只给执行”还是“只给数据”。
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum PteState {
    RealDataOnly,
    CopyExecOnly,
    CopyDataOnly,
    RealPassthrough,
    HideData,
    HideExec,
    HideShadow,
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum SyncAction {
    Arm,
    Restore,
    None,
}

/// 一次命中的快照：这个断点**第一次**在哪里、由哪个线程/核触发。
///
/// 只记一次（`HookEntry::first_hit`），后续可以用来对照“断点装在哪、实际第一次
/// 停在哪”，或者做单步/报告。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct FirstHit {
    /// 触发地址（`page_va + offset`）。
    pub va: u64,
    /// 触发时的 guest RIP（trap 语义，仍指向 `INT3`）。
    pub rip: u64,
    /// 触发的 `ETHREAD`；普通断点（kind 0）不读线程，为 0。
    pub thread: u64,
    /// 触发的 vCPU。
    pub cpu: u32,
    /// 触发的 `RDTSC`。
    pub tsc: u64,
}

#[derive(Clone, Copy, Debug)]
pub struct HookEntry {
    pub active: bool,
    pub kind: HookKind,
    pub page_gpa: u64,
    pub page_va: u64,
    pub cr3: u64,
    pub offsets: [u16; MAX_BREAKPOINTS_PER_PAGE],
    pub offset_count: u8,
    pub fake_pfn: u64,
    pub original_pte: u64,
    pub page2_gpa: u64,
    pub fake_pfn2: u64,
    pub original_pte2: u64,
    pub page_count: u8,
    pub detour_va: u64,
    pub handler_va: u64,
    pub detour_stolen_len: u16,
    pub trampoline_va: u64,
    pub hits: u64,
    /// **一次性断点开关**：由 `HYPERCALL_BREAKPOINT_ONESHOT` 设置。
    ///
    /// 命中一次、被 Continue（或超时）之后就不再触发（`enabled` 置 false），
    /// 避免多线程反复命中互相干扰。
    pub one_shot: bool,
    /// 断点当前是否还需要触发；`false` 表示已被禁用，执行到这里不再停。
    ///
    /// 一次性断点在第一次命中被继续之后会置为 `false`（页恢复原状）；普通断点
    /// 一直是 `true`。
    pub enabled: bool,
    /// **临时单步 hook**：只为“跨页单步”装的一次性 hook。
    ///
    /// 目标下一次命中、调试器继续之后，这个 hook 就完成了使命，整个槽位删除
    /// （见 `remove_breakpoint_offset`）。用户断点永远是 `false`。
    pub step_hook: bool,
    /// **第一次**命中的位置/线程/时间；`None` 表示这个断点还没触发过。
    ///
    /// 只记一次，之后命中不再改（`hits` 照常递增）。用于后续对照/报告，例如
    /// “断点装在哪、第一次实际停在哪”。
    pub first_hit: Option<FirstHit>,
    /// 单步时临时加进来的一次性断点偏移；`u16::MAX` 表示没有。
    ///
    /// `SingleStep` 在 MTF 单步完成后、下一条指令取指之前，把“下一条指令”的
    /// 偏移临时加进 `offsets` 并重建 fake page，这样目标执行完一条指令就停在
    /// 下一条上；下一次应用等待项后这个偏移会被摘掉（一次性）。
    pub temp_offset: u16,
    /// 一次性断点属于哪个 `ETHREAD`（被单步的那个线程）。
    ///
    /// 只有这个线程命中才建等待项；别的线程命中只单步放行，不能把 owner 的
    /// 停止点抢走。等待项被取消/owner 变化时把它清成 0，表示“已失效”，目标
    /// 下一次命中时由 `handle_breakpoint` 摘掉这个偏移。
    pub temp_thread: u64,
    /// 已经打印过的诊断日志类别（[`crate::arch::x86_64::vmx::ept::hook::WaitNote`] 的位）。
    /// 每个断点每种原因只提示一次；安装新断点时随 `HookEntry::empty()` 归零。
    pub noted: u32,
    pub generation: u32,
    pub retired_generation: u32,
    /// **字节补丁模式**：用户断点不用 fake page 换页，而是直接把真实页上那个
    /// 字节改成 `0xCC`（保存原字节，继续/移除时恢复）。
    ///
    /// 这样命中的陷入完全来自真实 `0xCC` 指令，不依赖 EPT 翻译切换是否被
    /// 目标 CPU 观察到——真机上换页 + INVEPT 有时仍会执行旧指令。
    pub byte_mode: bool,
    /// `byte_mode` 下被我们改掉的那个字节偏移。
    pub patched_offset: u16,
    /// `byte_mode` 下被改掉之前的原始字节。
    pub saved_byte: u8,
    /// **延迟补丁**：clean 视图还没在所有核生效之前，先不写 `0xCC`。
    ///
    /// 安装分两步：驱动先 `HOOK_INSTALL`（建 clean 副本、本核切 clean 视图、
    /// 置这个标志），然后按核轮询 `HOOK_APPLY` 让每个核都切到 clean 视图，
    /// 最后 `USER_BREAKPOINT_COMMIT` 才真正写 `0xCC`。这样任何核在任何时刻做数据读，
    /// 看到的都是副本里的原字节，没有“能看到补丁”的窗口。
    pub pending_patch: bool,
}

impl HookEntry {
    pub const fn empty() -> Self {
        Self {
            active: false,
            kind: HookKind::Breakpoint,
            page_gpa: 0,
            page_va: 0,
            cr3: 0,
            offsets: [0; MAX_BREAKPOINTS_PER_PAGE],
            offset_count: 0,
            fake_pfn: 0,
            original_pte: 0,
            page2_gpa: 0,
            fake_pfn2: 0,
            original_pte2: 0,
            page_count: 1,
            detour_va: 0,
            handler_va: 0,
            detour_stolen_len: 0,
            trampoline_va: 0,
            hits: 0,
            one_shot: false,
            enabled: true,
            step_hook: false,
            first_hit: None,
            temp_offset: u16::MAX,
            temp_thread: 0,
            noted: 0,
            generation: 0,
            retired_generation: 0,
            byte_mode: false,
            patched_offset: u16::MAX,
            saved_byte: 0,
            pending_patch: false,
        }
    }

    /// [`PteState::CopyExecOnly`] 态的 PTE：私有副本页，`X=1`、`R=W=0`。
    pub fn copy_exec_only_pte(&self) -> u64 {
        Self::fake_pte_for(self.original_pte, self.fake_pfn)
    }

    /// 第二页的 [`PteState::CopyExecOnly`] 态 PTE。
    pub fn copy_exec_only_pte2(&self) -> u64 {
        Self::fake_pte_for(self.original_pte2, self.fake_pfn2)
    }

    fn fake_pte_for(original_pte: u64, fake_pfn: u64) -> u64 {
        let pte = original_pte & !(EPT_READ | EPT_WRITE | EPT_PFN_MASK);

        pte | EPT_EXECUTE | (fake_pfn << 12)
    }

    /// [`PteState::RealDataOnly`] 态的 PTE：真实页，保留原 R/W，清掉 X（取指会
    /// 触发 EPT violation）。
    pub fn real_data_only_pte(&self) -> u64 {
        self.original_pte & !EPT_EXECUTE
    }

    /// 第二页的 [`PteState::RealDataOnly`] 态 PTE。
    pub fn real_data_only_pte2(&self) -> u64 {
        self.original_pte2 & !EPT_EXECUTE
    }

    /// [`PteState::CopyDataOnly`] 态的 PTE：clean 副本页，`R+W=1`、`X=0`。
    ///
    /// 数据读写走这一页 —— 读到的永远是**原始字节**（副本在打 `0xCC` 之前拷的），
    /// 所以断点对数据访问不可见；取指因为 `X=0` 会触发 EPT violation，处理器
    /// 再把 leaf 换成真实页（[`Self::original_pte`]，里面有 `0xCC`），目标重新
    /// 取指执行 `0xCC` → `#BP`。
    pub fn copy_data_only_pte(&self) -> u64 {
        let pte = self.original_pte & !(EPT_READ | EPT_WRITE | EPT_EXECUTE | EPT_PFN_MASK);

        pte | EPT_READ | EPT_WRITE | (self.fake_pfn << 12)
    }

    pub fn hide_data_pte(&self, zero_pfn: u64) -> u64 {
        let pte = self.original_pte & !(EPT_READ | EPT_WRITE | EPT_EXECUTE | EPT_PFN_MASK);

        pte | EPT_READ | EPT_WRITE | (zero_pfn << 12)
    }

    pub fn hide_exec_pte(&self) -> u64 {
        (self.original_pte & !(EPT_READ | EPT_WRITE)) | EPT_EXECUTE
    }

    pub fn hide_shadow_pte(&self, shadow_pfn: u64) -> u64 {
        let pte = self.original_pte & !(EPT_READ | EPT_WRITE | EPT_EXECUTE | EPT_PFN_MASK);

        pte | EPT_READ | EPT_WRITE | EPT_EXECUTE | (shadow_pfn << 12)
    }

    pub fn has_breakpoint(&self, offset: u16) -> bool {
        self.offsets[..self.offset_count as usize].contains(&offset)
    }

    pub fn add_breakpoint(&mut self, offset: u16) -> bool {
        if self.has_breakpoint(offset) || self.offset_count as usize >= MAX_BREAKPOINTS_PER_PAGE {
            return false;
        }

        self.offsets[self.offset_count as usize] = offset;
        self.offset_count += 1;

        true
    }

    pub fn remove_breakpoint(&mut self, offset: u16) -> bool {
        let count = self.offset_count as usize;

        let Some(position) = self.offsets[..count]
            .iter()
            .position(|candidate| *candidate == offset)
        else {
            return false;
        };

        for position in position..count - 1 {
            self.offsets[position] = self.offsets[position + 1];
        }

        self.offset_count -= 1;

        true
    }

    /// 记一个一次性断点（`SingleStep` 用）：加进 `offsets` 并记为临时。
    pub fn set_temp_breakpoint(&mut self, offset: u16) -> bool {
        if !self.add_breakpoint(offset) {
            return false;
        }

        self.temp_offset = offset;

        true
    }

    /// 摘掉一次性断点（如果有），返回被摘掉的偏移。
    pub fn take_temp_breakpoint(&mut self) -> Option<u16> {
        if self.temp_offset == u16::MAX {
            return None;
        }

        let offset = self.temp_offset;
        self.temp_offset = u16::MAX;
        self.temp_thread = 0;
        self.remove_breakpoint(offset);

        Some(offset)
    }

    /// 让一次性断点失效（等待项被取消/owner 变化）：保留偏移，等目标下一次
    /// 命中时由 `handle_breakpoint` 在目标 CPU 上摘掉并单步放行。
    #[allow(dead_code)] // 仅 native-bp 路径使用；关闭该 feature 时保留 ABI。
    pub fn invalidate_temp_breakpoint(&mut self) -> bool {
        if self.temp_offset == u16::MAX || self.temp_thread == 0 {
            return false;
        }

        self.temp_thread = 0;

        true
    }

    /// 当前 `temp_offset` 是否就是 `offset`（这次命中来自单步的一次性断点）。
    pub fn is_temp_breakpoint(&self, offset: u16) -> bool {
        self.temp_offset != u16::MAX && self.temp_offset == offset
    }
}

pub fn sync_action(entry: &HookEntry, applied: u32, pending_page: Option<u64>) -> SyncAction {
    if pending_page == Some(entry.page_gpa) {
        return SyncAction::None;
    }

    if entry.active && entry.generation > applied {
        SyncAction::Arm
    } else if !entry.active && entry.retired_generation > applied {
        SyncAction::Restore
    } else {
        SyncAction::None
    }
}

pub fn classify(current: u64, page_gpa: u64, fake_pfn: u64) -> PteState {
    let execute = current & EPT_EXECUTE != 0;
    let pfn = (current & EPT_PFN_MASK) >> 12;

    if !execute && pfn == page_gpa >> 12 {
        PteState::RealDataOnly
    } else if execute
        && current & EPT_READ == 0
        && current & EPT_WRITE == 0
        && pfn == fake_pfn
    {
        PteState::CopyExecOnly
    } else {
        PteState::RealPassthrough
    }
}

pub fn keep_memory_type(desired: u64, current: u64) -> u64 {
    (desired & !EPT_MEMORY_TYPE_MASK) | (current & EPT_MEMORY_TYPE_MASK)
}

pub fn classify_hide(
    current: u64,
    page_gpa: u64,
    zero_pfn: u64,
    shadow_pfn: u64,
) -> Option<PteState> {
    let read = current & EPT_READ != 0;
    let write = current & EPT_WRITE != 0;
    let execute = current & EPT_EXECUTE != 0;
    let pfn = (current & EPT_PFN_MASK) >> 12;

    if execute && read && write && pfn == page_gpa >> 12 {
        Some(PteState::RealPassthrough)
    } else if !execute && read && write && zero_pfn != 0 && pfn == zero_pfn {
        Some(PteState::HideData)
    } else if execute && !read && !write && pfn == page_gpa >> 12 {
        Some(PteState::HideExec)
    } else if execute && read && write && shadow_pfn != 0 && pfn == shadow_pfn {
        Some(PteState::HideShadow)
    } else {
        None
    }
}

pub fn find_page<T: Borrow<HookEntry>>(entries: &[T], page_gpa: u64) -> Option<usize> {
    entries.iter().position(|slot| {
        let entry = slot.borrow();
        entry.active
            && (entry.page_gpa == page_gpa
                || (entry.page_count > 1 && entry.page2_gpa == page_gpa))
    })
}

pub fn find_any_page<T: Borrow<HookEntry>>(entries: &[T], page_gpa: u64) -> Option<usize> {
    entries.iter().position(|slot| {
        let entry = slot.borrow();
        let used = entry.active || entry.retired_generation != 0;

        used && (entry.page_gpa == page_gpa
            || (entry.page_count > 1 && entry.page2_gpa == page_gpa))
    })
}

pub fn find_page_or_retired<T: Borrow<HookEntry>>(
    entries: &[T],
    page_gpa: u64,
) -> Option<usize> {
    if let Some(index) = find_page(entries, page_gpa) {
        return Some(index);
    }

    entries.iter().position(|slot| {
        let entry = slot.borrow();
        !entry.active
            && entry.retired_generation != 0
            && (entry.page_gpa == page_gpa
                || (entry.page_count > 1 && entry.page2_gpa == page_gpa))
    })
}

pub fn find_by_va<T: Borrow<HookEntry>>(entries: &[T], va: u64, cr3: u64) -> Option<usize> {
    let mut fallback = None;

    for (index, slot) in entries.iter().enumerate() {
        let entry = slot.borrow();

        if !entry.active {
            continue;
        }

        let matches = match entry.kind {
            HookKind::Breakpoint | HookKind::UserBreakpoint => {
                entry.page_va + (va & PAGE_MASK) == va
                    && entry.has_breakpoint((va & PAGE_MASK) as u16)
            }
            HookKind::DetourInternal => entry.detour_va == va,
            HookKind::DetourTrampoline => entry.detour_va == va,
            HookKind::Hide => entry.page_va == (va & !PAGE_MASK),
        };

        if !matches {
            continue;
        }

        if entry.cr3 == cr3 {
            return Some(index);
        }

        fallback.get_or_insert(index);
    }

    fallback
}

pub fn find_free_slot<T: Borrow<HookEntry>>(
    entries: &[T],
    page_gpa: u64,
    page2_gpa: u64,
) -> Option<usize> {
    let page_count = if page2_gpa == 0 { 1 } else { 2 };

    if let Some(index) = entries.iter().position(|slot| {
        let entry = slot.borrow();
        !entry.active
            && entry.retired_generation != 0
            && entry.page_gpa == page_gpa
            && entry.page2_gpa == page2_gpa
            && entry.page_count == page_count
    }) {
        return Some(index);
    }

    entries.iter().position(|slot| {
        let entry = slot.borrow();
        !entry.active && entry.retired_generation == 0
    })
}

pub fn retire(entry: &mut HookEntry, generation: u32) {
    entry.active = false;
    entry.retired_generation = generation;
}

#[cfg(test)]
mod tests {
    use super::*;

    fn entry(page_gpa: u64, kind: HookKind, cr3: u64, generation: u32) -> HookEntry {
        HookEntry {
            active: true,
            kind,
            page_gpa,
            page_va: 0xFFFF_F800_0000_0000 | page_gpa,
            cr3,
            original_pte: ((page_gpa >> 12) << 12) | 0x7,
            fake_pfn: 0x1234,
            generation,
            ..HookEntry::empty()
        }
    }

    #[test]
    fn copy_exec_only_pte_is_execute_only_and_keeps_the_memory_type() {
        let mut original = 0x5678 << 12 | 0x7;
        original |= 0b101 << 3; // 内存类型 + IPAT 位

        let mut hook = entry(0x5678_000, HookKind::Breakpoint, 1, 1);
        hook.original_pte = original;
        hook.fake_pfn = 0x9ABC;

        let copy = hook.copy_exec_only_pte();

        assert_eq!(copy & EPT_READ, 0);
        assert_eq!(copy & EPT_WRITE, 0);
        assert_ne!(copy & EPT_EXECUTE, 0);
        assert_eq!((copy & EPT_PFN_MASK) >> 12, 0x9ABC);
        assert_eq!(copy & EPT_MEMORY_TYPE_MASK, original & EPT_MEMORY_TYPE_MASK);
    }

    #[test]
    fn real_data_only_pte_keeps_the_real_page_and_denies_execute() {
        let mut original = 0x5678 << 12 | 0x7;
        original |= 0b110 << 3;

        let mut hook = entry(0x5678_000, HookKind::Breakpoint, 1, 1);
        hook.original_pte = original;

        let real = hook.real_data_only_pte();

        assert_eq!(real & EPT_EXECUTE, 0);
        assert_ne!(real & EPT_READ, 0);
        assert_ne!(real & EPT_WRITE, 0);
        assert_eq!((real & EPT_PFN_MASK) >> 12, 0x5678);
        assert_eq!(real & EPT_MEMORY_TYPE_MASK, original & EPT_MEMORY_TYPE_MASK);
    }

    #[test]
    fn hide_pte_states_are_read_write_or_execute_only() {
        let mut hook = entry(0x5678_000, HookKind::Hide, 1, 1);
        hook.original_pte = (0x5678 << 12) | 0x7;

        let data = hook.hide_data_pte(0x9ABC);
        assert_ne!(data & EPT_READ, 0);
        assert_ne!(data & EPT_WRITE, 0);
        assert_eq!(data & EPT_EXECUTE, 0);
        assert_eq!((data & EPT_PFN_MASK) >> 12, 0x9ABC);

        let exec = hook.hide_exec_pte();
        assert_eq!(exec & EPT_READ, 0);
        assert_eq!(exec & EPT_WRITE, 0);
        assert_ne!(exec & EPT_EXECUTE, 0);
        assert_eq!((exec & EPT_PFN_MASK) >> 12, 0x5678);

        let shadow = hook.hide_shadow_pte(0xDEF0);
        assert_ne!(shadow & EPT_READ, 0);
        assert_ne!(shadow & EPT_WRITE, 0);
        assert_ne!(shadow & EPT_EXECUTE, 0);
        assert_eq!((shadow & EPT_PFN_MASK) >> 12, 0xDEF0);

        assert_eq!(
            classify_hide(data, 0x5678_000, 0x9ABC, 0xDEF0),
            Some(PteState::HideData)
        );
        assert_eq!(
            classify_hide(exec, 0x5678_000, 0x9ABC, 0xDEF0),
            Some(PteState::HideExec)
        );
        assert_eq!(
            classify_hide(shadow, 0x5678_000, 0x9ABC, 0xDEF0),
            Some(PteState::HideShadow)
        );
        assert_eq!(
            classify_hide(hook.original_pte, 0x5678_000, 0x9ABC, 0xDEF0),
            Some(PteState::RealPassthrough)
        );
    }

    #[test]
    fn classify_recognises_the_three_states() {
        let hook = entry(0x5678_000, HookKind::Breakpoint, 1, 1);
        let page = hook.page_gpa;
        let fake_pfn = hook.fake_pfn;

        assert_eq!(
            classify(hook.real_data_only_pte(), page, fake_pfn),
            PteState::RealDataOnly
        );
        assert_eq!(
            classify(hook.copy_exec_only_pte(), page, fake_pfn),
            PteState::CopyExecOnly
        );
        assert_eq!(
            classify(hook.original_pte, page, fake_pfn),
            PteState::RealPassthrough
        );

        let other = (0x9999 << 12) | 0x5;
        assert_eq!(classify(other, page, fake_pfn), PteState::RealPassthrough);
    }

    #[test]
    fn breakpoint_offsets_are_bounded_and_unique() {
        let mut hook = HookEntry::empty();

        assert!(hook.add_breakpoint(0x10));
        assert!(hook.add_breakpoint(0x20));
        assert!(!hook.add_breakpoint(0x10));
        assert!(hook.has_breakpoint(0x20));

        for offset in 2..MAX_BREAKPOINTS_PER_PAGE {
            assert!(hook.add_breakpoint(offset as u16 * 0x30));
        }

        assert!(!hook.add_breakpoint(0x999));
        assert!(hook.remove_breakpoint(0x10));
        assert!(!hook.remove_breakpoint(0x10));
        assert!(!hook.has_breakpoint(0x10));
    }

    #[test]
    fn a_temp_breakpoint_is_removed_exactly_once() {
        let mut hook = HookEntry::empty();

        assert!(hook.set_temp_breakpoint(0x10));
        assert!(hook.has_breakpoint(0x10));
        assert!(hook.is_temp_breakpoint(0x10));
        assert!(!hook.is_temp_breakpoint(0x20));

        assert_eq!(hook.take_temp_breakpoint(), Some(0x10));
        assert!(!hook.has_breakpoint(0x10));
        assert!(!hook.is_temp_breakpoint(0x10));
        assert_eq!(hook.take_temp_breakpoint(), None);

        // 普通断点不会被误当成一次性断点。
        assert!(hook.add_breakpoint(0x30));
        assert!(!hook.is_temp_breakpoint(0x30));
        assert_eq!(hook.take_temp_breakpoint(), None);
        assert!(hook.has_breakpoint(0x30));
    }

    #[test]
    fn an_invalidated_temp_breakpoint_is_removed_on_the_next_hit() {
        let mut hook = HookEntry::empty();

        assert!(hook.set_temp_breakpoint(0x20));
        hook.temp_thread = 0xFFFF_8000_1234_5000;
        assert!(hook.is_temp_breakpoint(0x20));

        // 等待项被取消 / owner 变化：只标记失效，偏移还在（要等目标 CPU 摘）。
        assert!(hook.invalidate_temp_breakpoint());
        assert_eq!(hook.temp_thread, 0);
        assert!(hook.has_breakpoint(0x20));
        assert!(hook.is_temp_breakpoint(0x20));
        assert!(!hook.invalidate_temp_breakpoint());

        // 目标下一次命中时由 handle_breakpoint 摘除。
        assert_eq!(hook.take_temp_breakpoint(), Some(0x20));
        assert!(!hook.has_breakpoint(0x20));
    }

    #[test]
    fn free_slot_prefers_the_same_retired_page() {
        let mut entries = [HookEntry::empty(); MAX_HOOKS];

        assert_eq!(find_free_slot(&entries, 0x1000, 0), Some(0));

        entries[0] = entry(0x1000, HookKind::Breakpoint, 1, 5);
        retire(&mut entries[0], 6);

        assert_eq!(find_free_slot(&entries, 0x1000, 0), Some(0));

        assert_eq!(find_free_slot(&entries, 0x2000, 0), Some(1));

        for (index, slot) in entries.iter_mut().enumerate() {
            *slot = entry(0x1000 * (index as u64 + 1), HookKind::Breakpoint, 1, 1);
            retire(slot, 2);
        }

        assert_eq!(find_free_slot(&entries, 0x99000, 0), None);

        let mut cross = [HookEntry::empty(); MAX_HOOKS];
        cross[0] = entry(0x1000, HookKind::DetourInternal, 1, 1);
        cross[0].page_count = 2;
        cross[0].page2_gpa = 0x2000;
        retire(&mut cross[0], 2);

        assert_eq!(find_free_slot(&cross, 0x1000, 0), Some(1));
        assert_eq!(find_free_slot(&cross, 0x1000, 0x3000), Some(1));
        assert_eq!(find_free_slot(&cross, 0x1000, 0x2000), Some(0));
    }

    #[test]
    fn cross_page_entry_matches_both_pages() {
        let mut entries = [HookEntry::empty(); MAX_HOOKS];

        entries[0] = entry(0x1000, HookKind::DetourInternal, 1, 1);
        entries[0].page_count = 2;
        entries[0].page2_gpa = 0x2000;
        entries[0].original_pte2 = 0x2007;
        entries[0].fake_pfn2 = 0x5678;

        assert_eq!(find_page(&entries, 0x1000), Some(0));
        assert_eq!(find_page(&entries, 0x2000), Some(0));
        assert_eq!(find_page(&entries, 0x3000), None);

        assert_eq!(find_any_page(&entries, 0x1000), Some(0));
        assert_eq!(find_any_page(&entries, 0x2000), Some(0));
        assert_eq!(find_any_page(&entries, 0x3000), None);

        assert_eq!(find_page_or_retired(&entries, 0x1000), Some(0));
        assert_eq!(find_page_or_retired(&entries, 0x2000), Some(0));

        assert_eq!(entries[0].copy_exec_only_pte2() & EPT_PFN_MASK, 0x5678 << 12);
        assert_eq!(entries[0].real_data_only_pte2() & EPT_EXECUTE, 0);
    }

    #[test]
    fn find_by_va_prefers_the_installing_cr3() {
        let mut entries = [HookEntry::empty(); MAX_HOOKS];

        let va = 0x7FF6_0000_1234;

        entries[0] = entry(0x1000, HookKind::Breakpoint, 0x1111, 1);
        entries[0].page_va = 0x7FF6_0000_1000;
        entries[0].add_breakpoint(0x234);

        entries[1] = entry(0x2000, HookKind::Breakpoint, 0x2222, 1);
        entries[1].page_va = 0x7FF6_0000_1000;
        entries[1].add_breakpoint(0x234);

        assert_eq!(find_by_va(&entries, va, 0x2222), Some(1));
        assert_eq!(find_by_va(&entries, va, 0x1111), Some(0));

        assert_eq!(find_by_va(&entries, va, 0x3333), Some(0));

        assert_eq!(find_by_va(&entries, va + 1, 0x1111), None);
    }

    #[test]
    fn find_by_va_matches_detours_by_target_address() {
        let mut entries = [HookEntry::empty(); MAX_HOOKS];

        entries[0] = entry(0x1000, HookKind::DetourInternal, 0x1111, 1);
        entries[0].detour_va = 0xFFFF_F800_DEAD_BEEF;

        assert_eq!(
            find_by_va(&entries, 0xFFFF_F800_DEAD_BEEF, 0x1111),
            Some(0)
        );
        assert_eq!(find_by_va(&entries, 0xFFFF_F800_DEAD_BEE0, 0x1111), None);
    }

    #[test]
    fn page_lookup_prefers_active_and_falls_back_to_retired() {
        let mut entries = [HookEntry::empty(); MAX_HOOKS];

        entries[0] = entry(0x1000, HookKind::Breakpoint, 1, 1);
        retire(&mut entries[0], 2);

        assert_eq!(find_page(&entries, 0x1000), None);
        assert_eq!(find_page_or_retired(&entries, 0x1000), Some(0));

        entries[1] = entry(0x1000, HookKind::Breakpoint, 1, 3);

        assert_eq!(find_page(&entries, 0x1000), Some(1));
        assert_eq!(find_page_or_retired(&entries, 0x1000), Some(1));
    }

    #[test]
    fn sync_arms_new_and_restores_retired() {
        let mut hook = entry(0x1000, HookKind::Breakpoint, 1, 5);

        assert_eq!(sync_action(&hook, 4, None), SyncAction::Arm);
        assert_eq!(sync_action(&hook, 5, None), SyncAction::None);

        retire(&mut hook, 6);

        assert_eq!(sync_action(&hook, 5, None), SyncAction::Restore);
        assert_eq!(sync_action(&hook, 6, None), SyncAction::None);

        hook.active = true;
        hook.generation = 7;
        assert_eq!(sync_action(&hook, 6, Some(0x1000)), SyncAction::None);
    }

    #[test]
    fn lagging_cpu_restores_even_after_reinstall() {
        let mut hook = entry(0x1000, HookKind::Breakpoint, 1, 12);
        retire(&mut hook, 13);

        assert_eq!(sync_action(&hook, 9, None), SyncAction::Restore);

        assert_eq!(sync_action(&hook, 13, None), SyncAction::None);
    }

    #[test]
    fn two_vcpu_generation_sequence() {
        let mut hook = entry(0x1000, HookKind::Breakpoint, 1, 1);
        let mut cpu_a = 0u32;
        let mut cpu_b = 0u32;

        cpu_a = 1;
        assert_eq!(sync_action(&hook, cpu_b, None), SyncAction::Arm);
        cpu_b = 1;

        retire(&mut hook, 2);
        assert_eq!(sync_action(&hook, cpu_a, None), SyncAction::Restore);
        cpu_a = 2;
        assert_eq!(sync_action(&hook, cpu_b, None), SyncAction::Restore);
        cpu_b = 2;

        hook = entry(0x1000, HookKind::Breakpoint, 1, 3);
        assert_eq!(sync_action(&hook, cpu_a, None), SyncAction::Arm);
        cpu_a = 3;
        assert_eq!(sync_action(&hook, cpu_b, None), SyncAction::Arm);
        cpu_b = 3;

        retire(&mut hook, 4);
        assert_eq!(sync_action(&hook, cpu_a, None), SyncAction::Restore);
        assert_eq!(sync_action(&hook, cpu_b, None), SyncAction::Restore);
    }

    #[test]
    fn keep_memory_type_only_replaces_bits_3_to_7() {
        let desired = 0xAAAA_AAAA_AAAA_AAAAu64;
        let current = 0x0000_0000_0000_00F8u64;

        let merged = keep_memory_type(desired, current);

        assert_eq!(merged & EPT_MEMORY_TYPE_MASK, current & EPT_MEMORY_TYPE_MASK);
        assert_eq!(merged & !EPT_MEMORY_TYPE_MASK, desired & !EPT_MEMORY_TYPE_MASK);
    }

    #[test]
    fn only_detour_allocates_trampoline_page() {
        assert!(!HookKind::Breakpoint.has_detour_trampoline());
        assert!(HookKind::DetourInternal.has_detour_trampoline());
        assert!(!HookKind::DetourTrampoline.has_detour_trampoline());
        assert!(!HookKind::Hide.has_detour_trampoline());
        assert!(!HookKind::UserBreakpoint.has_detour_trampoline());
        assert_eq!(HookKind::try_from(3), Ok(HookKind::DetourTrampoline));
    }

    #[test]
    fn user_breakpoint_is_a_breakpoint_but_only_it_waits() {
        assert_eq!(HookKind::try_from(4), Ok(HookKind::UserBreakpoint));
        assert_eq!(HookKind::try_from(5), Err(HookError::InvalidAddress));

        assert!(HookKind::Breakpoint.is_breakpoint());
        assert!(HookKind::UserBreakpoint.is_breakpoint());
        assert!(!HookKind::DetourInternal.is_breakpoint());
        assert!(!HookKind::Hide.is_breakpoint());
        assert!(!HookKind::DetourTrampoline.is_breakpoint());

        assert!(HookKind::UserBreakpoint.is_user_breakpoint());
        assert!(!HookKind::Breakpoint.is_user_breakpoint());
    }
}
