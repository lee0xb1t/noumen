//! TSC 选择性隐藏面板。
//!
//! 目标配置是**全局**的：hypervisor 打开 `RDTSC_EXITING` 后，每条 `RDTSC` 都会
//! 退出，退出处理里再按当前进程/模块决定要不要扣掉 host 时间。所以这里只是
//! 下发/清除目标，没有“每核单独设置”的概念。

use eframe::egui;

use crate::{hypercall, MyApp};

pub fn show(app: &mut MyApp, ui: &mut egui::Ui) {
    ui.heading("TSC 选择性隐藏");
    ui.add_space(4.0);
    ui.monospace("只对指定进程/内核模块隐藏 VM-exit 开销；其它进程读 RDTSC 原样返回物理值。");

    ui.add_space(8.0);
    ui.separator();

    // ---- 进程 ----
    ui.heading("目标进程");
    ui.horizontal(|ui| {
        ui.label("PID:");
        ui.add(
            egui::TextEdit::singleline(&mut app.tsc_pid)
                .desired_width(90.0)
                .hint_text("0"),
        );

        if ui.button("设置").clicked() {
            app.status = app.tsc_set_pid();
        }

        if ui.button("清除").clicked() {
            app.tsc_pid = String::from("0");
            app.status = app.tsc_set_pid();
        }
    });

    ui.label(
        egui::RichText::new("CPL3 和 CPL0 是否参与隐藏由“模式”决定；读不到进程号时按不匹配处理。")
            .weak()
            .small(),
    );

    ui.horizontal(|ui| {
        ui.label("模式:");
        ui.radio_value(&mut app.tsc_mode, hypercall::TSC_MODE_KERNEL, "内核");
        ui.radio_value(&mut app.tsc_mode, hypercall::TSC_MODE_USER, "用户");
        ui.radio_value(&mut app.tsc_mode, hypercall::TSC_MODE_BOTH, "内核+用户");
        ui.label(
            egui::RichText::new("（改模式后点上面的“设置”重新下发）")
                .weak()
                .small(),
        );
    });

    ui.add_space(8.0);
    ui.separator();

    // ---- 内核模块 ----
    ui.heading("目标内核模块");
    ui.horizontal(|ui| {
        ui.label("基址:");
        ui.add(
            egui::TextEdit::singleline(&mut app.tsc_module_base)
                .desired_width(160.0)
                .hint_text("0xFFFFF80600000000"),
        );
    });
    ui.horizontal(|ui| {
        ui.label("大小:");
        ui.add(
            egui::TextEdit::singleline(&mut app.tsc_module_size)
                .desired_width(160.0)
                .hint_text("0x200000"),
        );

        if ui.button("设置").clicked() {
            app.status = app.tsc_set_module();
        }

        if ui.button("清除").clicked() {
            app.tsc_module_base.clear();
            app.tsc_module_size.clear();
            app.status = app.tsc_set_module();
        }
    });

    ui.label(
        egui::RichText::new("只在 CPL0 生效：RIP 落在 [基址, 基址+大小) 时隐藏。")
            .weak()
            .small(),
    );

    ui.add_space(8.0);
    ui.separator();

    // ---- 退出补偿 ----
    ui.heading("退出补偿");
    ui.horizontal(|ui| {
        ui.label("每次命中退出:");
        ui.add(
            egui::TextEdit::singleline(&mut app.tsc_exit_overhead)
                .desired_width(120.0)
                .hint_text("0"),
        );
        ui.label("tick");

        if ui.button("设置").clicked() {
            app.status = app.tsc_set_exit_overhead();
        }

        if ui.button("关闭").clicked() {
            app.tsc_exit_overhead = String::from("0");
            app.status = app.tsc_set_exit_overhead();
        }
    });

    ui.label(
        egui::RichText::new(
            "账本测不到 guest→root 转换本身，每次退出都会漏一截，残差 ≈ 两次退出转换之和。\n\
             填「目标进程实测残差 / 2」：先设好目标 PID，看目标里 rdtsc;cpuid;rdtsc 的平均值，\n\
             它半值就是这里要填的数；填对了目标量到的差值会回到 0 附近。\n\
             别填过头：补得比实际转换多会让目标的 TSC 回退（日志 rewind= 不为 0）。",
        )
        .weak()
        .small(),
    );

    ui.add_space(8.0);
    ui.separator();

    // ---- 状态 ----
    ui.heading("状态");
    if app.tsc_status.is_empty() {
        ui.monospace("（还没有下发过配置）");
    } else {
        ui.monospace(&app.tsc_status);
    }

    ui.add_space(4.0);
    ui.label(
        egui::RichText::new(
            "提示：\n\
             - 打开隐藏后 RDTSC 全系统都会 exit，忙等循环会明显变慢；不需要时点“清除”。\n\
             - 偏移表由“环境”页在 Start 之前下发（按 Windows build 选表）。\n\
             - 清除进程和模块后，hypervisor 会关掉 rdtsc_exiting，恢复原生性能。\n\
             - 日志 `TSC: hidden rdtsc ... since=N` 的 `since` 就是连续两次隐藏读的差值，\n\
               也就是目标自己量到的那个数；`exits`/`matched` 说明这两次读之间发生了几次退出、\n\
               其中几次被判成目标（正常应为 2 / 2）。",
        )
        .weak()
        .small(),
    );
}
