//! uapp 的内存通道：一块环 + 直接 `VMCALL`。
//!
//! 这是 uapp 侧唯一接触 hypercall 的地方。设计要点：
//!
//! * **一条小读也走 `MEM_BATCH`**：少一个 ABI 面，而且批量本来就要用；
//! * 环是本地内存（`VirtualAlloc` + `VirtualLock`），由 hypervisor 用当前 CR3 翻译；
//! * 单核模式必须把线程钉在 `start_processor` 上才能发 VMCALL（`PinnedThread`）；
//! * 批量/单条都返回 [`MemError`]，不 panic。
//!
//! `bench_*` 是测速函数：用同一套环，测「单条 vs 批量」的摊还成本。

use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

use windows_sys::Win32::System::Memory::{
    VirtualAlloc, VirtualFree, VirtualLock, VirtualUnlock, MEM_COMMIT, MEM_RELEASE, MEM_RESERVE,
    PAGE_READWRITE,
};

use uapp::mem_layout::{
    self, MemError, MemScanRequest, MemStats, Ring, MAX_BATCH_OPS, MAX_OP_BYTES,
    SCAN_MAX_MATCHES,
};

use crate::command;
use crate::hypercall;

/// 环大小。1 MiB 能放下 126 条 4 KiB（504 KiB）载荷。
const RING_BYTES: usize = 1024 * 1024;

/// `VirtualAlloc` 出来的一块内存；Drop 时释放并解锁。
struct RingBuffer {
    ptr: *mut u8,
    len: usize,
}

impl RingBuffer {
    fn new(len: usize) -> Result<Self, String> {
        let ptr = unsafe {
            VirtualAlloc(
                std::ptr::null_mut(),
                len,
                MEM_COMMIT | MEM_RESERVE,
                PAGE_READWRITE,
            ) as *mut u8
        };

        if ptr.is_null() {
            return Err(format!(
                "VirtualAlloc 失败: {}",
                std::io::Error::last_os_error()
            ));
        }

        let buffer = Self { ptr, len };

        // 预触碰：保证每一页都有物理页，避免第一次 VMCALL 时缺页。
        unsafe {
            std::ptr::write_bytes(ptr, 0, len);
        }

        // VirtualLock 失败不致命（页可能被换出，但我们每批都会先写头）。
        unsafe {
            let _ = VirtualLock(ptr as *const _, len);
        }

        Ok(buffer)
    }

    fn as_mut_slice(&mut self) -> &mut [u8] {
        unsafe { std::slice::from_raw_parts_mut(self.ptr, self.len) }
    }
}

impl Drop for RingBuffer {
    fn drop(&mut self) {
        if !self.ptr.is_null() {
            unsafe {
                let _ = VirtualUnlock(self.ptr as *const _, self.len);
                let _ = VirtualFree(self.ptr as *mut _, 0, MEM_RELEASE);
            }
        }
    }
}

/// uapp 的内存客户端。
pub struct MemClient {
    /// 持有设备句柄（drop 时关闭），不直接使用。
    _dev: command::Device,
    key: u64,
    info: command::Info,
    ring: RingBuffer,
    max_ops: usize,
    max_batch_bytes: u32,
    direct_map: u64,
    /// 单核模式下必须钉在 `start_processor` 上；`None` 表示全核。
    _pin: Option<hypercall::PinnedThread>,
}

impl MemClient {
    /// 打开设备、查能力、分配环。
    pub fn new(dev: command::Device) -> Result<Self, String> {
        let info = dev.get_info().map_err(|error| error.to_string())?;

        if !info.is_running() {
            return Err(format!(
                "hypervisor 未运行（状态: {}）",
                info.state_name()
            ));
        }

        // 单核模式：只有 start_processor 在 VMX 里，VMCALL 必须在它上面执行。
        let pin = if info.single_core != 0 {
            Some(
                hypercall::PinnedThread::new(info.start_processor)
                    .map_err(|error| format!("绑核到处理器 {} 失败: {error}", info.start_processor))?,
            )
        } else {
            None
        };

        let query = unsafe { hypercall::mem_query(info.hypercall_key) };

        if query.abi == 0 {
            return Err(String::from("MEM_QUERY 返回 ABI 0，镜像可能没编 mem feature"));
        }

        if query.max_ops == 0 || query.max_batch_bytes == 0 {
            return Err(String::from("MEM_QUERY 能力非法"));
        }

        let ring = RingBuffer::new(RING_BYTES)?;

        Ok(Self {
            _dev: dev,
            key: info.hypercall_key,
            info,
            ring,
            max_ops: (query.max_ops as usize).min(MAX_BATCH_OPS),
            max_batch_bytes: query.max_batch_bytes as u32,
            direct_map: query.direct_map,
            _pin: pin,
        })
    }

    pub fn info(&self) -> &command::Info {
        &self.info
    }

    pub fn max_ops(&self) -> usize {
        self.max_ops
    }

    pub fn max_batch_bytes(&self) -> u32 {
        self.max_batch_bytes
    }

    pub fn direct_map(&self) -> u64 {
        self.direct_map
    }


    /// 读一段虚拟内存（可跨页；自动拆成多条）。
    pub fn read_virt(&mut self, cr3: u64, va: u64, out: &mut [u8]) -> Result<(), MemError> {
        self.read_chunks(cr3, va, out, false)
    }

    /// 写一段虚拟内存。
    pub fn write_virt(&mut self, cr3: u64, va: u64, data: &[u8]) -> Result<(), MemError> {
        self.write_chunks(cr3, va, data, false)
    }




    /// 读一个 `u64`。
    pub fn read_u64(&mut self, cr3: u64, va: u64) -> Result<u64, MemError> {
        let mut buffer = [0u8; 8];
        self.read_virt(cr3, va, &mut buffer)?;
        Ok(u64::from_le_bytes(buffer))
    }

    pub fn read_u32(&mut self, cr3: u64, va: u64) -> Result<u32, MemError> {
        let mut buffer = [0u8; 4];
        self.read_virt(cr3, va, &mut buffer)?;
        Ok(u32::from_le_bytes(buffer))
    }

    pub fn read_u16(&mut self, cr3: u64, va: u64) -> Result<u16, MemError> {
        let mut buffer = [0u8; 2];
        self.read_virt(cr3, va, &mut buffer)?;
        Ok(u16::from_le_bytes(buffer))
    }


    /// 物理内存区间表：`(start, size)`，来自驱动 `MmGetPhysicalMemoryRanges`。
    ///
    /// 驱动把表放在镜像尾部（与镜像同一次分配），`MEM_MAP` 把它拷到用户缓冲。
    pub fn phys_map(&mut self) -> Result<Vec<(u64, u64)>, MemError> {
        const CAPACITY: usize = 256;

        let mut buffer = vec![0u8; CAPACITY * 16];
        let written = unsafe {
            hypercall::vmcall(
                self.key,
                hypercall::FUNCTION_MEM_MAP,
                buffer.as_mut_ptr() as u64,
                CAPACITY as u64,
            )
        } as u32;

        let mut out = Vec::new();

        for chunk in buffer.chunks_exact(16).take(written as usize) {
            let start = u64::from_le_bytes(chunk[0..8].try_into().unwrap());
            let size = u64::from_le_bytes(chunk[8..16].try_into().unwrap());

            if size != 0 {
                out.push((start, size));
            }
        }

        Ok(out)
    }
    /// 每 CPU 统计。
    pub fn stats(&mut self) -> Result<Vec<MemStats>, MemError> {
        let mut buffer: Vec<MemStats> = vec![MemStats::default(); 64];
        let written = unsafe {
            hypercall::vmcall(
                self.key,
                hypercall::FUNCTION_MEM_STATS,
                buffer.as_mut_ptr() as u64,
                buffer.len() as u64,
            )
        };

        buffer.truncate(written as usize);
        Ok(buffer)
    }

    /// 分段扫描的一步：调用方持有 `MemScanRequest`（游标在结构体里）。
    ///
    /// 返回本次命中的地址。`done` 表示扫完，`status` 是 `MemError`。
    pub fn scan_step(&mut self, request: &mut MemScanRequest) -> Result<ScanStep, MemError> {
        let mut matches = [0u64; SCAN_MAX_MATCHES];

        request.out_va = matches.as_mut_ptr() as u64;
        request.out_cap = SCAN_MAX_MATCHES as u32;
        request.max_matches = SCAN_MAX_MATCHES as u16;

        let status = unsafe {
            hypercall::vmcall3(
                self.key,
                hypercall::FUNCTION_MEM_SCAN,
                request as *mut MemScanRequest as u64,
                std::mem::size_of::<MemScanRequest>() as u64,
                0,
            )
        } as u16;

        let error = MemError::from_u16(status);
        let matches = matches[..request.matches as usize].to_vec();

        Ok(ScanStep {
            matches,
            done: request.done != 0,
            scanned: request.scanned,
            status: error,
        })
    }

    /// 把扫描跑完（自检/bench 用）。`limit` 是安全上限，防止死循环。
    pub fn scan_all(
        &mut self,
        request: &mut MemScanRequest,
        cancel: &AtomicBool,
    ) -> Result<Vec<u64>, MemError> {
        let mut all = Vec::new();
        let limit = 1_000_000usize;

        for _ in 0..limit {
            if cancel.load(Ordering::Relaxed) {
                break;
            }

            let step = self.scan_step(request)?;
            all.extend_from_slice(&step.matches);

            if !step.status.is_ok() {
                return Err(step.status);
            }

            if step.done {
                break;
            }
        }

        Ok(all)
    }

    // ---- bench ---------------------------------------------------------

    /// 批量读测速。
    ///
    /// 每批 `cfg.batch_ops` 条、每条 `cfg.len` 字节，重复 `cfg.batches` 次；
    /// 返回吞吐、平均/ p50 / p99 每批耗时，以及 hypervisor 侧的 root 驻留。
    pub fn bench_read(&mut self, cfg: &BenchConfig) -> Result<BenchReport, MemError> {
        let batch_ops = cfg.batch_ops.clamp(1, self.max_ops);
        let len = cfg.len.clamp(1, MAX_OP_BYTES);
        let batches = cfg.batches.max(1);
        let key = self.key;

        let mut latencies = Vec::with_capacity(batches);
        let mut root_tsc = 0u64;
        let mut elapsed_total = Duration::ZERO;
        let mut payload_bytes = 0u64;

        for _ in 0..batches {
            let started = Instant::now();

            let mut ring = Ring::new(self.ring.as_mut_slice())?;

            for index in 0..batch_ops {
                let va = cfg
                    .va
                    .wrapping_add((index as u64) * (len as u64))
                    & !0x7u64;
                ring.push_read(cfg.cr3, va, len)?;
            }

            let processed = Self::batch_call(key, &ring)?;

            if processed as usize != batch_ops {
                return Err(MemError::BadEntry);
            }

            if let Some((_, error)) = ring.first_failure() {
                return Err(error);
            }

            // 读结果，顺带验证载荷可用。
            for index in 0..batch_ops {
                let entry = ring.entry(index).ok_or(MemError::BadEntry)?;
                let bytes = ring.payload(entry.off as usize, entry.len as usize)
                    .ok_or(MemError::BadEntry)?;
                payload_bytes += bytes.len() as u64;
            }

            let header = ring.header();
            drop(ring);

            let elapsed = started.elapsed();
            elapsed_total += elapsed;
            latencies.push(elapsed);
            root_tsc += header.root_tsc;
        }

        Ok(BenchReport::from_samples(
            latencies,
            elapsed_total,
            (batches * batch_ops) as u64,
            payload_bytes,
            root_tsc,
        ))
    }

    /// 单条读测速（每批只有 1 条），用来对比批量的摊还收益。
    pub fn bench_single(&mut self, cfg: &BenchConfig) -> Result<BenchReport, MemError> {
        let single = BenchConfig {
            batch_ops: 1,
            ..*cfg
        };

        self.bench_read(&single)
    }


    // ---- 内部 ----------------------------------------------------------

    fn batch_call(key: u64, ring: &Ring<'_>) -> Result<u32, MemError> {
        let processed = unsafe {
            hypercall::vmcall3(
                key,
                hypercall::FUNCTION_MEM_BATCH,
                ring.as_ptr() as u64,
                ring.len() as u64,
                0,
            )
        } as u32;

        let error = ring.error();

        if error != 0 {
            return Err(match error {
                1 => MemError::BadEntry,
                2 => MemError::BadEntry,
                _ => MemError::Unsupported,
            });
        }

        Ok(processed)
    }

    fn read_chunks(
        &mut self,
        cr3: u64,
        addr: u64,
        out: &mut [u8],
        phys: bool,
    ) -> Result<(), MemError> {
        let key = self.key;
        let mut done = 0usize;

        while done < out.len() {
            let mut ring = Ring::new(self.ring.as_mut_slice())?;
            let mut pending: Vec<(usize, usize, usize)> = Vec::new();
            let mut chunk = done;

            while pending.len() < self.max_ops && chunk < out.len() {
                let remaining = out.len() - chunk;
                let len = remaining.min(MAX_OP_BYTES);
                let address = addr.wrapping_add(chunk as u64);

                let offset = if phys {
                    ring.push_read_phys(address, len)?
                } else {
                    ring.push_read(cr3, address, len)?
                };

                pending.push((offset, chunk, len));
                chunk += len;
            }

            if pending.is_empty() {
                break;
            }

            let processed = Self::batch_call(key, &ring)?;

            if processed as usize != pending.len() {
                return Err(MemError::BadEntry);
            }

            for (index, (offset, destination, len)) in pending.iter().enumerate() {
                let entry = ring.entry(index).ok_or(MemError::BadEntry)?;
                let status = MemError::from_u16(entry.status);

                if !status.is_ok() {
                    return Err(status);
                }

                let payload = ring
                    .payload(*offset, *len)
                    .ok_or(MemError::BadEntry)?;

                out[*destination..*destination + *len].copy_from_slice(payload);
            }

            done = chunk;
        }

        Ok(())
    }

    /// 散列批量读：一次提交多条**不连续**读，逐条返回结果。
    ///
    /// 失败条目返回 `None`（典型的失败是被换出的页 → `NotPresent`），调用方不必
    /// 逐条处理错误；只有环/协议本身出错才返回 `Err`。
    ///
    /// 导出表解析那种“几千个小读”的场景用它：2500 个导出名从 2500 次 VMCALL
    /// 降到 ~25 次。
    pub fn read_many(
        &mut self,
        cr3: u64,
        reads: &[(u64, usize)],
    ) -> Result<Vec<Option<Vec<u8>>>, MemError> {
        let key = self.key;
        let mut out = Vec::with_capacity(reads.len());

        for chunk in reads.chunks(self.max_ops) {
            let mut ring = Ring::new(self.ring.as_mut_slice())?;
            let mut pending: Vec<(usize, usize)> = Vec::with_capacity(chunk.len());

            for (va, len) in chunk {
                if *len > MAX_OP_BYTES {
                    return Err(MemError::TooLarge);
                }

                let offset = ring.push_read(cr3, *va, *len)?;
                pending.push((offset, *len));
            }

            let processed = Self::batch_call(key, &ring)?;

            if processed as usize != pending.len() {
                return Err(MemError::BadEntry);
            }

            for (index, (offset, len)) in pending.iter().enumerate() {
                let entry = ring.entry(index).ok_or(MemError::BadEntry)?;

                if !MemError::from_u16(entry.status).is_ok() {
                    out.push(None);
                    continue;
                }

                let payload = ring.payload(*offset, *len).ok_or(MemError::BadEntry)?;
                out.push(Some(payload.to_vec()));
            }
        }

        Ok(out)
    }

    fn write_chunks(
        &mut self,
        cr3: u64,
        addr: u64,
        data: &[u8],
        phys: bool,
    ) -> Result<(), MemError> {
        let key = self.key;
        let mut done = 0usize;

        while done < data.len() {
            let mut ring = Ring::new(self.ring.as_mut_slice())?;
            let mut pending = 0usize;
            let mut chunk = done;

            while pending < self.max_ops && chunk < data.len() {
                let remaining = data.len() - chunk;
                let len = remaining.min(MAX_OP_BYTES);
                let slice = &data[chunk..chunk + len];
                let address = addr.wrapping_add(chunk as u64);

                if phys {
                    ring.push_write_phys(address, slice)?;
                } else {
                    ring.push_write(cr3, address, slice)?;
                }

                pending += 1;
                chunk += len;
            }

            if pending == 0 {
                break;
            }

            let processed = Self::batch_call(key, &ring)?;

            if processed as usize != pending {
                return Err(MemError::BadEntry);
            }

            if let Some((_, error)) = ring.first_failure() {
                return Err(error);
            }

            done = chunk;
        }

        Ok(())
    }
}



/// 一次扫描的进展。
#[derive(Debug, Clone)]
pub struct ScanStep {
    pub matches: Vec<u64>,
    pub done: bool,
    pub scanned: u32,
    pub status: MemError,
}

/// 测速参数。
#[derive(Debug, Clone, Copy)]
pub struct BenchConfig {
    pub batch_ops: usize,
    pub batches: usize,
    pub len: usize,
    pub cr3: u64,
    pub va: u64,
}

impl Default for BenchConfig {
    fn default() -> Self {
        Self {
            batch_ops: MAX_BATCH_OPS,
            batches: 200,
            len: 8,
            cr3: 0,
            va: 0,
        }
    }
}

/// 测速结果。
#[derive(Debug, Clone, Default)]
pub struct BenchReport {
    pub bytes: u64,
    pub elapsed: Duration,
    pub ops_per_sec: f64,
    pub mb_per_sec: f64,
    pub avg_us: f64,
    pub p50_us: f64,
    pub p99_us: f64,
    pub root_tsc: u64,
}

impl BenchReport {
    fn from_samples(
        mut latencies: Vec<Duration>,
        elapsed: Duration,
        ops: u64,
        bytes: u64,
        root_tsc: u64,
    ) -> Self {
        latencies.sort();

        let pick = |percent: f64| -> f64 {
            if latencies.is_empty() {
                return 0.0;
            }

            let index = ((latencies.len() as f64 - 1.0) * percent).round() as usize;
            latencies[index.min(latencies.len() - 1)].as_secs_f64() * 1_000_000.0
        };

        let seconds = elapsed.as_secs_f64().max(1e-9);

        Self {
            bytes,
            elapsed,
            ops_per_sec: ops as f64 / seconds,
            mb_per_sec: bytes as f64 / seconds / (1024.0 * 1024.0),
            avg_us: elapsed.as_secs_f64() * 1_000_000.0 / latencies.len().max(1) as f64,
            p50_us: pick(0.50),
            p99_us: pick(0.99),
            root_tsc,
        }
    }

    pub fn summary(&self) -> String {
        format!(
            "{:.0} ops/s, {:.1} MiB/s, avg {:.1} µs, p50 {:.1} µs, p99 {:.1} µs, root_tsc {}",
            self.ops_per_sec,
            self.mb_per_sec,
            self.avg_us,
            self.p50_us,
            self.p99_us,
            self.root_tsc
        )
    }
}

const _: () = assert!(RING_BYTES >= mem_layout::MIN_RING_BYTES);
const _: () = assert!(RING_BYTES <= mem_layout::MAX_RING_BYTES);
