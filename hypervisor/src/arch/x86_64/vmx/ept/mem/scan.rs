//! 分段扫描：调用方给预算，本函数干完一段就返回，游标保存在请求结构体里。
//!
//! 这是唯一会长时间占用 CPU 的操作，所以接口**天生分段**：
//!
//! * 每次最多 `max_bytes`（默认 256 KiB，硬上限 4 MiB）；
//! * 每次最多 `budget_us`（默认 200 µs，硬上限 2 ms，按 `tsc::tsc_hz()` 换算）；
//! * **保证前进**：至少处理一页，或者跳过一个不可访问页；不会原地打转；
//! * 跨块匹配：保留 `pattern_len - 1` 字节尾部，与下一块头部拼起来再比。
//!
//! 工作缓冲是**全局共享**的一块 4 KiB：宿主栈只有 16 KiB，页级缓冲不能放栈上。
//! 并发扫描拿到 [`MemError::Busy`] 而不是自旋。

use crate::arch::x86_64::{
    sync, tsc,
    vmx::{
        ept::mem::{
            error::MemError,
            pattern::{MemPattern, MAX_PATTERN},
            phys, virt,
        },
    },
};

/// 默认/上限字节预算。
pub const SCAN_DEFAULT_BYTES: u32 = 256 * 1024;
pub const SCAN_MAX_BYTES: u32 = 4 * 1024 * 1024;
/// 默认/上限时间预算（微秒）。
pub const SCAN_DEFAULT_US: u32 = 200;
pub const SCAN_MAX_US: u32 = 2000;

/// 地址空间。
pub const SPACE_VIRT: u8 = 0;
pub const SPACE_PHYS: u8 = 1;

/// 找到第一个就停（`done` 置 1）。
pub const SCAN_FLAG_STOP_AFTER_FIRST: u8 = 1 << 0;
/// 虚拟空间里遇到 `NotPresent` 就跳到下一页，而不是整次失败。
pub const SCAN_FLAG_SKIP_INACCESSIBLE: u8 = 1 << 1;
/// 只接受 16 字节对齐的匹配地址。
pub const SCAN_FLAG_MATCH_ALIGNED: u8 = 1 << 2;

/// 一次扫描请求。**同一块内存既入参又出参**，游标由调用方持有。
///
/// 布局固定 200 字节（调用方 侧镜像同一份），改字段必须同步 `mem_layout.rs`。
#[repr(C)]
#[derive(Clone, Copy)]
pub struct MemScanRequest {
    pub abi: u16,
    pub space: u8,
    pub flags: u8,
    pub status: u16,
    pub pattern_len: u16,
    pub max_matches: u16,
    pub matches: u16,
    pub done: u8,
    pub reserved0: [u8; 3],
    /// 下一次从哪继续；返回时是下一页的起点。
    pub cursor: u64,
    /// 扫描上界（不含）。
    pub end: u64,
    /// `space = virt` 时的目标 CR3。
    pub cr3: u64,
    /// 匹配地址写到哪（用**当前** guest CR3，u64 数组）。
    pub out_va: u64,
    pub max_bytes: u32,
    pub budget_us: u32,
    pub out_cap: u32,
    pub scanned: u32,
    pub tsc: u64,
    pub pattern: [u8; MAX_PATTERN],
    /// `1` = 必须相等，`0` = 通配。
    pub mask: [u8; MAX_PATTERN],
}

const _: () = assert!(core::mem::size_of::<MemScanRequest>() == 200);

impl Default for MemScanRequest {
    fn default() -> Self {
        Self {
            abi: super::ABI_VERSION,
            space: SPACE_VIRT,
            flags: 0,
            status: 0,
            pattern_len: 0,
            max_matches: 0,
            matches: 0,
            done: 0,
            reserved0: [0; 3],
            cursor: 0,
            end: 0,
            cr3: 0,
            out_va: 0,
            max_bytes: 0,
            budget_us: 0,
            out_cap: 0,
            scanned: 0,
            tsc: 0,
            pattern: [0; MAX_PATTERN],
            mask: [0; MAX_PATTERN],
        }
    }
}

const PAGE_SIZE: usize = 0x1000;

static SCAN_BUF: sync::Mutex<[u8; PAGE_SIZE]> = sync::Mutex::new([0; PAGE_SIZE]);

/// 执行一段。
///
/// `out_cr3` 是**当前** guest CR3（匹配地址写进 `out_va` 用它）；`req.cr3` 是
/// 被扫描地址空间的 CR3。两者在多进程扫描时不是一回事。
pub fn scan(req: &mut MemScanRequest, out_cr3: u64) {
    req.status = MemError::Ok.as_u16();
    req.matches = 0;
    req.scanned = 0;
    req.tsc = 0;
    req.done = 0;

    let pattern_len = req.pattern_len as usize;

    if pattern_len == 0 || pattern_len > MAX_PATTERN || req.space > SPACE_PHYS {
        req.status = MemError::BadEntry.as_u16();
        return;
    }

    let Some(pattern) = MemPattern::from_raw(
        &req.pattern[..pattern_len],
        &req.mask[..pattern_len],
    ) else {
        req.status = MemError::BadEntry.as_u16();
        return;
    };

    if req.end <= req.cursor {
        req.done = 1;
        return;
    }

    if req.out_cap == 0 || req.max_matches == 0 {
        req.status = MemError::BadEntry.as_u16();
        return;
    }

    let max_bytes = if req.max_bytes == 0 {
        SCAN_DEFAULT_BYTES
    } else {
        req.max_bytes.min(SCAN_MAX_BYTES)
    } as usize;

    let budget_us = if req.budget_us == 0 {
        SCAN_DEFAULT_US
    } else {
        req.budget_us.min(SCAN_MAX_US)
    };
    let budget_ticks = (budget_us as u64).saturating_mul(tsc::tsc_hz()) / 1_000_000;

    let Some(mut guard) = SCAN_BUF.try_lock() else {
        req.status = MemError::Busy.as_u16();
        return;
    };

    let buffer = &mut *guard;
    let start_tsc = unsafe { x86::time::rdtsc() };

    let mut cursor = req.cursor;
    let mut scanned = 0usize;
    let mut matches = 0u16;
    let mut status = MemError::Ok;
    let mut done = false;

    // 跨块匹配：上一块的尾部（最多 pattern_len - 1 字节）与它结束的地址。
    let mut prev_tail = [0u8; MAX_PATTERN];
    let mut prev_tail_len = 0usize;
    let mut prev_end = 0u64;

    while scanned < max_bytes {
        if cursor >= req.end {
            done = true;
            break;
        }

        // 物理空间：跳过 RAM 区间之间的空洞。
        if req.space == SPACE_PHYS {
            let Some((start, end)) = phys::next_range(cursor) else {
                done = true;
                break;
            };

            if start > cursor {
                cursor = start;
                prev_tail_len = 0;

                if cursor >= req.end {
                    done = true;
                    break;
                }
            }

            if end <= cursor {
                done = true;
                break;
            }
        }

        let mut chunk = ((req.end - cursor) as usize).min(PAGE_SIZE);

        if req.space == SPACE_PHYS {
            if let Some((_, end)) = phys::next_range(cursor) {
                chunk = chunk.min((end - cursor) as usize);
            }
        }

        if chunk == 0 {
            done = true;
            break;
        }

        let read_result = if req.space == SPACE_VIRT {
            virt::read(req.cr3, cursor, &mut buffer[..chunk])
        } else {
            phys::read(cursor, &mut buffer[..chunk])
        };

        if let Err(error) = read_result {
            if req.space == SPACE_VIRT
                && error == MemError::NotPresent
                && req.flags & SCAN_FLAG_SKIP_INACCESSIBLE != 0
            {
                // 跳到下一页；保证前进，且不需要计数。
                cursor = (cursor & !(PAGE_SIZE as u64 - 1)).saturating_add(PAGE_SIZE as u64);
                prev_tail_len = 0;
                continue;
            }

            status = error;
            break;
        }

        // 1) 跨块匹配：起点在上一块里、结束在本块里。
        if prev_tail_len > 0 && prev_end == cursor {
            let tail = prev_tail_len.min(pattern_len - 1);
            let head = (pattern_len - 1).min(chunk);
            let mut window = [0u8; MAX_PATTERN * 2];

            window[..tail].copy_from_slice(&prev_tail[..tail]);
            window[tail..tail + head].copy_from_slice(&buffer[..head]);

            let window_len = tail + head;
            let mut index = 0usize;

            while index < tail {
                if index + pattern_len <= window_len && pattern.matches_at(&window[index..]) {
                    let address = prev_end - tail as u64 + index as u64;

                    if !accept(req, address, pattern_len) {
                        index += 1;
                        continue;
                    }

                    match emit(req, out_cr3, address, &mut matches) {
                        Ok(true) => {}
                        Ok(false) => {
                            break;
                        }
                        Err(error) => {
                            status = error;
                            break;
                        }
                    }

                    if req.flags & SCAN_FLAG_STOP_AFTER_FIRST != 0 {
                        done = true;
                        break;
                    }
                }

                index += 1;
            }

            if status != MemError::Ok || done {
                break;
            }

            if matches >= req.max_matches || matches as u32 >= req.out_cap {
                break;
            }
        }

        // 2) 块内匹配。
        if chunk >= pattern_len {
            let mut offset = 0usize;

            while offset + pattern_len <= chunk {
                if pattern.matches_at(&buffer[offset..]) {
                    let address = cursor + offset as u64;

                    if accept(req, address, pattern_len) {
                        match emit(req, out_cr3, address, &mut matches) {
                            Ok(true) => {}
                            Ok(false) => {
                                break;
                            }
                            Err(error) => {
                                status = error;
                                break;
                            }
                        }

                        if req.flags & SCAN_FLAG_STOP_AFTER_FIRST != 0 {
                            done = true;
                            break;
                        }
                    }
                }

                offset += 1;
            }

            if status != MemError::Ok || done {
                break;
            }

            if matches >= req.max_matches || matches as u32 >= req.out_cap {
                break;
            }
        }

        // 3) 保存尾部，前进。
        let keep = (pattern_len - 1).min(chunk);
        prev_tail[..keep].copy_from_slice(&buffer[chunk - keep..chunk]);
        prev_tail_len = keep;
        prev_end = cursor + chunk as u64;

        cursor += chunk as u64;
        scanned += chunk;

        if cursor >= req.end {
            done = true;
            break;
        }

        if unsafe { x86::time::rdtsc() }.wrapping_sub(start_tsc) >= budget_ticks {
            break;
        }
    }

    req.cursor = cursor;
    req.scanned = scanned as u32;
    req.matches = matches;
    req.done = done as u8;
    req.status = status.as_u16();
    req.tsc = unsafe { x86::time::rdtsc() }.wrapping_sub(start_tsc);
}

/// 匹配地址是否满足“对齐 + 在范围内”的要求。
#[inline]
fn accept(req: &MemScanRequest, address: u64, pattern_len: usize) -> bool {
    if req.flags & SCAN_FLAG_MATCH_ALIGNED != 0 && address & 0xF != 0 {
        return false;
    }

    address
        .checked_add(pattern_len as u64)
        .is_some_and(|end| end <= req.end)
}

/// 记录一个匹配。返回 `Ok(false)` 表示输出缓冲已满（调用方应停止本段）。
fn emit(
    req: &MemScanRequest,
    out_cr3: u64,
    address: u64,
    matches: &mut u16,
) -> Result<bool, MemError> {
    if *matches >= req.max_matches || *matches as u32 >= req.out_cap {
        return Ok(false);
    }

    let slot = req
        .out_va
        .checked_add((*matches as u64) * 8)
        .ok_or(MemError::BadEntry)?;

    virt::write(out_cr3, slot, &address.to_le_bytes())?;
    *matches += 1;

    Ok(true)
}
