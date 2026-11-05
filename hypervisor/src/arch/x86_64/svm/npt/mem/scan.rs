//! Segmented memory scan (`MEM_SCAN`) for the SVM backend.

use super::pattern::{Pattern, MAX_PATTERN};

use crate::{
    arch::x86_64::{
        paging::DIRECT_MAP_SIZE,
        svm::mem::{self, MemError},
        sync,
        tsc::tsc_hz,
    },
};

pub const SCAN_DEFAULT_BYTES: u32 = 256 * 1024;
pub const SCAN_MAX_BYTES: u32 = 4 * 1024 * 1024;
pub const SCAN_DEFAULT_US: u32 = 200;
pub const SCAN_MAX_US: u32 = 2000;

pub const SPACE_VIRT: u8 = 0;
pub const SPACE_PHYS: u8 = 1;

pub const SCAN_FLAG_STOP_AFTER_FIRST: u8 = 1 << 0;
pub const SCAN_FLAG_SKIP_INACCESSIBLE: u8 = 1 << 1;
pub const SCAN_FLAG_MATCH_ALIGNED: u8 = 1 << 2;

const PAGE_SIZE: usize = 0x1000;

/// Scan request (200 bytes), mirrored by 调用方.
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
    pub cursor: u64,
    pub end: u64,
    pub cr3: u64,
    pub out_va: u64,
    pub max_bytes: u32,
    pub budget_us: u32,
    pub out_cap: u32,
    pub scanned: u32,
    pub tsc: u64,
    pub pattern: [u8; MAX_PATTERN],
    pub mask: [u8; MAX_PATTERN],
}

const _: () = assert!(core::mem::size_of::<MemScanRequest>() == 200);

static SCAN_BUF: sync::Mutex<[u8; PAGE_SIZE]> = sync::Mutex::new([0; PAGE_SIZE]);

/// Execute one segment. `out_cr3` is the caller's current CR3 (match addresses
/// are written through it); `req.cr3` is the scanned address space.
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

    let bytes = &req.pattern[..pattern_len];
    let mask = &req.mask[..pattern_len];
    let pattern = match Pattern::new(bytes, mask) {
        Some(pattern) => pattern,
        None => {
            req.status = MemError::BadEntry.as_u16();
            return;
        }
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
    let budget_ticks = (budget_us as u64).saturating_mul(tsc_hz()) / 1_000_000;

    let Some(mut guard) = SCAN_BUF.try_lock() else {
        req.status = MemError::Busy.as_u16();
        return;
    };

    let buffer = &mut *guard;
    let start_tsc = unsafe { core::arch::x86_64::_rdtsc() };

    let mut cursor = req.cursor;
    let mut scanned = 0usize;
    let mut matches = 0u16;
    let mut status = MemError::Ok;
    let mut done = false;

    let mut prev_tail = [0u8; MAX_PATTERN];
    let mut prev_tail_len = 0usize;
    let mut prev_end = 0u64;

    while scanned < max_bytes {
        if cursor >= req.end {
            done = true;
            break;
        }

        if req.space == SPACE_PHYS {
            if cursor >= DIRECT_MAP_SIZE {
                done = true;
                break;
            }
        }

        let mut chunk = ((req.end - cursor) as usize).min(PAGE_SIZE);

        if req.space == SPACE_PHYS {
            chunk = chunk.min((DIRECT_MAP_SIZE - cursor) as usize);
        }

        if chunk == 0 {
            done = true;
            break;
        }

        let read_result = if req.space == SPACE_VIRT {
            mem::read_bytes(req.cr3, cursor, &mut buffer[..chunk])
        } else {
            mem::read_gpa(cursor, &mut buffer[..chunk])
        };

        if let Err(error) = read_result {
            if req.space == SPACE_VIRT
                && error == MemError::NotPresent
                && req.flags & SCAN_FLAG_SKIP_INACCESSIBLE != 0
            {
                cursor = (cursor & !(PAGE_SIZE as u64 - 1)).saturating_add(PAGE_SIZE as u64);
                prev_tail_len = 0;
                continue;
            }

            status = error;
            break;
        }

        // 1) Match windows that start in the previous chunk.
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

                    if accept(req, address, pattern_len) {
                        match emit(req, out_cr3, address, &mut matches) {
                            Ok(true) => {}
                            Ok(false) => break,
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

                index += 1;
            }

            if status != MemError::Ok || done {
                break;
            }

            if matches >= req.max_matches || matches as u32 >= req.out_cap {
                break;
            }
        }

        // 2) Match windows fully inside this chunk.
        if chunk >= pattern_len {
            let mut offset = 0usize;

            while offset + pattern_len <= chunk {
                if pattern.matches_at(&buffer[offset..]) {
                    let address = cursor + offset as u64;

                    if accept(req, address, pattern_len) {
                        match emit(req, out_cr3, address, &mut matches) {
                            Ok(true) => {}
                            Ok(false) => break,
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

        if unsafe { core::arch::x86_64::_rdtsc() }.wrapping_sub(start_tsc) >= budget_ticks {
            break;
        }
    }

    req.cursor = cursor;
    req.scanned = scanned as u32;
    req.matches = matches;
    req.done = done as u8;
    req.status = status.as_u16();
    req.tsc = unsafe { core::arch::x86_64::_rdtsc() }.wrapping_sub(start_tsc);
}

/// Read a request from the guest, scan, and write it back. Returns the status.
pub fn scan_guest(request_va: u64, size: u64, out_cr3: u64, cpu: usize) -> u64 {
    if request_va == 0 || size < core::mem::size_of::<MemScanRequest>() as u64 {
        return MemError::BadEntry.as_u64();
    }

    let mut request = match mem::read_struct::<MemScanRequest>(out_cr3, request_va) {
        Ok(request) => request,
        Err(error) => return error.as_u64(),
    };

    let started = unsafe { core::arch::x86_64::_rdtsc() };

    scan(&mut request, out_cr3);

    mem::record_scan(cpu, request.scanned as u64, unsafe {
        core::arch::x86_64::_rdtsc()
    } - started);

    match mem::write_struct(out_cr3, request_va, &request) {
        Ok(()) => request.status as u64,
        Err(error) => error.as_u64(),
    }
}

fn accept(req: &MemScanRequest, address: u64, pattern_len: usize) -> bool {
    if req.flags & SCAN_FLAG_MATCH_ALIGNED != 0 && address & 0xf != 0 {
        return false;
    }

    address
        .checked_add(pattern_len as u64)
        .is_some_and(|end| end <= req.end)
}

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

    mem::write_bytes(out_cr3, slot, &address.to_le_bytes())?;
    *matches += 1;

    Ok(true)
}
