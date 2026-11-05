//! “内存”面板：连接、读写、扫描、进程/模块、测速、自检。
//!
//! 所有操作都走 [`MemClient`]（`MEM_*` hypercall），不新增 IOCTL。UI 只做输入
//! 校验与结果展示，失败一律写 `status`，不 panic。

use eframe::egui;

use crate::{
    command::{self, KernelArgs},
    mem_client::{BenchConfig, MemClient},
    mem_test,
    os_win::{ModuleEntry, ProcessEntry, WinOs},
};

/// 面板状态。
pub struct MemUi {
    client: Option<MemClient>,
    os: Option<WinOs>,
    kargs: Option<KernelArgs>,
    os_info: Option<command::OsInfo>,
    phys_summary: String,
    status: String,

    cr3: String,
    addr: String,
    len: String,
    hexdump: String,

    write_text: String,
    allow_write: bool,

    processes: Vec<ProcessEntry>,
    modules: Vec<ModuleEntry>,
    selected_process: Option<usize>,

    pattern: String,
    scan_start: String,
    scan_end: String,
    scan_phys: bool,
    scan_request: Option<uapp::mem_layout::MemScanRequest>,
    scan_matches: Vec<u64>,
    scan_scanned: u64,

    bench_ops: String,
    bench_batches: String,
    bench_result: String,

    selftest: String,
}

impl Default for MemUi {
    fn default() -> Self {
        Self {
            client: None,
            os: None,
            kargs: None,
            os_info: None,
            phys_summary: String::new(),
            status: String::from("未连接（先启动 hypervisor）"),

            cr3: String::new(),
            addr: String::new(),
            len: String::from("16"),
            hexdump: String::new(),

            write_text: String::new(),
            allow_write: false,

            processes: Vec::new(),
            modules: Vec::new(),
            selected_process: None,

            pattern: String::from("4D 5A"),
            scan_start: String::new(),
            scan_end: String::new(),
            scan_phys: false,
            scan_request: None,
            scan_matches: Vec::new(),
            scan_scanned: 0,

            bench_ops: String::from("126"),
            bench_batches: String::from("200"),
            bench_result: String::new(),

            selftest: String::new(),
        }
    }
}

impl MemUi {
    pub fn show(&mut self, ui: &mut egui::Ui) {
        ui.heading("内存");
        ui.add_space(2.0);

        ui.horizontal_wrapped(|ui| {
            if self.client.is_some() {
                if ui.button("断开").clicked() {
                    self.disconnect();
                }
            } else if ui.button("连接").clicked() {
                self.connect();
            }

            if let Some(client) = &self.client {
                ui.label(format!(
                    "abi {} / max_ops {} / batch {} KiB / direct map {:#x} / {}",
                    uapp::mem_layout::ABI_VERSION,
                    client.max_ops(),
                    client.max_batch_bytes() / 1024,
                    client.direct_map(),
                    client.info().mode_name()
                ));
            }

            if let Some(os) = &self.os {
                ui.label(format!(
                    "Win build {} / 偏移表 {} [{}]",
                    os.build,
                    os.offsets.build,
                    os.check_summary()
                ));
            }
        });

        ui.label(&self.status);
        if !self.phys_summary.is_empty() {
            ui.label(&self.phys_summary);
        }
        ui.separator();

        self.show_read(ui);
        self.show_write(ui);
        self.show_scan(ui);
        self.show_processes(ui);
        self.show_bench(ui);
        self.show_selftest(ui);
    }

    // ---- 连接 ----------------------------------------------------------

    fn connect(&mut self) {
        let device = match command::Device::open() {
            Ok(device) => device,
            Err(error) => {
                self.status = format!("打开设备失败: {error}");
                return;
            }
        };

        let kargs = device.get_kernel_args().ok();
        let os_info = device.get_os_info().ok();

        let mut client = match MemClient::new(device) {
            Ok(client) => client,
            Err(error) => {
                self.status = format!("内存服务不可用: {error}");
                return;
            }
        };

        let os = match (kargs.as_ref(), os_info.as_ref()) {
            (Some(kargs), Some(info)) => match WinOs::attach(&mut client, kargs.system_cr3, info) {
                Ok(os) => Some(os),
                Err(error) => {
                    self.status = format!("内存已连接；OS 层不可用: {error}");
                    None
                }
            },
            _ => None,
        };

        if let Some(kargs) = &kargs {
            self.cr3 = format!("{:X}", kargs.system_cr3);
        }

        if let Some(os) = &os {
            self.addr = format!("{:X}", os.nt_base);
            self.scan_start = format!("{:X}", os.nt_base);
            self.scan_end = format!("{:X}", os.nt_base + 0x10_0000);
            self.status = String::from("已连接");
        } else if self.status.is_empty() {
            self.status = String::from("已连接（无 OS 引导信息）");
        }

        if let Ok(ranges) = client.phys_map() {
            let total: u64 = ranges.iter().map(|(_, size)| *size).sum();
            self.phys_summary = format!(
                "物理区间 {} 段 / {:.1} GiB",
                ranges.len(),
                total as f64 / (1024.0 * 1024.0 * 1024.0)
            );
        }

        self.client = Some(client);
        self.os = os;
        self.kargs = kargs;
        self.os_info = os_info;
    }

    fn disconnect(&mut self) {
        self.client = None;
        self.os = None;
        self.os_info = None;
        self.processes.clear();
        self.modules.clear();
        self.scan_request = None;
        self.status = String::from("已断开");
    }

    // ---- 读 ------------------------------------------------------------

    fn show_read(&mut self, ui: &mut egui::Ui) {
        ui.label("读取");
        ui.horizontal_wrapped(|ui| {
            ui.label("CR3:");
            ui.add(egui::TextEdit::singleline(&mut self.cr3).desired_width(150.0));
            ui.label("地址:");
            ui.add(egui::TextEdit::singleline(&mut self.addr).desired_width(150.0));
            ui.label("长度:");
            ui.add(egui::TextEdit::singleline(&mut self.len).desired_width(50.0));

            if ui.button("读").clicked() {
                self.read();
            }
        });

        if !self.hexdump.is_empty() {
            scroll_list(ui, "mem_hexdump", 120.0, |ui| {
                ui.monospace(&self.hexdump);
            });
        }

        ui.separator();
    }

    fn read(&mut self) {
        let Some(client) = self.client.as_mut() else {
            self.status = String::from("未连接");
            return;
        };

        let cr3 = match uapp::parse_hex_address(&self.cr3) {
            Ok(value) => value,
            Err(error) => {
                self.status = error;
                return;
            }
        };

        let address = match uapp::parse_hex_address(&self.addr) {
            Ok(value) => value,
            Err(error) => {
                self.status = error;
                return;
            }
        };

        let length = self
            .len
            .trim()
            .parse::<usize>()
            .unwrap_or(16)
            .clamp(1, 4096);

        let mut buffer = vec![0u8; length];

        match client.read_virt(cr3, address, &mut buffer) {
            Ok(()) => {
                self.hexdump = hexdump(address, &buffer);
                self.status = format!("读取 {length} 字节成功");
            }
            Err(error) => self.status = format!("读取失败: {error}"),
        }
    }

    // ---- 写 ------------------------------------------------------------

    fn show_write(&mut self, ui: &mut egui::Ui) {
        ui.label("写入");
        ui.horizontal_wrapped(|ui| {
            ui.label("字节:");
            ui.add(
                egui::TextEdit::singleline(&mut self.write_text)
                    .hint_text("AA BB CC")
                    .desired_width(220.0),
            );
            ui.checkbox(&mut self.allow_write, "允许写入");

            if ui.button("写并读回").clicked() {
                self.write();
            }
        });
    }

    fn write(&mut self) {
        if !self.allow_write {
            self.status = String::from("先勾选“允许写入”");
            return;
        }

        let Some(client) = self.client.as_mut() else {
            self.status = String::from("未连接");
            return;
        };

        let cr3 = match uapp::parse_hex_address(&self.cr3) {
            Ok(value) => value,
            Err(error) => {
                self.status = error;
                return;
            }
        };

        let address = match uapp::parse_hex_address(&self.addr) {
            Ok(value) => value,
            Err(error) => {
                self.status = error;
                return;
            }
        };

        let data = match parse_bytes(&self.write_text) {
            Ok(data) if !data.is_empty() => data,
            Ok(_) => {
                self.status = String::from("写入内容为空");
                return;
            }
            Err(error) => {
                self.status = error;
                return;
            }
        };

        if let Err(error) = client.write_virt(cr3, address, &data) {
            self.status = format!("写入失败: {error}");
            return;
        }

        let mut readback = vec![0u8; data.len()];

        match client.read_virt(cr3, address, &mut readback) {
            Ok(()) if readback == data => {
                self.status = format!("写入 {} 字节，读回一致", data.len());
                self.hexdump = hexdump(address, &readback);
            }
            Ok(()) => self.status = String::from("写入成功但读回不一致"),
            Err(error) => self.status = format!("写入成功，读回失败: {error}"),
        }
    }

    // ---- 扫描 ----------------------------------------------------------

    fn show_scan(&mut self, ui: &mut egui::Ui) {
        ui.label("扫描（分段，单次 ≤ 200 µs）");
        ui.horizontal_wrapped(|ui| {
            ui.label("签名:");
            ui.add(egui::TextEdit::singleline(&mut self.pattern).desired_width(160.0));
            ui.checkbox(&mut self.scan_phys, "物理地址");
            ui.label("起:");
            ui.add(egui::TextEdit::singleline(&mut self.scan_start).desired_width(130.0));
            ui.label("止:");
            ui.add(egui::TextEdit::singleline(&mut self.scan_end).desired_width(130.0));

            if self.scan_request.is_some() {
                if ui.button("停止").clicked() {
                    self.scan_request = None;
                    self.status = String::from("扫描已停止");
                }
            } else if ui.button("开始扫描").clicked() {
                self.start_scan();
            }
        });

        if !self.scan_matches.is_empty() {
            ui.label(format!(
                "命中 {} 个，已扫 {} KiB",
                self.scan_matches.len(),
                self.scan_scanned / 1024
            ));

            let lines: Vec<String> = self
                .scan_matches
                .iter()
                .take(16)
                .map(|address| format!("{address:#018x}"))
                .collect();

            ui.monospace(lines.join("  "));
        }

        self.step_scan(ui);
        ui.separator();
    }

    fn start_scan(&mut self) {
        let Some(client) = self.client.as_mut() else {
            self.status = String::from("未连接");
            return;
        };

        let pattern = match uapp::pattern::Pattern::parse(&self.pattern) {
            Ok(pattern) => pattern,
            Err(error) => {
                self.status = format!("签名非法: {error}");
                return;
            }
        };

        let start = match uapp::parse_hex_address(&self.scan_start) {
            Ok(value) => value,
            Err(error) => {
                self.status = error;
                return;
            }
        };

        let end = match uapp::parse_hex_address(&self.scan_end) {
            Ok(value) => value,
            Err(error) => {
                self.status = error;
                return;
            }
        };

        let request = if self.scan_phys {
            uapp::mem_layout::MemScanRequest::phys(start, end, &pattern)
        } else {
            let cr3 = match uapp::parse_hex_address(&self.cr3) {
                Ok(value) => value,
                Err(error) => {
                    self.status = error;
                    return;
                }
            };

            uapp::mem_layout::MemScanRequest::virt(cr3, start, end, &pattern)
        };

        let mut request = match request {
            Ok(request) => request,
            Err(error) => {
                self.status = error;
                return;
            }
        };

        request.flags |= uapp::mem_layout::SCAN_FLAG_SKIP_INACCESSIBLE;

        self.scan_matches.clear();
        self.scan_scanned = 0;
        self.scan_request = Some(request);
        self.status = format!("扫描 {start:#x}..{end:#x}");

        let _ = client;
    }

    /// 每个 UI 帧推进一次扫描，避免卡住界面。
    fn step_scan(&mut self, ui: &mut egui::Ui) {
        let Some(request) = self.scan_request.as_mut() else {
            return;
        };

        let Some(client) = self.client.as_mut() else {
            self.scan_request = None;
            return;
        };

        match client.scan_step(request) {
            Ok(step) => {
                self.scan_matches.extend_from_slice(&step.matches);
                self.scan_scanned += step.scanned as u64;

                if !step.status.is_ok() {
                    self.status = format!("扫描失败: {}", step.status);
                    self.scan_request = None;
                } else if step.done {
                    self.status = format!(
                        "扫描完成：{} 个命中，{} KiB",
                        self.scan_matches.len(),
                        self.scan_scanned / 1024
                    );
                    self.scan_request = None;
                }
            }
            Err(error) => {
                self.status = format!("扫描失败: {error}");
                self.scan_request = None;
            }
        }

        ui.ctx().request_repaint();
    }

    // ---- 进程 / 模块 ---------------------------------------------------

    fn show_processes(&mut self, ui: &mut egui::Ui) {
        ui.label("进程 / 模块");
        ui.horizontal_wrapped(|ui| {
            if ui.button("刷新进程").clicked() {
                self.refresh_processes();
            }

            if ui.button("内核模块").clicked() {
                self.load_kernel_modules();
            }

            if ui.button("选中进程的模块").clicked() {
                self.load_modules();
            }

            if let Some(index) = self.selected_process {
                if let Some(process) = self.processes.get(index) {
                    ui.label(format!(
                        "{}({}) EPROCESS {:#x} DTB {:#x}",
                        process.name, process.pid, process.eprocess, process.dtb
                    ));
                }
            }
        });

        if !self.processes.is_empty() {
            scroll_list(ui, "mem_process_list", 160.0, |ui| {
                    let mut clicked = None;

                    for (index, process) in self.processes.iter().enumerate() {
                        let label = format!(
                            "{:>5}  {:<16} {:#018x}  {:#x}",
                            process.pid, process.name, process.eprocess, process.dtb
                        );

                        if ui
                            .selectable_label(self.selected_process == Some(index), label)
                            .clicked()
                        {
                            clicked = Some(index);
                        }
                    }

                    if let Some(index) = clicked {
                        self.selected_process = Some(index);

                        if let Some(process) = self.processes.get(index) {
                            self.cr3 = format!("{:X}", process.dtb);
                        }
                    }
                });
        }

        if !self.modules.is_empty() {
            scroll_list(ui, "mem_module_list", 140.0, |ui| {
                    for module in &self.modules {
                        ui.monospace(format!(
                            "{:#018x} {:>8}  {:08x}  {}",
                            module.base, module.size, module.time_date_stamp, module.name
                        ));
                    }
                });
        }

        ui.separator();
    }

    fn refresh_processes(&mut self) {
        let (Some(client), Some(os)) = (self.client.as_mut(), self.os.as_ref()) else {
            self.status = String::from("OS 层未就绪");
            return;
        };

        match os.processes(client) {
            Ok(processes) => {
                self.status = format!("{} 个进程", processes.len());
                self.processes = processes;
            }
            Err(error) => self.status = format!("枚举进程失败: {error}"),
        }
    }

    fn load_modules(&mut self) {
        let Some(index) = self.selected_process else {
            self.status = String::from("先选中一个进程");
            return;
        };

        let (Some(client), Some(os)) = (self.client.as_mut(), self.os.as_ref()) else {
            self.status = String::from("OS 层未就绪");
            return;
        };

        let Some(process) = self.processes.get(index).cloned() else {
            return;
        };

        match os.modules(client, &process) {
            Ok(modules) => {
                self.status = format!("{} 的模块 {} 个", process.name, modules.len());
                self.modules = modules;
            }
            Err(error) => self.status = format!("枚举模块失败: {error}"),
        }
    }

    fn load_kernel_modules(&mut self) {
        let (Some(client), Some(os)) = (self.client.as_mut(), self.os.as_ref()) else {
            self.status = String::from("OS 层未就绪");
            return;
        };

        match os.kernel_modules(client) {
            Ok(modules) => {
                self.status = format!("内核模块 {} 个", modules.len());
                self.modules = modules;
            }
            Err(error) => self.status = format!("枚举内核模块失败: {error}"),
        }
    }

    // ---- 测速 ----------------------------------------------------------

    fn show_bench(&mut self, ui: &mut egui::Ui) {
        ui.label("测速");
        ui.horizontal_wrapped(|ui| {
            ui.label("每条批:");
            ui.add(egui::TextEdit::singleline(&mut self.bench_ops).desired_width(50.0));
            ui.label("批数:");
            ui.add(egui::TextEdit::singleline(&mut self.bench_batches).desired_width(60.0));

            if ui.button("批量读测速").clicked() {
                self.bench();
            }

            if ui.button("对比单条").clicked() {
                self.bench_single();
            }
        });

        if !self.bench_result.is_empty() {
            ui.monospace(&self.bench_result);
        }

        ui.separator();
    }

    fn bench_config(&self) -> Option<BenchConfig> {
        let os = self.os.as_ref()?;

        Some(BenchConfig {
            batch_ops: self.bench_ops.trim().parse().unwrap_or(126),
            batches: self.bench_batches.trim().parse().unwrap_or(200),
            len: 8,
            cr3: os.system_cr3,
            va: os.nt_base,
        })
    }

    fn bench(&mut self) {
        let Some(config) = self.bench_config() else {
            self.status = String::from("OS 层未就绪，无法确定测速目标");
            return;
        };

        let Some(client) = self.client.as_mut() else {
            self.status = String::from("未连接");
            return;
        };

        match client.bench_read(&config) {
            Ok(report) => {
                self.bench_result = format!(
                    "{} 条/批 × {} 批：{}\n总耗时 {:.2} ms，载荷 {:.1} MiB",
                    config.batch_ops,
                    config.batches,
                    report.summary(),
                    report.elapsed.as_secs_f64() * 1000.0,
                    report.bytes as f64 / (1024.0 * 1024.0)
                );
            }
            Err(error) => self.status = format!("测速失败: {error}"),
        }
    }

    fn bench_single(&mut self) {
        let Some(config) = self.bench_config() else {
            self.status = String::from("OS 层未就绪，无法确定测速目标");
            return;
        };

        let Some(client) = self.client.as_mut() else {
            self.status = String::from("未连接");
            return;
        };

        match client.bench_single(&config) {
            Ok(report) => {
                self.bench_result = format!(
                    "单条 × {} 批：{}\n（对比：同批数下 126 条/批的 amortized 成本）",
                    config.batches,
                    report.summary()
                );
            }
            Err(error) => self.status = format!("测速失败: {error}"),
        }
    }

    // ---- 自检 ----------------------------------------------------------

    fn show_selftest(&mut self, ui: &mut egui::Ui) {
        ui.label("自检");
        ui.horizontal_wrapped(|ui| {
            if ui.button("运行自检").clicked() {
                self.selftest();
            }

            if !self.selftest.is_empty() && ui.button("清空").clicked() {
                self.selftest.clear();
            }
        });

        if !self.selftest.is_empty() {
            scroll_list(ui, "mem_selftest", 200.0, |ui| {
                ui.monospace(&self.selftest);
            });
        }
    }

    fn selftest(&mut self) {
        let (Some(client), Some(kargs), Some(info)) = (
            self.client.as_mut(),
            self.kargs.as_ref(),
            self.os_info.as_ref(),
        ) else {
            self.status = String::from("未连接，或驱动没有提供 OS 引导信息");
            return;
        };

        let steps = mem_test::run(client, kargs.system_cr3, info);
        let passed = steps.iter().filter(|step| step.ok).count();
        self.status = format!("自检 {passed}/{} 通过", steps.len());
        self.selftest = mem_test::format_report(&steps);
    }
}

/// 在外层 `ScrollArea` 里放一个**有界**的滚动列表。
///
/// 直接嵌套 `ScrollArea` 时，内层拿到的 `available_height` 是外层滚动区的
/// “无限高度”，viewport 不成立，滚轮事件落不到列表上（表现为列表不能滚）。
/// `allocate_ui_with_layout` 先把子区域钉成固定高度，滚动区才有真实 viewport。
fn scroll_list<R>(
    ui: &mut egui::Ui,
    id: &str,
    height: f32,
    add_contents: impl FnOnce(&mut egui::Ui) -> R,
) -> R {
    ui.allocate_ui_with_layout(
        egui::vec2(ui.available_width(), height),
        egui::Layout::top_down(egui::Align::Min),
        |ui| {
            egui::ScrollArea::vertical()
                .id_salt(id)
                .auto_shrink([false, false])
                .show(ui, add_contents)
                .inner
        },
    )
    .inner
}

/// hexdump：`地址  16 字节  ASCII`。
fn hexdump(base: u64, bytes: &[u8]) -> String {
    let mut out = String::new();

    for (row, chunk) in bytes.chunks(16).enumerate() {
        let address = base.wrapping_add((row * 16) as u64);
        out.push_str(&format!("{address:#018x}  "));

        for index in 0..16 {
            match chunk.get(index) {
                Some(byte) => out.push_str(&format!("{byte:02x} ")),
                None => out.push_str("   "),
            }

            if index == 7 {
                out.push(' ');
            }
        }

        out.push(' ');

        for byte in chunk {
            out.push(if byte.is_ascii_graphic() || *byte == b' ' {
                *byte as char
            } else {
                '.'
            });
        }

        out.push('\n');
    }

    out
}

/// `AA BB CC` / `AABBCC` → 字节。
fn parse_bytes(text: &str) -> Result<Vec<u8>, String> {
    let compact: String = text
        .chars()
        .filter(|character| !character.is_whitespace() && *character != ',')
        .collect();

    if compact.len() % 2 != 0 {
        return Err(String::from("字节串长度必须是偶数"));
    }

    let mut out = Vec::with_capacity(compact.len() / 2);

    for index in (0..compact.len()).step_by(2) {
        let pair = &compact[index..index + 2];
        out.push(u8::from_str_radix(pair, 16).map_err(|_| format!("非法字节: {pair}"))?);
    }

    Ok(out)
}
