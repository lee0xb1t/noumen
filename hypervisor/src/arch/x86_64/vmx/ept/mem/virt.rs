//! 客户机虚拟内存（VA）的读写：任意 CR3、按页切片、只查 present。
//!
//! 两个必须写进头注释的约定：
//!
//! 1. **只查 present**：`translate_guest` 固定传 `user = false, write = false`，
//!    不检查 U/S 与 RW，等价于 DMA 的视角（写只读页、读超级页都能工作）。
//! 2. **失败不注入异常**：所有错误都变成 [`MemError`]，由 `MEM_BATCH` 的条目状态
//!    或 `MEM_SCAN` 的 `status` 带回 guest。guest 缓冲区那条路径用的是
//!    `guest_memory::copy_to_guest`，语义不同，不要混用。

use crate::arch::x86_64::{
    guest_memory,
    paging::DIRECT_MAP_SIZE,
    vmx::ept::mem::{error::MemError, phys},
};

/// 4 KiB。跨页由本模块内部切开，调用方不必关心。
pub const PAGE_SIZE: usize = 0x1000;

/// CR3 的物理地址部分（低 12 位是 PCID / 标志）。
pub const CR3_MASK: u64 = 0x000F_FFFF_FFFF_F000;

#[inline]
fn page_offset(address: u64) -> usize {
    (address & (PAGE_SIZE as u64 - 1)) as usize
}

/// 任意 CR3 下的 VA → GPA；只查 present，不动 A/D。
pub fn translate(cr3: u64, va: u64) -> Result<u64, MemError> {
    let cr3 = cr3 & CR3_MASK;

    if cr3 == 0 || cr3 >= DIRECT_MAP_SIZE {
        return Err(MemError::BadCr3);
    }

    guest_memory::translate_guest(cr3, va, false, false, false).map_err(MemError::from)
}

/// 读目标 VA 到本地切片。允许跨页；失败时已写出的前缀保留。
pub fn read(cr3: u64, va: u64, out: &mut [u8]) -> Result<(), MemError> {
    let mut done = 0usize;

    while done < out.len() {
        let current = va
            .checked_add(done as u64)
            .ok_or(MemError::NonCanonical)?;
        let pa = translate(cr3, current)?;
        let offset = page_offset(current);
        let chunk = (out.len() - done).min(PAGE_SIZE - offset);

        phys::read(pa, &mut out[done..done + chunk])?;
        done += chunk;
    }

    Ok(())
}

/// 把本地切片写到目标 VA。允许跨页。
pub fn write(cr3: u64, va: u64, data: &[u8]) -> Result<(), MemError> {
    let mut done = 0usize;

    while done < data.len() {
        let current = va
            .checked_add(done as u64)
            .ok_or(MemError::NonCanonical)?;
        let pa = translate(cr3, current)?;
        let offset = page_offset(current);
        let chunk = (data.len() - done).min(PAGE_SIZE - offset);

        phys::write(pa, &data[done..done + chunk])?;
        done += chunk;
    }

    Ok(())
}

/// 目标 VA → 本地物理地址（批量环的主路径：源是 guest VA，目的是 ring 的 GPA）。
pub fn copy_virt_to_phys(cr3: u64, va: u64, dst_pa: u64, len: usize) -> Result<usize, MemError> {
    let mut done = 0usize;

    while done < len {
        let current = va
            .checked_add(done as u64)
            .ok_or(MemError::NonCanonical)?;
        let src_pa = translate(cr3, current)?;
        let offset = page_offset(current);
        let chunk = (len - done).min(PAGE_SIZE - offset);

        phys::copy(src_pa, dst_pa + done as u64, chunk)?;
        done += chunk;
    }

    Ok(done)
}

/// 本地物理地址 → 目标 VA。
pub fn copy_phys_to_virt(src_pa: u64, cr3: u64, va: u64, len: usize) -> Result<usize, MemError> {
    let mut done = 0usize;

    while done < len {
        let current = va
            .checked_add(done as u64)
            .ok_or(MemError::NonCanonical)?;
        let dst_pa = translate(cr3, current)?;
        let offset = page_offset(current);
        let chunk = (len - done).min(PAGE_SIZE - offset);

        phys::copy(src_pa + done as u64, dst_pa, chunk)?;
        done += chunk;
    }

    Ok(done)
}

/// 两个虚拟地址之间的拷贝（各自的 CR3 可以不同）。
///
/// 批量环用它把「目标进程内存」和「调用者的环」都只翻译一次：
/// 每段同时受两侧页边界限制，因此不会跨越未翻译的页。
pub fn copy_virt_to_virt(
    src_cr3: u64,
    src_va: u64,
    dst_cr3: u64,
    dst_va: u64,
    len: usize,
) -> Result<usize, MemError> {
    let mut done = 0usize;

    while done < len {
        let src_current = src_va
            .checked_add(done as u64)
            .ok_or(MemError::NonCanonical)?;
        let dst_current = dst_va
            .checked_add(done as u64)
            .ok_or(MemError::NonCanonical)?;

        let src_pa = translate(src_cr3, src_current)?;
        let dst_pa = translate(dst_cr3, dst_current)?;

        let chunk = (len - done)
            .min(PAGE_SIZE - page_offset(src_current))
            .min(PAGE_SIZE - page_offset(dst_current));

        phys::copy(src_pa, dst_pa, chunk)?;
        done += chunk;
    }

    Ok(done)
}

/// 读一个 `T`（`T` 必须是任意位模式都有效的类型）。
///
/// # Safety
///
/// 仅用于本模块自己的 `#[repr(C)]` 整数结构体；不用于含引用/枚举的值。
pub fn read_struct<T: Copy>(cr3: u64, va: u64) -> Result<T, MemError> {
    let mut value = core::mem::MaybeUninit::<T>::uninit();
    let bytes = unsafe {
        core::slice::from_raw_parts_mut(
            value.as_mut_ptr() as *mut u8,
            core::mem::size_of::<T>(),
        )
    };

    read(cr3, va, bytes)?;

    Ok(unsafe { value.assume_init() })
}

/// 写一个 `T`；约束同 [`read_struct`]。
pub fn write_struct<T: Copy>(cr3: u64, va: u64, value: &T) -> Result<(), MemError> {
    let bytes = unsafe {
        core::slice::from_raw_parts(value as *const T as *const u8, core::mem::size_of::<T>())
    };

    write(cr3, va, bytes)
}
