use super::*;
use super::super::state;
use super::detour;

pub(crate) fn existing_original_pte(table: &[HookSlot], page_gpa: u64) -> Option<u64> {
    let index = state::find_any_page(table, page_gpa)?;
    let entry = &table[index];

    let pte = if entry.page_gpa == page_gpa {
        entry.original_pte
    } else {
        entry.original_pte2
    };

    (pte != 0).then_some(pte)
}

pub(crate) fn ensure_page(page: &mut Page<[u8; PAGE_SIZE]>) -> Result<(), HookError> {
    if page.virt().as_u64() == 0 {
        *page = memory::alloc::<[u8; PAGE_SIZE]>().map_err(|_| HookError::NoMemory)?;
    }

    // fake/trampoline 页必须落在 identity EPT 覆盖的范围内。
    if page.phys().as_u64() >= DIRECT_MAP_SIZE {
        return Err(HookError::NoMemory);
    }

    Ok(())
}

pub(crate) fn rebuild_fake_page(
    entry: &HookEntry,
    fake: &mut Page<[u8; PAGE_SIZE]>,
) -> Result<(), HookError> {
    copy_from_gpa(entry.page_gpa, fake.as_mut_ptr() as *mut u8, PAGE_SIZE)?;

    for index in 0..entry.offset_count as usize {
        let offset = entry.offsets[index] as usize;

        if offset >= PAGE_SIZE {
            return Err(HookError::InvalidAddress);
        }

        fake[offset] = 0xCC;
    }

    Ok(())
}

/// 把 fake page 上 `offset` 处的 `0xCC` 换回真实页的字节（只用于单步期间）。
///
/// 为什么需要它：MTF 单步要求目标**重新取指**执行真实指令。如果 CPU 还在用
/// 旧的 EPT 翻译（TLB 没刷干净）或者复用了同一条线性地址上旧的解码结果，
/// 它会再次执行 `0xCC`，于是 `Continue` 永远等不到 MTF 退出（日志表现为
/// “警告 seq=N 的 Continue 没有等到 MTF 退出”无限循环）。把 fake page 的这一
/// 个字节也换成真实指令后，即便取指仍落在 fake page 上，执行的也是真实指令。
///
/// 单步结束（`settle_page`）时会用 [`rebuild_fake_page`] 把 `0xCC` 放回去。
pub(crate) fn patch_fake_byte_from_real(
    entry: &HookEntry,
    fake: &mut Page<[u8; PAGE_SIZE]>,
    offset: u16,
) -> Result<(), HookError> {
    if offset as usize >= PAGE_SIZE {
        return Err(HookError::InvalidAddress);
    }

    let mut byte = [0u8; 1];

    copy_from_gpa(entry.page_gpa + offset as u64, byte.as_mut_ptr(), 1)?;

    fake[offset as usize] = byte[0];

    Ok(())
}

pub(crate) fn detour_needs_second_page(page_gpa: u64, offset: u16) -> Result<bool, HookError> {
    let tail = PAGE_SIZE - offset as usize;
    let head_len = tail.min(detour::MAX_STOLEN_LEN);
    let mut head = [0u8; detour::MAX_STOLEN_LEN];

    copy_from_gpa(page_gpa + offset as u64, head.as_mut_ptr(), head_len)?;

    Ok(offset as usize + detour::ABSOLUTE_JUMP_LEN > PAGE_SIZE
        || detour::needs_second_page(&head[..head_len]))
}

pub(crate) fn copy_from_gpa(gpa: u64, destination: *mut u8, length: usize) -> Result<(), HookError> {
    let end = gpa
        .checked_add(length as u64)
        .ok_or(HookError::InvalidAddress)?;

    if end > DIRECT_MAP_SIZE {
        return Err(HookError::NotMapped);
    }

    let source = paging::phys_to_virt(PhysAddr::new(gpa)).as_ptr::<u8>();
    let mut exception_info = HostExceptionInfo::new();

    if !safe_routine::memcpy_safe(&mut exception_info, destination, source, length) {
        return Err(HookError::NotMapped);
    }

    Ok(())
}

pub(crate) fn read_gpa_byte(gpa: u64) -> Result<u8, HookError> {
    let mut byte = 0u8;

    copy_from_gpa(gpa, &mut byte as *mut u8, 1)?;

    Ok(byte)
}

/// 对 GPA 处的一个字节做“同值写”，并 `CLFLUSH` 让写尽快可见。
///
/// ## 为什么需要它（指令解码缓存）
///
/// x86 **没有**任何一条指令能直接清掉解码缓存（DSB/uop cache）：`INVEPT` /
/// `INVVPID` 只失效**翻译**，`INVLPG` 只失效 TLB，`CPUID`/`MOV CR3` 只是让
/// 流水线串行化。真正被架构保证的机制只有**自修改代码检测**：对同一物理行做
/// 一次 store，CPU 会作废该行上已经译码的指令，之后取指必须重新解码。
///
/// 所以每次切换 EPT leaf 之后，都对被断点覆盖的字节做一次“原值 store”
/// （内容不变，但产生了 store），再 `CLFLUSH`+`MFENCE` 让这次写入尽快落到
/// 所有缓存层，避免取指继续用旧的译码结果（真机上“时好时坏”的根因）。
pub(crate) fn poke_gpa(gpa: u64) -> Result<(), HookError> {
    if gpa >= DIRECT_MAP_SIZE {
        return Err(HookError::NotMapped);
    }

    let pointer = paging::phys_to_virt(PhysAddr::new(gpa)).as_mut_ptr::<u8>();

    unsafe {
        let byte = core::ptr::read_volatile(pointer);

        core::ptr::write_volatile(pointer, byte);

        // 把这一行从所有缓存层清出去，并保证 store/刷新按顺序完成。
        core::arch::asm!("clflush [{pointer}]", pointer = in(reg) pointer, options(nostack, preserves_flags));
        core::arch::asm!("mfence", options(nostack, preserves_flags));
    }

    Ok(())
}

/// 直接改写 GPA 处的一个字节（字节补丁模式用）。
///
/// 写的是**真实物理页**（宿主直映射），不经过 guest 页表，所以即使目标进程
/// 把代码页映射成只读也能改；写完再读回校验。
pub(crate) fn write_gpa_byte(gpa: u64, value: u8) -> Result<(), HookError> {
    if gpa >= DIRECT_MAP_SIZE {
        return Err(HookError::NotMapped);
    }

    let pointer = paging::phys_to_virt(PhysAddr::new(gpa)).as_mut_ptr::<u8>();

    unsafe {
        core::ptr::write_volatile(pointer, value);

        if core::ptr::read_volatile(pointer) != value {
            return Err(HookError::NotMapped);
        }
    }

    Ok(())
}

/// Write `bytes` to guest physical memory through the host direct map.
///
/// Used to place the origin trampoline into the driver's NonPagedPool page.
/// The page is not verified to belong to the guest: the caller (the driver)
/// supplied the VA, and the hypervisor translated it before calling here.
pub(crate) fn write_gpa(gpa: u64, bytes: &[u8]) -> Result<(), HookError> {
    let end = gpa
        .checked_add(bytes.len() as u64)
        .ok_or(HookError::InvalidAddress)?;

    if end > DIRECT_MAP_SIZE {
        return Err(HookError::NotMapped);
    }

    let destination = paging::phys_to_virt(PhysAddr::new(gpa)).as_mut_ptr::<u8>();
    let mut exception_info = HostExceptionInfo::new();

    if !safe_routine::memcpy_safe(
        &mut exception_info,
        destination,
        bytes.as_ptr(),
        bytes.len(),
    ) {
        return Err(HookError::NotMapped);
    }

    Ok(())
}

/// Touch every cache line of a freshly written range so instruction fetch sees
/// the new bytes (same-value stores, see [`poke_gpa`]).
pub(crate) fn poke_range(gpa: u64, length: usize) {
    for offset in (0..length).step_by(64) {
        let _ = poke_gpa(gpa + offset as u64);
    }
}

/// 让被断点覆盖的字节“重新取指”：真实页和 fake 页各做一次同值写。
///
/// 两个方向都要做：
/// * 切到 fake 页时，陈旧解码来自**真实页**；
/// * 切回真实页（Continue 单步）时，陈旧解码来自**fake 页**的 `0xCC`
///   （这就是“Continue 没等到 MTF 退出”死循环的成因）。
pub(crate) fn poke_breakpoint_bytes(entry: &HookEntry) {
    // Hide 不改写字节；断点和两种 detour 都在 `offset` 处写了 14 字节
    // （detour）或 `0xCC`（断点）。detour 也必须 poke，否则 CPU 可能继续用
    // 真实页上已经译码的旧 prologue，执行到跳转覆盖范围中间时取到坏指令
    // （NtCreateFile+7 的 page fault 就是这个原因）。
    if entry.kind == HookKind::Hide {
        return;
    }

    let copy_pte = entry.copy_exec_only_pte();
    let copy_base = if copy_pte == 0 {
        0
    } else {
        (copy_pte >> 12) << 12
    };

    let copy2_pte = entry.copy_exec_only_pte2();
    let copy2_base = if copy2_pte == 0 {
        0
    } else {
        (copy2_pte >> 12) << 12
    };

    let count = if entry.kind.is_breakpoint() {
        entry.offset_count as usize
    } else {
        1
    };

    for index in 0..count {
        let offset = entry.offsets[index] as u64;

        let _ = poke_gpa(entry.page_gpa + offset);

        if copy_base != 0 {
            let _ = poke_gpa(copy_base + offset);
        }
    }

    if entry.page_count > 1 {
        if entry.page2_gpa != 0 {
            let _ = poke_gpa(entry.page2_gpa);
        }

        if copy2_base != 0 {
            let _ = poke_gpa(copy2_base);
        }
    }
}

// Windows x64 的 KPCR 布局（Win7–Win11 稳定）：`KPCR.Prcb` 在 0x180，
// `KPRCB.CurrentThread` 是 Prcb 的第一个指针，所以 ETHREAD 在 KPCR+0x188。
const KPCR_CURRENT_THREAD_OFFSET: u64 = 0x180 + 0x8;

/// 用给定 CR3 读 guest 虚拟地址处的 `u64`；读不到返回 `None`。
pub(crate) fn read_guest_u64(cr3: u64, address: u64) -> Option<u64> {
    let phys = guest_memory::translate_guest(cr3, address, false, false, false).ok()?;
    let mut value = 0u64;

    copy_from_gpa(phys, &mut value as *mut u64 as *mut u8, core::mem::size_of::<u64>()).ok()?;

    Some(value)
}

/// 当前 guest 线程（`ETHREAD` 指针）。
///
/// 内核态用 VMCS 的 `GS_BASE`（指向 KPCR）；用户态用 `IA32_KERNEL_GSBASE`
/// （`swapgs` 之后 KPCR 在它里面）。KPCR 是内核地址，用户 CR3 可能没有映射，
/// 所以统一用 loader 提供的 `system_cr3` 翻译。
///
/// 读不到（KPCR 未映射、偏移不匹配、非 Windows guest）时返回 `None`：调用方
/// 退回“同一 vCPU + 同一栈”的降级匹配。
pub(crate) fn current_guest_thread() -> Option<u64> {
    let kpcr = if guest_is_user().ok()? {
        let mut exception_info = HostExceptionInfo::new();

        safe_routine::rdmsr_safe(&mut exception_info, x86::msr::IA32_KERNEL_GSBASE)?
    } else {
        guest::GsBase::read().ok()?.as_raw()
    };

    // KPCR 必须是内核地址（高半区），否则这个值不可信。
    if kpcr == 0 || ((kpcr >> 47) & 1) == 0 {
        return None;
    }

    let system_cr3 = crate::hv::GLOBAL_ARGS.get().system_cr3 as u64;
    let thread = read_guest_u64(system_cr3, kpcr + KPCR_CURRENT_THREAD_OFFSET)?;

    // 同一个 sanity check：线程对象也在内核高半区。
    if thread == 0 || ((thread >> 47) & 1) == 0 {
        return None;
    }

    Some(thread)
}

pub(crate) fn guest_is_user() -> TerminateResult<bool> {
    Ok((guest::CsSelector::read()?.as_raw() & 3) == 3)
}

pub(crate) fn set_monitor_trap_flag(enabled: bool) -> TerminateResult<()> {
    let mut controls = control::ProcBasedCtls::read()?;

    if controls.monitor_trap_flag() == enabled {
        return Ok(());
    }

    controls.set_monitor_trap_flag(enabled);
    controls.write_adjust()?;

    let readback = control::ProcBasedCtls::read()?;
    if readback.monitor_trap_flag() != enabled {
        return Err(Terminate::internal());
    }

    Ok(())
}
