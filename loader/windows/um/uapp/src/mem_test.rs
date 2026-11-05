//! 端到端自检：一次点完，覆盖 hypervisor 的 `MEM_*` 与 uapp 的 OS 层。
//!
//! 失败不中断：每一步都记 `ok/detail/elapsed`，最后一起显示。
//! 写测试**只写自己分配的一页**，不碰游戏/内核。

use std::time::{Duration, Instant};

use windows_sys::Win32::System::Memory::{
    VirtualAlloc, VirtualFree, MEM_COMMIT, MEM_RELEASE, MEM_RESERVE, PAGE_READWRITE,
};
use windows_sys::Win32::System::Threading::GetCurrentProcessId;

use uapp::pattern::Pattern;

use crate::command::OsInfo;
use crate::mem_client::{BenchConfig, MemClient};
use crate::os_win::WinOs;

/// 一步的结果。
#[derive(Debug, Clone)]
pub struct TestStep {
    pub name: &'static str,
    pub ok: bool,
    pub detail: String,
    pub elapsed: Duration,
}

impl TestStep {
    fn ok(name: &'static str, detail: impl Into<String>, elapsed: Duration) -> Self {
        Self {
            name,
            ok: true,
            detail: detail.into(),
            elapsed,
        }
    }

    fn fail(name: &'static str, detail: impl Into<String>, elapsed: Duration) -> Self {
        Self {
            name,
            ok: false,
            detail: detail.into(),
            elapsed,
        }
    }
}

/// 跑一遍自检。
pub fn run(mem: &mut MemClient, system_cr3: u64, info: &OsInfo) -> Vec<TestStep> {
    let mut steps = Vec::new();

    // 1. MEM_QUERY：能力面。
    {
        let started = Instant::now();

        steps.push(TestStep::ok(
            "MEM_QUERY",
            format!(
                "max_ops={} max_batch={} KiB direct_map={:#x}",
                mem.max_ops(),
                mem.max_batch_bytes() / 1024,
                mem.direct_map()
            ),
            started.elapsed(),
        ));
    }

    // 2. OS 引导 + V1–V4。
    let started = Instant::now();
    let attached = WinOs::attach(mem, system_cr3, info);

    let os = match attached {
        Ok(os) => {
            steps.push(TestStep::ok(
                "OS 引导 / V1–V4",
                format!(
                    "build {} 偏移表 {} checks [{}]{}",
                    os.build,
                    os.offsets.build,
                    os.check_summary(),
                    if os.notes.is_empty() {
                        String::new()
                    } else {
                        format!("；{}", os.notes.join("；"))
                    }
                ),
                started.elapsed(),
            ));
            os
        }
        Err(error) => {
            steps.push(TestStep::fail("OS 引导 / V1–V4", error, started.elapsed()));
            return steps;
        }
    };

    // 3. System EPROCESS + 进程链表。
    let started = Instant::now();

    match os.processes(mem) {
        Ok(processes) => {
            let system = processes.iter().find(|entry| entry.pid == 4);
            let alive = processes.iter().filter(|entry| entry.alive()).count();

            steps.push(TestStep::ok(
                "进程链表",
                format!(
                    "{} 个进程（存活 {alive}）；System: {}",
                    processes.len(),
                    system
                        .map(|entry| format!("pid 4 base {:#x}", entry.image_base))
                        .unwrap_or_else(|| String::from("缺失"))
                ),
                started.elapsed(),
            ));
        }
        Err(error) => steps.push(TestStep::fail("进程链表", error, started.elapsed())),
    }

    // 4. 模块链表：挑一个带 PEB 的进程（优先 explorer.exe）。
    let started = Instant::now();

    match os.processes(mem) {
        Ok(processes) => {
            let target = processes
                .iter()
                .find(|entry| entry.alive() && entry.name.eq_ignore_ascii_case("explorer.exe"))
                .or_else(|| {
                    processes
                        .iter()
                        .find(|entry| entry.alive() && entry.peb != 0 && entry.pid > 4)
                });

            match target {
                Some(process) => match os.modules(mem, process) {
                    Ok(modules) => steps.push(TestStep::ok(
                        "模块链表",
                        format!(
                            "{} 的模块 {} 个；首个 {:#x}",
                            process.name,
                            modules.len(),
                            modules.first().map(|module| module.base).unwrap_or(0)
                        ),
                        started.elapsed(),
                    )),
                    Err(error) => steps.push(TestStep::fail("模块链表", error, started.elapsed())),
                },
                None => steps.push(TestStep::fail(
                    "模块链表",
                    "没有找到带 PEB 的目标进程",
                    started.elapsed(),
                )),
            }
        }
        Err(error) => steps.push(TestStep::fail("模块链表", error, started.elapsed())),
    }

    // 5. 自己一页：写 → 读回。
    let started = Instant::now();
    let page = unsafe {
        VirtualAlloc(
            std::ptr::null_mut(),
            0x1000,
            MEM_COMMIT | MEM_RESERVE,
            PAGE_READWRITE,
        )
    };

    if page.is_null() {
        steps.push(TestStep::fail(
            "读写回环",
            "VirtualAlloc 失败",
            started.elapsed(),
        ));
    } else {
        // `VirtualAlloc(MEM_COMMIT)` 只建立 VAD，PTE 是 demand-zero：页没被触碰
        // 过之前，软件页表遍历会得到 NotPresent。先由用户态写一个字节把页带进来，
        // 这也正是 DMA/外部视角的固有语义（页不在就没有物理页可读）。
        unsafe {
            std::ptr::write_volatile(page as *mut u8, 0);
        }

        let result = (|| -> Result<String, String> {
            let cr3 = os
                .process_by_pid(mem, unsafe { GetCurrentProcessId() })
                .map_err(|error| error.to_string())?
                .and_then(|process| os.process_cr3(&process))
                .ok_or_else(|| String::from("进程链表里找不到自己"))?;

            let payload = b"noumen-mem-test";
            mem.write_virt(cr3, page as u64, payload)
                .map_err(|error| error.to_string())?;

            let mut readback = [0u8; 16];
            mem.read_virt(cr3, page as u64, &mut readback)
                .map_err(|error| error.to_string())?;

            if &readback[..payload.len()] != payload {
                return Err(String::from("读回内容不一致"));
            }

            Ok(format!("cr3 {cr3:#x} page {:#x} 16 字节一致", page as u64))
        })();

        match result {
            Ok(detail) => steps.push(TestStep::ok("读写回环", detail, started.elapsed())),
            Err(error) => steps.push(TestStep::fail("读写回环", error, started.elapsed())),
        }

        unsafe {
            let _ = VirtualFree(page, 0, MEM_RELEASE);
        }
    }

    // 6. 签名扫描：ntoskrnl 的第一页必然以 MZ 开头。
    let started = Instant::now();
    let pattern = Pattern::parse("4D 5A").expect("常量签名");

    match uapp::mem_layout::MemScanRequest::virt(os.system_cr3, os.nt_base, os.nt_base + 0x1000, &pattern)
    {
        Ok(mut request) => {
            request.flags |= uapp::mem_layout::SCAN_FLAG_MATCH_ALIGNED;

            match mem.scan_all(&mut request, &std::sync::atomic::AtomicBool::new(false)) {
                Ok(matches) => {
                    if matches.first().copied() == Some(os.nt_base) {
                        steps.push(TestStep::ok(
                            "签名扫描",
                            format!("ntoskrnl 首地址 {:#x} 命中", os.nt_base),
                            started.elapsed(),
                        ));
                    } else {
                        steps.push(TestStep::fail(
                            "签名扫描",
                            format!("期望 {:#x}，实际 {:?}", os.nt_base, matches.first()),
                            started.elapsed(),
                        ));
                    }
                }
                Err(error) => steps.push(TestStep::fail("签名扫描", error.to_string(), started.elapsed())),
            }
        }
        Err(error) => steps.push(TestStep::fail("签名扫描", error, started.elapsed())),
    }

    // 7. 批量测速。
    let started = Instant::now();
    let config = BenchConfig {
        batch_ops: mem.max_ops().min(126),
        batches: 100,
        len: 8,
        cr3: os.system_cr3,
        va: os.nt_base,
    };

    match mem.bench_read(&config) {
        Ok(report) => steps.push(TestStep::ok(
            "批量测速",
            format!("{} 条/批 × {} 批：{}", config.batch_ops, config.batches, report.summary()),
            started.elapsed(),
        )),
        Err(error) => steps.push(TestStep::fail("批量测速", error.to_string(), started.elapsed())),
    }

    // 8. 统计对账。
    let started = Instant::now();

    match mem.stats() {
        Ok(stats) => {
            let reads: u64 = stats.iter().map(|entry| entry.reads).sum();
            let faults: u64 = stats.iter().map(|entry| entry.faults).sum();

            steps.push(TestStep::ok(
                "MEM_STATS",
                format!(
                    "{} 个 CPU 槽位；reads={reads} faults={faults} scanned={}",
                    stats.len(),
                    stats.iter().map(|entry| entry.scanned).sum::<u64>()
                ),
                started.elapsed(),
            ));
        }
        Err(error) => steps.push(TestStep::fail("MEM_STATS", error.to_string(), started.elapsed())),
    }

    // 9. 物理区间 + 物理扫描：区间表来自驱动 `MmGetPhysicalMemoryRanges`，
    // 放在镜像尾部；扫描只在 RAM 段内推进，空洞不发起读。
    let started = Instant::now();

    match mem.phys_map() {
        Ok(ranges) if !ranges.is_empty() => {
            let total: u64 = ranges.iter().map(|(_, size)| *size).sum();
            let (start, _) = ranges[0];
            let pattern = Pattern::parse("4D 5A").expect("常量签名");

            let result = uapp::mem_layout::MemScanRequest::phys(start, start + 0x1_0000, &pattern)
                .map_err(|error| error.to_string())
                .and_then(|mut request| {
                    request.flags |= uapp::mem_layout::SCAN_FLAG_SKIP_INACCESSIBLE;
                    mem.scan_all(&mut request, &std::sync::atomic::AtomicBool::new(false))
                        .map(|matches| format!("{start:#x} 起 64 KiB：{} 命中", matches.len()))
                        .map_err(|error| error.to_string())
                });

            match result {
                Ok(detail) => steps.push(TestStep::ok(
                    "物理区间/扫描",
                    format!(
                        "{} 段 / {:.1} GiB；{}",
                        ranges.len(),
                        total as f64 / (1024.0 * 1024.0 * 1024.0),
                        detail
                    ),
                    started.elapsed(),
                )),
                Err(error) => steps.push(TestStep::fail("物理区间/扫描", error, started.elapsed())),
            }
        }
        Ok(_) => {
            // 驱动可以传空的 phys_map_base（不分配区间表）：扫描退化为
            // `pa < DIRECT_MAP_SIZE` 的直接映射，空洞由 HostFault 跳过。
            let pattern = Pattern::parse("4D 5A").expect("常量签名");

            let result = uapp::mem_layout::MemScanRequest::phys(0x1000, 0x1_1000, &pattern)
                .map_err(|error| error.to_string())
                .and_then(|mut request| {
                    request.flags |= uapp::mem_layout::SCAN_FLAG_SKIP_INACCESSIBLE;

                    mem.scan_all(&mut request, &std::sync::atomic::AtomicBool::new(false))
                        .map(|matches| matches.len())
                        .map_err(|error| error.to_string())
                });

            match result {
                Ok(hits) => steps.push(TestStep::ok(
                    "物理区间/扫描",
                    format!("未提供区间表（phys_map_base=0）；0x1000 起 64 KiB：{hits} 命中"),
                    started.elapsed(),
                )),
                Err(error) => steps.push(TestStep::fail("物理区间/扫描", error, started.elapsed())),
            }
        }
        Err(error) => steps.push(TestStep::fail(
            "物理区间/扫描",
            error.to_string(),
            started.elapsed(),
        )),
    }

    steps
}

/// 把结果渲染成文本（UI/日志共用）。
pub fn format_report(steps: &[TestStep]) -> String {
    let mut out = String::new();
    let passed = steps.iter().filter(|step| step.ok).count();

    out.push_str(&format!("自检 {passed}/{} 通过\n", steps.len()));

    for step in steps {
        out.push_str(&format!(
            "{} {:<16} {:>6.2} ms  {}\n",
            if step.ok { "PASS" } else { "FAIL" },
            step.name,
            step.elapsed.as_secs_f64() * 1000.0,
            step.detail
        ));
    }

    out
}
