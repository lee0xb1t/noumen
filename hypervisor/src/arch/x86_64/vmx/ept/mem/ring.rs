//! 批量环：一次 `MEM_BATCH` 处理 N 条内存操作。
//!
//! 为什么是环而不是单发 hypercall：单条读写也要一次 VM exit，而真实调用方
//! （2 ms tick、每 tick 上千个字段）必须把 exit 摊销掉。环的请求、载荷和结果
//! 全在**调用方自己的用户内存**里，因此：
//!
//! * hypervisor 用**当前 CR3** 把环翻译成 GPA，之后全程 GPA→GPA 的 `memcpy_safe`；
//! * **不需要** MDL、不需要 PFN 数组、不需要会话状态——hypervisor 无状态；
//! * 环是线程局部的：同一时刻只允许一个 VMCALL 在飞。
//!
//! 协议：单生产者 / 单消费者，无锁。

use crate::arch::x86_64::vmx::ept::mem::{error::MemError, virt};

/// 环头的魔数：ASCII `NMEM`。
pub const RING_MAGIC: u32 = 0x4D45_4D4E;
/// 环 ABI 版本。
pub const RING_ABI: u16 = 1;
/// 条目区大小：`(0x1000 - 0x40) / 32 = 126`。
pub const MAX_BATCH_OPS: usize = 126;
/// 载荷区起点。
pub const DATA_OFFSET: usize = 0x1000;
/// 单条载荷上限。
pub const MAX_OP_BYTES: usize = 0x1000;
/// 环字节数范围。
pub const MIN_RING_BYTES: u32 = 64 * 1024;
pub const MAX_RING_BYTES: u32 = 4 * 1024 * 1024;
/// 单批最大载荷字节数（126 × 4 KiB）。
pub const MAX_BATCH_BYTES: u32 = (MAX_BATCH_OPS * MAX_OP_BYTES) as u32;

/// 环条目的操作码。
pub const OP_READ_VIRT: u16 = 0;
pub const OP_WRITE_VIRT: u16 = 1;
pub const OP_READ_PHYS: u16 = 2;
pub const OP_WRITE_PHYS: u16 = 3;
pub const OP_TRANSLATE: u16 = 4;

/// 环头（64 字节）。生产者 = guest，消费者 = hypervisor。
#[repr(C)]
#[derive(Debug, Clone, Copy)]
pub struct MemRingHeader {
    pub magic: u32,
    pub abi: u16,
    pub flags: u16,
    pub capacity: u32,
    pub entry_capacity: u16,
    /// hypervisor 写：已处理到的下标。
    pub head: u16,
    /// guest 写：已提交到的下标。
    pub tail: u16,
    pub reserved0: u16,
    /// guest 每次提交 +1。
    pub seq: u32,
    /// hypervisor 写：结构错误码（1 = 头非法，2 = 下标非法，3 = 环不可访问）。
    pub error: u32,
    pub reserved1: u32,
    pub batches: u64,
    pub ops: u64,
    pub bytes: u64,
    pub root_tsc: u64,
}

const _: () = assert!(core::mem::size_of::<MemRingHeader>() == 64);

/// 环条目（32 字节）。
#[repr(C)]
#[derive(Debug, Clone, Copy, Default)]
pub struct MemOp {
    pub op: u16,
    pub status: u16,
    pub len: u32,
    pub cr3: u64,
    pub addr: u64,
    pub off: u32,
    pub aux: u32,
}

const _: () = assert!(core::mem::size_of::<MemOp>() == 32);

/// 头部错误码。
mod ring_error {
    pub const INVALID_INDICES: u32 = 2;
    pub const UNMAPPED: u32 = 3;
}

/// 执行一批。
///
/// * `ring_cr3`：调用线程的 guest CR3（环与结果都在它的地址空间里）；
/// * `ring_va` / `ring_bytes`：环的位置与长度；
/// * `dry_run`：只校验结构、不执行、不写载荷（`R9` bit0）；
/// * `cpu`：统计用。
///
/// 返回处理条数；结构错误写进 `header.error`。
pub fn execute(ring_cr3: u64, ring_va: u64, ring_bytes: u64, dry_run: bool, cpu: usize) -> u32 {
    let capacity = ring_bytes as u32;

    if ring_va == 0
        || capacity < MIN_RING_BYTES
        || capacity > MAX_RING_BYTES
        || !capacity.is_power_of_two()
        || ring_bytes & (DATA_OFFSET as u64 - 1) != 0
    {
        return 0;
    }

    let Ok(mut header) = virt::read_struct::<MemRingHeader>(ring_cr3, ring_va) else {
        return 0;
    };

    // 头非法时没法安全地写回（环本身可能不可访问），直接返回。
    if header.magic != RING_MAGIC
        || header.abi != RING_ABI
        || header.capacity != capacity
        || header.entry_capacity as usize != MAX_BATCH_OPS
    {
        return 0;
    }

    let head = header.head as usize;
    let tail = header.tail as usize;

    if head > tail || tail > MAX_BATCH_OPS {
        header.error = ring_error::INVALID_INDICES;
        let _ = virt::write_struct(ring_cr3, ring_va, &header);
        return 0;
    }

    if head == tail {
        return 0;
    }

    let start_tsc = unsafe { x86::time::rdtsc() };
    let mut processed = 0u32;
    let mut payload_bytes = 0u64;
    let mut reads = 0u64;
    let mut writes = 0u64;
    let mut faults = 0u64;
    let mut error = 0u32;

    for index in head..tail {
        let entry_va = match ring_va.checked_add(0x40 + (index * 32) as u64) {
            Some(entry_va) => entry_va,
            None => {
                error = ring_error::UNMAPPED;
                break;
            }
        };

        let Ok(op) = virt::read_struct::<MemOp>(ring_cr3, entry_va) else {
            error = ring_error::UNMAPPED;
            break;
        };

        let mut written = op.len;

        let status = match validate_op(&op, capacity as u64) {
            Err(error) => error,
            Ok(()) if dry_run => MemError::Ok,
            Ok(()) => execute_op(ring_cr3, ring_va, &op, &mut written),
        };

        if !dry_run {
            // 只回写 op/status/len 这 8 个字节，载荷已经在数据区里。
            let mut reply = [0u8; 8];
            reply[0..2].copy_from_slice(&op.op.to_le_bytes());
            reply[2..4].copy_from_slice(&status.as_u16().to_le_bytes());
            reply[4..8].copy_from_slice(&written.to_le_bytes());

            if virt::write(ring_cr3, entry_va, &reply).is_err() {
                error = ring_error::UNMAPPED;
                break;
            }
        }

        match status {
            MemError::Ok => {
                if op.op == OP_READ_VIRT || op.op == OP_READ_PHYS {
                    reads += 1;
                } else if op.op == OP_WRITE_VIRT || op.op == OP_WRITE_PHYS {
                    writes += 1;
                }

                payload_bytes += written as u64;
            }
            _ => faults += 1,
        }

        processed += 1;
    }

    if !dry_run {
        header.head = tail as u16;
        header.error = error;
        header.batches = header.batches.wrapping_add(1);
        header.ops = header.ops.wrapping_add(processed as u64);
        header.bytes = header.bytes.wrapping_add(payload_bytes);
        header.root_tsc = header
            .root_tsc
            .wrapping_add(unsafe { x86::time::rdtsc() }.wrapping_sub(start_tsc));

        let _ = virt::write_struct(ring_cr3, ring_va, &header);
    }

    super::stats::record(cpu, reads, writes, faults);

    processed
}

/// 结构校验；失败只影响这一条。
fn validate_op(op: &MemOp, capacity: u64) -> Result<(), MemError> {
    if op.aux != 0 {
        return Err(MemError::BadEntry);
    }

    if op.op > OP_TRANSLATE {
        return Err(MemError::BadEntry);
    }

    if op.op == OP_TRANSLATE {
        if op.len != 0 {
            return Err(MemError::TooLarge);
        }

        if (op.off as u64) < DATA_OFFSET as u64
            || (op.off as u64).saturating_add(8) > capacity
        {
            return Err(MemError::BadEntry);
        }

        return Ok(());
    }

    if op.len as usize > MAX_OP_BYTES {
        return Err(MemError::TooLarge);
    }

    if (op.off as u64) < DATA_OFFSET as u64 {
        return Err(MemError::BadEntry);
    }

    let end = (op.off as u64)
        .checked_add(op.len as u64)
        .ok_or(MemError::BadEntry)?;

    if end > capacity {
        return Err(MemError::BadEntry);
    }

    Ok(())
}

/// 执行单条；`written` 回写实际字节数。
fn execute_op(ring_cr3: u64, ring_va: u64, op: &MemOp, written: &mut u32) -> MemError {
    let payload_va = match ring_va.checked_add(op.off as u64) {
        Some(payload_va) => payload_va,
        None => return MemError::BadEntry,
    };

    let len = op.len as usize;

    let result = match op.op {
        OP_READ_VIRT => virt::copy_virt_to_virt(op.cr3, op.addr, ring_cr3, payload_va, len)
            .map(|_| ()),
        OP_WRITE_VIRT => {
            virt::copy_virt_to_virt(ring_cr3, payload_va, op.cr3, op.addr, len).map(|_| ())
        }
        OP_READ_PHYS => virt::copy_phys_to_virt(op.addr, ring_cr3, payload_va, len).map(|_| ()),
        OP_WRITE_PHYS => {
            virt::copy_virt_to_phys(ring_cr3, payload_va, op.addr, len).map(|_| ())
        }
        OP_TRANSLATE => match virt::translate(op.cr3, op.addr) {
            Ok(gpa) => {
                *written = 8;
                virt::write(ring_cr3, payload_va, &gpa.to_le_bytes())
            }
            Err(error) => Err(error),
        },
        _ => Err(MemError::BadEntry),
    };

    match result {
        Ok(()) => MemError::Ok,
        Err(error) => {
            *written = 0;
            error
        }
    }
}

