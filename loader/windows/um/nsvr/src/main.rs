//! nsvr：kdmapper 版的用户态前端。
//!
//! 没有驱动、没有设备对象，**所有操作都只通过 CPL3 的 `vmcall` 进 hypervisor**。
//! 单页界面，没有 tab。

#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

mod stealth;
mod vmcall;

/// 构建期生成的默认 hypercall key（见 `build.rs`）。
mod generated {
    include!(concat!(env!("OUT_DIR"), "/hypercall_key.rs"));
}

use eframe::egui;

fn main() -> eframe::Result<()> {
    let options = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default().with_inner_size([720.0, 720.0]),
        ..Default::default()
    };

    eframe::run_native(
        "nsvr",
        options,
        Box::new(|_| Ok(Box::<App>::default())),
    )
}
struct App {
    // hypercall
    key: String,
    output: String,

    // TSC
    target_pid: String,
    target_mode: u32,
    divisor: String,
    overhead: String,
    hit_log: bool,

    // MEM
    mem_addr: String,
    mem_len: String,

    // 隐蔽性
    stealth: Vec<String>,
}

impl Default for App {
    fn default() -> Self {
        Self {
            key: String::from(generated::DEFAULT_KEY),
            output: String::new(),
            target_pid: String::new(),
            target_mode: 2,
            divisor: String::from("10"),
            overhead: String::from("0"),
            hit_log: true,
            mem_addr: String::new(),
            mem_len: String::from("16"),
            stealth: Vec::new(),
        }
    }
}

impl App {
    fn key_value(&self) -> u64 {
        parse_u64(&self.key).unwrap_or(0)
    }

    fn call(&mut self, command: u64, a0: u64, a1: u64, a2: u64) {
        let key = self.key_value();
        // SAFETY: 只有在 hypervisor 已接管当前 CPU 时才允许 VMCALL；key 错误会 #UD。
        let rax = unsafe { vmcall::vmcall(key, command, a0, a1, a2) };

        self.output = format!("cmd={command:#x} arg0={a0:#x} -> RAX={rax}");
    }

    fn ui_hypercall(&mut self, ui: &mut egui::Ui) {
        ui.heading("Hypercall");
        ui.horizontal(|ui| {
            ui.label("key:");
            ui.add(egui::TextEdit::singleline(&mut self.key).desired_width(160.0));
            ui.label(format!(
                "(默认 {}，构建参数 --hypercall-key 可改)",
                generated::DEFAULT_KEY
            ));
        });
        ui.label(
            egui::RichText::new("key 错误时 hypervisor 注入 #UD，本进程会直接崩溃。")
                .small()
                .weak(),
        );
    }

    fn ui_stealth(&mut self, ui: &mut egui::Ui) {
        ui.separator();
        ui.heading("隐蔽性自测");

        ui.horizontal_wrapped(|ui| {
            if ui.button("CPUID / VMX 标志").clicked() {
                self.stealth = stealth::cpuid_report();
            }
            if ui.button("RDTSC 差值").clicked() {
                self.stealth = stealth::rdtsc_report();
            }
        });

        for line in &self.stealth {
            ui.label(line);
        }
    }

    fn ui_tsc(&mut self, ui: &mut egui::Ui) {
        ui.separator();
        ui.heading("TSC");

        ui.horizontal(|ui| {
            ui.label("目标 PID:");
            ui.add(egui::TextEdit::singleline(&mut self.target_pid).desired_width(100.0));
            ui.label("模式:");
            for (mode, name) in [(0u32, "内核"), (1, "用户"), (2, "两者")] {
                ui.radio_value(&mut self.target_mode, mode, name);
            }
            if ui.button("设置目标").clicked() {
                let pid = parse_u64(&self.target_pid).unwrap_or(0) as u64;

                self.call(vmcall::TSC_SET_TARGET_PID, pid, self.target_mode as u64, 0);
            }
        });

        ui.horizontal(|ui| {
            ui.label("除数 N:");
            ui.add(egui::TextEdit::singleline(&mut self.divisor).desired_width(80.0));
            if ui.button("应用除数").clicked() {
                let n = parse_u64(&self.divisor).unwrap_or(10);

                self.call(vmcall::TSC_SET_DIVISOR, n, 0, 0);
            }

            ui.label("补偿 tick:");
            ui.add(egui::TextEdit::singleline(&mut self.overhead).desired_width(80.0));
            if ui.button("应用补偿").clicked() {
                let ticks = parse_u64(&self.overhead).unwrap_or(0);

                self.call(vmcall::TSC_SET_EXIT_OVERHEAD, ticks, 0, 0);
            }
        });

        ui.horizontal(|ui| {
            if ui.checkbox(&mut self.hit_log, "命中日志").changed() {
                let enabled = u64::from(self.hit_log);

                self.call(vmcall::TSC_SET_HIT_LOG, enabled, 0, 0);
            }
            ui.label(
                egui::RichText::new("一条命中日志 1~2 万 tick，测精度时关掉。")
                    .small()
                    .weak(),
            );
        });
    }

    fn ui_mem(&mut self, ui: &mut egui::Ui) {
        ui.separator();
        ui.heading("内存");

        ui.horizontal(|ui| {
            ui.label("地址:");
            ui.add(egui::TextEdit::singleline(&mut self.mem_addr).desired_width(160.0));
            ui.label("长度:");
            ui.add(egui::TextEdit::singleline(&mut self.mem_len).desired_width(60.0));

            if ui.button("读").clicked() {
                let addr = parse_u64(&self.mem_addr).unwrap_or(0);
                let len = parse_u64(&self.mem_len).unwrap_or(16).min(0x1000) as u32;
                let (rax, status) = vmcall::mem_single(
                    self.key_value(),
                    vmcall::OP_READ_VIRT,
                    addr,
                    len,
                    None,
                );

                self.output = format!("MEM_BATCH read {addr:#x}+{len:#x} -> RAX={rax} status={status}");
            }

            if ui.button("查询 ABI").clicked() {
                let rax = vmcall::mem_query(self.key_value());

                self.output = format!("MEM_QUERY -> RAX={rax}");
            }

            if ui.button("写 0").clicked() {
                let addr = parse_u64(&self.mem_addr).unwrap_or(0);
                let len = parse_u64(&self.mem_len).unwrap_or(16).min(0x1000) as u32;
                let zeros = vec![0u8; len as usize];
                let (rax, status) = vmcall::mem_single(
                    self.key_value(),
                    vmcall::OP_WRITE_VIRT,
                    addr,
                    len,
                    Some(&zeros),
                );

                self.output = format!("MEM_BATCH write {addr:#x}+{len:#x} -> RAX={rax} status={status}");
            }
        });
    }
}

impl eframe::App for App {
    fn ui(&mut self, ui: &mut egui::Ui, _frame: &mut eframe::Frame) {
        egui::ScrollArea::vertical().show(ui, |ui| {
            self.ui_hypercall(ui);
            self.ui_stealth(ui);
            self.ui_tsc(ui);
            self.ui_mem(ui);

            ui.separator();
            ui.heading("结果");
            ui.monospace(&self.output);
        });
    }
}

fn parse_u64(text: &str) -> Result<u64, String> {
    let trimmed = text.trim();

    if trimmed.is_empty() {
        return Err(String::from("empty"));
    }

    if let Some(hex) = trimmed.strip_prefix("0x").or_else(|| trimmed.strip_prefix("0X")) {
        u64::from_str_radix(hex, 16).map_err(|e| e.to_string())
    } else {
        trimmed.parse::<u64>().map_err(|e| e.to_string())
    }
}
