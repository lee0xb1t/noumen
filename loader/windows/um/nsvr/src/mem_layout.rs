//! mem 的 guest 侧 ABI 镜像：环、条目、扫描请求、错误码。
//!
//! 与 hypervisor 的 `src/arch/x86_64/vmx/ept/mem/*` 逐字段对应；任何布局改动都
//! 必须两边同时改，并升 `ABI_VERSION`。所有结构体都有大小断言。

use std::fmt;

use crate::pattern::Pattern;

/// 与 hypervisor `mem::ABI_VERSION` 一致。
pub const ABI_VERSION: u16 = 1;
/// 环头魔数 `NMEM`。
pub const RING_MAGIC: u32 = 0x4D45_4D4E;
pub const RING_ABI: u16 = 1;

pub const HEADER_SIZE: usize = 64;
pub const ENTRY_OFFSET: usize = 0x40;
pub const ENTRY_SIZE: usize = 32;
pub const MAX_BATCH_OPS: usize = 126;
pub const DATA_OFFSET: usize = 0x1000;
pub const MAX_OP_BYTES: usize = 0x1000;
pub const MIN_RING_BYTES: usize = 64 * 1024;
pub const MAX_RING_BYTES: usize = 4 * 1024 * 1024;
/// 单次扫描最多写回的匹配数（与 `MemScanRequest` 的调用约定一致）。
pub const SCAN_MAX_MATCHES: usize = 64;
pub const PATTERN_MAX: usize = 64;

pub const OP_READ_VIRT: u16 = 0;
pub const OP_WRITE_VIRT: u16 = 1;
pub const OP_READ_PHYS: u16 = 2;
pub const OP_WRITE_PHYS: u16 = 3;
pub const OP_TRANSLATE: u16 = 4;

pub const SPACE_VIRT: u8 = 0;
pub const SPACE_PHYS: u8 = 1;

pub const SCAN_FLAG_STOP_AFTER_FIRST: u8 = 1 << 0;
pub const SCAN_FLAG_SKIP_INACCESSIBLE: u8 = 1 << 1;
pub const SCAN_FLAG_MATCH_ALIGNED: u8 = 1 << 2;

/// `MemError` 的镜像。编号即 ABI。
#[repr(u16)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MemError {
    Ok = 0,
    NotPresent = 1,
    NonCanonical = 2,
    OutOfMap = 3,
    BadCr3 = 4,
    NotRam = 5,
    TooLarge = 6,
    BadEntry = 7,
    Busy = 8,
    HostFault = 9,
    Unsupported = 10,
    Hooked = 11,
    Unaligned = 12,
}

impl MemError {
    pub const fn from_u16(value: u16) -> Self {
        match value {
            0 => Self::Ok,
            1 => Self::NotPresent,
            2 => Self::NonCanonical,
            3 => Self::OutOfMap,
            4 => Self::BadCr3,
            5 => Self::NotRam,
            6 => Self::TooLarge,
            7 => Self::BadEntry,
            8 => Self::Busy,
            9 => Self::HostFault,
            10 => Self::Unsupported,
            11 => Self::Hooked,
            12 => Self::Unaligned,
            _ => Self::BadEntry,
        }
    }

    pub const fn is_ok(self) -> bool {
        matches!(self, Self::Ok)
    }

    /// 中文说明，UI 与日志共用。
    pub const fn text(self) -> &'static str {
        match self {
            Self::Ok => "成功",
            Self::NotPresent => "页不存在（换出/未提交/CR3 不对）",
            Self::NonCanonical => "地址不是 48 位规范形式",
            Self::OutOfMap => "物理地址超出直接映射",
            Self::BadCr3 => "CR3 非法",
            Self::NotRam => "物理地址不在 RAM 区间",
            Self::TooLarge => "单条载荷超限",
            Self::BadEntry => "请求结构非法",
            Self::Busy => "扫描缓冲被占用",
            Self::HostFault => "宿主访问 fault",
            Self::Unsupported => "内存服务不可用",
            Self::Hooked => "目标页被 EPT hook 占用",
            Self::Unaligned => "地址未对齐",
        }
    }
}

impl fmt::Display for MemError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(formatter, "{} ({})", self.text(), *self as u16)
    }
}

/// 环头（64 字节）。
#[repr(C)]
#[derive(Debug, Clone, Copy, Default)]
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

const _: () = assert!(std::mem::size_of::<MemRingHeader>() == 64);

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

const _: () = assert!(std::mem::size_of::<MemOp>() == 32);

/// 分段扫描请求（200 字节）。
#[repr(C)]
#[derive(Debug, Clone, Copy)]
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
    pub pattern: [u8; PATTERN_MAX],
    pub mask: [u8; PATTERN_MAX],
}

const _: () = assert!(std::mem::size_of::<MemScanRequest>() == 200);

impl Default for MemScanRequest {
    fn default() -> Self {
        Self {
            abi: ABI_VERSION,
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
            pattern: [0; PATTERN_MAX],
            mask: [0; PATTERN_MAX],
        }
    }
}

impl MemScanRequest {
    /// 虚拟空间扫描。
    pub fn virt(cr3: u64, start: u64, end: u64, pattern: &Pattern) -> Result<Self, String> {
        Self::new(SPACE_VIRT, cr3, start, end, pattern)
    }

    /// 物理空间扫描。
    pub fn phys(start: u64, end: u64, pattern: &Pattern) -> Result<Self, String> {
        Self::new(SPACE_PHYS, 0, start, end, pattern)
    }

    fn new(space: u8, cr3: u64, start: u64, end: u64, pattern: &Pattern) -> Result<Self, String> {
        if pattern.len() == 0 || pattern.len() > PATTERN_MAX {
            return Err(format!("签名长度必须在 1..={PATTERN_MAX}"));
        }

        if end <= start {
            return Err(String::from("扫描范围为空"));
        }

        let mut request = Self::default();
        request.space = space;
        request.cr3 = cr3;
        request.cursor = start;
        request.end = end;
        request.pattern_len = pattern.len() as u16;
        request.max_matches = SCAN_MAX_MATCHES as u16;
        request.pattern[..pattern.len()].copy_from_slice(pattern.bytes());
        request.mask[..pattern.len()].copy_from_slice(pattern.mask());

        Ok(request)
    }

    /// 每次调用的预算（0 表示用默认值）。
    pub fn with_budget(mut self, bytes: u32, micros: u32) -> Self {
        self.max_bytes = bytes;
        self.budget_us = micros;
        self
    }

    /// 匹配结果写到 `out_va`（`u64` 数组）。
    pub fn with_output(mut self, out_va: u64, capacity: u32) -> Self {
        self.out_va = out_va;
        self.out_cap = capacity;
        self
    }

    pub fn status(&self) -> MemError {
        MemError::from_u16(self.status)
    }

    pub fn is_done(&self) -> bool {
        self.done != 0
    }
}

/// 物理内存区间（16 字节）。
#[repr(C)]
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct PhysRange {
    pub start: u64,
    pub size: u64,
}

const _: () = assert!(std::mem::size_of::<PhysRange>() == 16);

/// 每 CPU 统计（48 字节）。
#[repr(C)]
#[derive(Debug, Clone, Copy, Default)]
pub struct MemStats {
    pub cpu: u32,
    pub reserved: u32,
    pub reads: u64,
    pub writes: u64,
    pub faults: u64,
    pub scanned: u64,
    pub root_tsc: u64,
}

const _: () = assert!(std::mem::size_of::<MemStats>() == 48);

fn write_u16(bytes: &mut [u8], offset: usize, value: u16) {
    bytes[offset..offset + 2].copy_from_slice(&value.to_le_bytes());
}

fn write_u32(bytes: &mut [u8], offset: usize, value: u32) {
    bytes[offset..offset + 4].copy_from_slice(&value.to_le_bytes());
}

fn write_u64(bytes: &mut [u8], offset: usize, value: u64) {
    bytes[offset..offset + 8].copy_from_slice(&value.to_le_bytes());
}

fn read_u16(bytes: &[u8], offset: usize) -> u16 {
    u16::from_le_bytes(bytes[offset..offset + 2].try_into().unwrap())
}

fn read_u32(bytes: &[u8], offset: usize) -> u32 {
    u32::from_le_bytes(bytes[offset..offset + 4].try_into().unwrap())
}

fn read_u64(bytes: &[u8], offset: usize) -> u64 {
    u64::from_le_bytes(bytes[offset..offset + 8].try_into().unwrap())
}

/// 一块环内存的视图：初始化头部、追加条目、读取结果。
///
/// # 用法
///
/// ```ignore
/// let mut buffer = vec![0u8; 1024 * 1024];
/// let mut ring = Ring::new(&mut buffer)?;
/// let off = ring.push_read(cr3, va, 8)?;
/// // VMCALL(MEM_BATCH, ring.as_ptr(), ring.len(), 0)
/// let data = ring.payload(off, 8)?;
/// ```
pub struct Ring<'a> {
    bytes: &'a mut [u8],
    data_used: usize,
}

impl<'a> Ring<'a> {
    /// 校验容量并写入空头。
    pub fn new(bytes: &'a mut [u8]) -> Result<Self, MemError> {
        if bytes.len() < MIN_RING_BYTES
            || bytes.len() > MAX_RING_BYTES
            || !bytes.len().is_power_of_two()
        {
            return Err(MemError::BadEntry);
        }

        let mut ring = Self {
            bytes,
            data_used: 0,
        };
        ring.reset();

        Ok(ring)
    }

    /// 清空头与数据区（在每批之前调用）。
    pub fn reset(&mut self) {
        self.bytes[..HEADER_SIZE].fill(0);

        write_u32(self.bytes, 0x00, RING_MAGIC);
        write_u16(self.bytes, 0x04, RING_ABI);
        write_u32(self.bytes, 0x08, self.bytes.len() as u32);
        write_u16(self.bytes, 0x0C, MAX_BATCH_OPS as u16);

        self.data_used = 0;
    }

    pub fn as_ptr(&self) -> *const u8 {
        self.bytes.as_ptr()
    }

    pub fn len(&self) -> usize {
        self.bytes.len()
    }

    pub fn is_empty(&self) -> bool {
        self.bytes.is_empty()
    }

    /// 已提交条数。
    pub fn count(&self) -> usize {
        read_u16(self.bytes, 0x10) as usize
    }

    /// hypervisor 回写的结构错误（0 = 无）。
    pub fn error(&self) -> u32 {
        read_u32(self.bytes, 0x18)
    }

    /// 头部快照（统计/调试用）。
    pub fn header(&self) -> MemRingHeader {
        MemRingHeader {
            magic: read_u32(self.bytes, 0x00),
            abi: read_u16(self.bytes, 0x04),
            flags: read_u16(self.bytes, 0x06),
            capacity: read_u32(self.bytes, 0x08),
            entry_capacity: read_u16(self.bytes, 0x0C),
            head: read_u16(self.bytes, 0x0E),
            tail: read_u16(self.bytes, 0x10),
            reserved0: read_u16(self.bytes, 0x12),
            seq: read_u32(self.bytes, 0x14),
            error: read_u32(self.bytes, 0x18),
            reserved1: read_u32(self.bytes, 0x1C),
            batches: read_u64(self.bytes, 0x20),
            ops: read_u64(self.bytes, 0x28),
            bytes: read_u64(self.bytes, 0x30),
            root_tsc: read_u64(self.bytes, 0x38),
        }
    }

    fn set_tail(&mut self, tail: u16) {
        write_u16(self.bytes, 0x10, tail);
    }

    /// 追加一条操作，返回载荷在环内的偏移。
    fn push(&mut self, op: u16, cr3: u64, addr: u64, len: usize) -> Result<usize, MemError> {
        let count = self.count();

        if count >= MAX_BATCH_OPS {
            return Err(MemError::TooLarge);
        }

        let payload = if op == OP_TRANSLATE { 8 } else { len };

        if payload > MAX_OP_BYTES {
            return Err(MemError::TooLarge);
        }

        let offset = DATA_OFFSET + self.data_used;
        let end = offset + payload;

        if end > self.bytes.len() {
            return Err(MemError::TooLarge);
        }

        let entry = ENTRY_OFFSET + count * ENTRY_SIZE;

        write_u16(self.bytes, entry, op);
        write_u16(self.bytes, entry + 2, 0);
        write_u32(self.bytes, entry + 4, len as u32);
        write_u64(self.bytes, entry + 8, cr3);
        write_u64(self.bytes, entry + 16, addr);
        write_u32(self.bytes, entry + 24, offset as u32);
        write_u32(self.bytes, entry + 28, 0);

        self.set_tail((count + 1) as u16);
        self.data_used = (end - DATA_OFFSET + 7) & !7;

        Ok(offset)
    }

    /// 读一条虚拟地址，返回结果载荷的偏移。
    pub fn push_read(&mut self, cr3: u64, va: u64, len: usize) -> Result<usize, MemError> {
        self.push(OP_READ_VIRT, cr3, va, len)
    }

    /// 写一条虚拟地址（数据立即拷进环）。
    pub fn push_write(&mut self, cr3: u64, va: u64, data: &[u8]) -> Result<usize, MemError> {
        let offset = self.push(OP_WRITE_VIRT, cr3, va, data.len())?;
        self.bytes[offset..offset + data.len()].copy_from_slice(data);
        Ok(offset)
    }

    pub fn push_read_phys(&mut self, pa: u64, len: usize) -> Result<usize, MemError> {
        self.push(OP_READ_PHYS, 0, pa, len)
    }

    pub fn push_write_phys(&mut self, pa: u64, data: &[u8]) -> Result<usize, MemError> {
        let offset = self.push(OP_WRITE_PHYS, 0, pa, data.len())?;
        self.bytes[offset..offset + data.len()].copy_from_slice(data);
        Ok(offset)
    }

    /// 翻译一条虚拟地址，结果（GPA）写在返回的载荷偏移处。
    pub fn push_translate(&mut self, cr3: u64, va: u64) -> Result<usize, MemError> {
        self.push(OP_TRANSLATE, cr3, va, 0)
    }

    /// 读第 `index` 条的执行结果。
    pub fn entry(&self, index: usize) -> Option<MemOp> {
        if index >= MAX_BATCH_OPS || index >= self.count() {
            return None;
        }

        let offset = ENTRY_OFFSET + index * ENTRY_SIZE;

        Some(MemOp {
            op: read_u16(self.bytes, offset),
            status: read_u16(self.bytes, offset + 2),
            len: read_u32(self.bytes, offset + 4),
            cr3: read_u64(self.bytes, offset + 8),
            addr: read_u64(self.bytes, offset + 16),
            off: read_u32(self.bytes, offset + 24),
            aux: read_u32(self.bytes, offset + 28),
        })
    }

    /// 载荷切片（读结果，或写入前的内容）。
    pub fn payload(&self, offset: usize, len: usize) -> Option<&[u8]> {
        self.bytes.get(offset..offset + len)
    }

    /// 逐条检查结果；返回第一个失败项。
    pub fn first_failure(&self) -> Option<(usize, MemError)> {
        for index in 0..self.count() {
            let entry = self.entry(index)?;
            let error = MemError::from_u16(entry.status);

            if !error.is_ok() {
                return Some((index, error));
            }
        }

        None
    }

    /// 把 hypervisor 的 `head` 归零（同一块环重复使用前调用）。
    pub fn reset_head(&mut self) {
        write_u16(self.bytes, 0x0E, 0);
        write_u16(self.bytes, 0x10, 0);
        self.data_used = 0;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ring_buffer() -> Vec<u8> {
        vec![0u8; 64 * 1024]
    }

    #[test]
    fn initializes_header() {
        let mut buffer = ring_buffer();
        let expected_len = buffer.len();
        let ring = Ring::new(&mut buffer).unwrap();
        let header = ring.header();

        assert_eq!(header.magic, RING_MAGIC);
        assert_eq!(header.abi, RING_ABI);
        assert_eq!(header.capacity as usize, expected_len);
        assert_eq!(header.entry_capacity as usize, MAX_BATCH_OPS);
        assert_eq!(ring.count(), 0);
    }

    #[test]
    fn pushes_entries_and_payloads() {
        let mut buffer = ring_buffer();
        let mut ring = Ring::new(&mut buffer).unwrap();

        let read_off = ring.push_read(0x1234_5000, 0xFFFF_8000_0000_1000, 8).unwrap();
        let write_off = ring.push_write(0x1234_5000, 0xFFFF_8000_0000_2000, b"abcd").unwrap();

        assert_eq!(ring.count(), 2);
        assert_eq!(read_off, DATA_OFFSET);
        assert_eq!(write_off, DATA_OFFSET + 8);
        assert_eq!(ring.payload(write_off, 4).unwrap(), b"abcd");

        let first = ring.entry(0).unwrap();
        assert_eq!(first.op, OP_READ_VIRT);
        assert_eq!(first.addr, 0xFFFF_8000_0000_1000);
        assert_eq!(first.off as usize, DATA_OFFSET);
    }

    #[test]
    fn enforces_capacity_limits() {
        let mut buffer = ring_buffer();

        {
            let mut ring = Ring::new(&mut buffer).unwrap();

            for _ in 0..MAX_BATCH_OPS {
                ring.push_read(1, 0, 8).unwrap();
            }

            assert!(ring.push_read(1, 0, 8).is_err());
        }

        {
            let mut ring = Ring::new(&mut buffer).unwrap();
            assert!(ring.push_read(1, 0, MAX_OP_BYTES + 1).is_err());
        }
    }

    #[test]
    fn rejects_bad_capacity() {
        let mut small = vec![0u8; 4096];
        assert!(Ring::new(&mut small).is_err());

        let mut odd = vec![0u8; 96 * 1024];
        assert!(Ring::new(&mut odd).is_err());
    }

    #[test]
    fn reports_first_failure() {
        let mut buffer = ring_buffer();

        {
            let mut ring = Ring::new(&mut buffer).unwrap();
            ring.push_read(1, 0x1000, 8).unwrap();
            ring.push_read(1, 0x2000, 8).unwrap();

            // 手工把第二条状态改成 NotPresent。
            let entry = ENTRY_OFFSET + ENTRY_SIZE;
            write_u16(ring.bytes, entry + 2, MemError::NotPresent as u16);

            assert_eq!(ring.first_failure(), Some((1, MemError::NotPresent)));
        }
    }

    #[test]
    fn scan_request_round_trips_pattern() {
        let pattern = Pattern::parse("4D 5A ??").unwrap();
        let request = MemScanRequest::virt(0xABCD_000, 0x1000, 0x2000, &pattern)
            .unwrap()
            .with_budget(64 * 1024, 100)
            .with_output(0x5000, 16);

        assert_eq!(request.pattern_len, 3);
        assert_eq!(&request.pattern[..3], &[0x4d, 0x5a, 0]);
        assert_eq!(&request.mask[..3], &[0xFF, 0xFF, 0]);
        assert_eq!(request.status(), MemError::Ok);
        assert!(!request.is_done());
    }

    #[test]
    fn scan_request_rejects_empty_range() {
        let pattern = Pattern::parse("4D 5A").unwrap();
        assert!(MemScanRequest::phys(0x2000, 0x2000, &pattern).is_err());
        assert!(MemScanRequest::phys(0x3000, 0x2000, &pattern).is_err());
    }

    #[test]
    fn error_round_trips() {
        for value in 0..=12u16 {
            assert_eq!(MemError::from_u16(value) as u16, value);
        }

        assert_eq!(MemError::from_u16(999), MemError::BadEntry);
    }
}
