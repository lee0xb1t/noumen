//! CPL3 直接执行 `VMCALL`。nsvr 不经过驱动，所以这是唯一的调用通道。
//!
//! ```text
//! RAX = key   RCX = command   RDX/R8/R9 = arg0..2   RAX = 返回值
//! ```

/// 0x200：设置 TSC 隐藏目标进程（`RDX` = PID，`R8` = 模式 0/1/2）。
pub const TSC_SET_TARGET_PID: u64 = 0x200;
/// 0x204：每次命中退出的补偿 tick。
pub const TSC_SET_EXIT_OVERHEAD: u64 = 0x204;
/// 0x205：可见时钟除数。
pub const TSC_SET_DIVISOR: u64 = 0x205;
/// 0x206：命中日志开关。
pub const TSC_SET_HIT_LOG: u64 = 0x206;
/// 0x20：`MEM_QUERY`。
pub const MEM_QUERY: u64 = 0x20;
/// 0x21：`MEM_BATCH`。
pub const MEM_BATCH: u64 = 0x21;

pub const RING_MAGIC: u32 = 0x4D45_4D4E;
pub const RING_ABI: u16 = 1;
pub const HEADER_SIZE: usize = 64;
pub const ENTRY_OFFSET: usize = 0x40;
pub const ENTRY_SIZE: usize = 32;
pub const DATA_OFFSET: usize = 0x1000;
pub const MAX_OP_BYTES: usize = 0x1000;
pub const MIN_RING_BYTES: usize = 64 * 1024;

pub const OP_READ_VIRT: u16 = 0;
pub const OP_WRITE_VIRT: u16 = 1;

#[repr(C)]
#[derive(Default)]
struct RingHeader {
    magic: u32,
    abi: u16,
    flags: u16,
    capacity: u32,
    entry_capacity: u16,
    head: u16,
    tail: u16,
    reserved0: u16,
    seq: u32,
    error: u32,
    reserved1: u32,
    batches: u64,
    ops: u64,
    bytes: u64,
    root_tsc: u64,
}

#[repr(C)]
#[derive(Default)]
struct RingOp {
    op: u16,
    status: u16,
    len: u32,
    cr3: u64,
    addr: u64,
    off: u32,
    aux: u32,
}

const _: () = assert!(core::mem::size_of::<RingHeader>() == HEADER_SIZE);
const _: () = assert!(core::mem::size_of::<RingOp>() == ENTRY_SIZE);

/// 以 CPL3 身份执行 `vmcall`。
///
/// # Safety
/// 只在 hypervisor 已接管当前 CPU 且 key 正确时调用；key 错会注入 `#UD`。
pub unsafe fn vmcall(key: u64, command: u64, arg0: u64, arg1: u64, arg2: u64) -> u64 {
    let result: u64;

    unsafe {
        core::arch::asm!(
            "vmcall",
            inlateout("rax") key => result,
            in("rcx") command,
            in("rdx") arg0,
            in("r8") arg1,
            in("r9") arg2,
            options(nostack),
        );
    }

    result
}

/// `MEM_QUERY`：只报告 `RAX`（ABI 版本）。
pub fn mem_query(key: u64) -> u64 {
    unsafe { vmcall(key, MEM_QUERY, 0, 0, 0) }
}

/// 单条 `MEM_BATCH`：环里只放一个操作，载荷在 `DATA_OFFSET`。
///
/// 返回 `(RAX, 条目 status)`，`status == 0` 为成功。
pub fn mem_single(key: u64, op: u16, addr: u64, len: u32, payload: Option<&[u8]>) -> (u64, u16) {
    let bytes = MIN_RING_BYTES.max(DATA_OFFSET + MAX_OP_BYTES);
    let mut ring = vec![0u8; bytes];

    unsafe {
        let header = &mut *(ring.as_mut_ptr() as *mut RingHeader);

        header.magic = RING_MAGIC;
        header.abi = RING_ABI;
        header.capacity = bytes as u32;
        header.entry_capacity = 1;
        header.tail = 1;

        let entry = &mut *(ring.as_mut_ptr().add(ENTRY_OFFSET) as *mut RingOp);

        entry.op = op;
        entry.len = len;
        entry.addr = addr;

        if let Some(data) = payload {
            let copy = data.len().min(len as usize).min(MAX_OP_BYTES);

            ring[DATA_OFFSET..DATA_OFFSET + copy].copy_from_slice(&data[..copy]);
        }
    }

    let rax = unsafe { vmcall(key, MEM_BATCH, ring.as_ptr() as u64, bytes as u64, 0) };
    let status = unsafe { (*(ring.as_ptr().add(ENTRY_OFFSET) as *const RingOp)).status };

    (rax, status)
}
