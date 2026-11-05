use super::{
    error::MemError,
    phys::{read_gpa, write_gpa},
    stats::record,
    virt::{read_bytes, read_struct, read_virt, translate, write_bytes, write_struct, write_virt},
};

pub const RING_MAGIC: u32 = 0x4d45_4d4e;
pub const RING_ABI: u16 = 1;
pub const MAX_BATCH_OPS: usize = 126;
pub const DATA_OFFSET: usize = 0x1000;
pub const MAX_OP_BYTES: usize = 0x1000;
pub const MIN_RING_BYTES: u32 = 64 * 1024;
pub const MAX_RING_BYTES: u32 = 4 * 1024 * 1024;
pub const MAX_BATCH_BYTES: u32 = (MAX_BATCH_OPS * MAX_OP_BYTES) as u32;

pub const OP_READ_VIRT: u16 = 0;
pub const OP_WRITE_VIRT: u16 = 1;
pub const OP_READ_PHYS: u16 = 2;
pub const OP_WRITE_PHYS: u16 = 3;
pub const OP_TRANSLATE: u16 = 4;

#[repr(C)]
#[derive(Clone, Copy, Debug)]
pub struct MemRingHeader {
    pub magic: u32,
    pub abi: u16,
    pub flags: u16,
    pub capacity: u32,
    pub entry_capacity: u16,
    pub head: u16,
    pub tail: u16,
    pub reserved0: u16,
    pub seq: u32,
    pub error: u32,
    pub reserved1: u32,
    pub batches: u64,
    pub ops: u64,
    pub bytes: u64,
    pub root_tsc: u64,
}

const _: () = assert!(core::mem::size_of::<MemRingHeader>() == 64);

#[repr(C)]
#[derive(Clone, Copy, Debug, Default)]
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


const HEADER_INDICES: u32 = 2;
const HEADER_UNMAPPED: u32 = 3;


pub fn max_batch_bytes() -> u32 {
    MAX_BATCH_BYTES
}

/// Process one `MEM_BATCH` ring.
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

    let Ok(mut header) = read_struct::<MemRingHeader>(ring_cr3, ring_va) else {
        return 0;
    };

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
        header.error = HEADER_INDICES;
        let _ = write_struct(ring_cr3, ring_va, &header);
        return 0;
    }

    if head == tail {
        return 0;
    }

    let mut processed = 0u32;
    let mut payload_bytes = 0u64;
    let mut reads = 0u64;
    let mut writes = 0u64;
    let mut faults = 0u64;
    let mut error = 0u32;

    for index in head..tail {
        let Some(entry_va) = ring_va.checked_add(0x40 + (index * 32) as u64) else {
            error = HEADER_UNMAPPED;
            break;
        };

        let Ok(op) = read_struct::<MemOp>(ring_cr3, entry_va) else {
            error = HEADER_UNMAPPED;
            break;
        };

        let mut written = op.len;
        let status = match validate_op(&op, capacity as u64) {
            Err(error) => error,
            Ok(()) if dry_run => MemError::Ok,
            Ok(()) => execute_op(ring_cr3, ring_va, &op, &mut written),
        };

        if !dry_run {
            let mut reply = [0u8; 8];
            reply[0..2].copy_from_slice(&op.op.to_le_bytes());
            reply[2..4].copy_from_slice(&status.as_u16().to_le_bytes());
            reply[4..8].copy_from_slice(&written.to_le_bytes());

            if write_bytes(ring_cr3, entry_va, &reply).is_err() {
                error = HEADER_UNMAPPED;
                break;
            }
        }

        if status == MemError::Ok {
            payload_bytes += written as u64;

            if op.op == OP_READ_VIRT || op.op == OP_READ_PHYS {
                reads += 1;
            } else if op.op == OP_WRITE_VIRT || op.op == OP_WRITE_PHYS {
                writes += 1;
            }
        } else {
            faults += 1;
        }

        processed += 1;
    }

    if !dry_run {
        header.head = tail as u16;
        header.error = error;
        header.batches = header.batches.wrapping_add(1);
        header.ops = header.ops.wrapping_add(processed as u64);
        header.bytes = header.bytes.wrapping_add(payload_bytes);

        let _ = write_struct(ring_cr3, ring_va, &header);
    }

    record(cpu, reads, writes, faults);

    processed
}


fn validate_op(op: &MemOp, capacity: u64) -> Result<(), MemError> {
    if op.aux != 0 || op.op > OP_TRANSLATE {
        return Err(MemError::BadEntry);
    }

    if op.op == OP_TRANSLATE {
        if op.len != 0 {
            return Err(MemError::TooLarge);
        }

        if (op.off as u64) < DATA_OFFSET as u64 || (op.off as u64).saturating_add(8) > capacity {
            return Err(MemError::BadEntry);
        }

        return Ok(());
    }

    if op.len as usize > MAX_OP_BYTES || (op.off as u64) < DATA_OFFSET as u64 {
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

fn execute_op(ring_cr3: u64, ring_va: u64, op: &MemOp, written: &mut u32) -> MemError {
    let Some(payload_va) = ring_va.checked_add(op.off as u64) else {
        return MemError::BadEntry;
    };

    let len = op.len as usize;
    let mut buffer = [0u8; MAX_OP_BYTES];

    let result = match op.op {
        OP_READ_VIRT => read_virt(op.cr3, op.addr, &mut buffer[..len])
            .and_then(|()| write_bytes(ring_cr3, payload_va, &buffer[..len])),
        OP_WRITE_VIRT => read_bytes(ring_cr3, payload_va, &mut buffer[..len])
            .and_then(|()| write_virt(op.cr3, op.addr, &buffer[..len])),
        OP_READ_PHYS => read_gpa(op.addr, &mut buffer[..len])
            .and_then(|()| write_bytes(ring_cr3, payload_va, &buffer[..len])),
        OP_WRITE_PHYS => read_bytes(ring_cr3, payload_va, &mut buffer[..len])
            .and_then(|()| write_gpa(op.addr, &buffer[..len])),
        OP_TRANSLATE => match translate(op.cr3, op.addr) {
            Ok(gpa) => {
                *written = 8;
                write_bytes(ring_cr3, payload_va, &gpa.to_le_bytes())
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

