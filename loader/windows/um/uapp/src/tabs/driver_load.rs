//! 环境 tab：驱动加载/卸载、hypervisor 启停、设备工具、内核参数。

use eframe::egui;

use crate::{MyApp, utils};

pub fn show(app: &mut MyApp, ui: &mut egui::Ui) {
    ui.heading("驱动");
    ui.add_space(2.0);

    ui.horizontal(|ui| {
        ui.label("服务名:");
        ui.text_edit_singleline(&mut app.drv_name);
    });

    ui.horizontal(|ui| {
        ui.label("驱动路径:");
        ui.text_edit_singleline(&mut app.drv_path);

        if ui.button("打开").clicked()
            && let Some(path) = utils::pick_driver_file()
        {
            app.drv_name = utils::service_name_from_path(&path);
            app.drv_path = path.to_string_lossy().into_owned();
            app.status = format!("已选择 {}", path.display());
        }
    });

    ui.horizontal(|ui| {
        if ui.button("加载驱动").clicked() {
            let result = utils::nt_load_driver(&app.drv_name, &app.drv_path);
            app.reset_device();
            app.status = match result {
                Ok(()) => format!("已加载服务 {}", app.drv_name),
                Err(err) => err,
            };
        }

        if ui.button("卸载驱动").clicked() {
            let result = utils::nt_unload_driver(&app.drv_name);
            app.reset_device();
            app.status = match result {
                Ok(()) => format!("已卸载服务 {}", app.drv_name),
                Err(err) => err,
            };
        }
    });

    ui.separator();
    ui.heading("Hypervisor");
    ui.add_space(2.0);

    ui.horizontal(|ui| {
        ui.label("虚拟机:");
        ui.text_edit_singleline(&mut app.image_path);

        if ui.button("设置").clicked() {
            app.status = app.set_image_path();
        }
    });

    ui.horizontal(|ui| {
        ui.label("日志路径:");
        ui.text_edit_singleline(&mut app.log_path);

        if ui.button("设置").clicked() {
            app.status = app.set_log_path();
        }
    });

    ui.horizontal_wrapped(|ui| {
        if ui.button("Hypervisor Start").clicked() {
            app.status = app.start_hypervisor();
        }

        if ui.button("Start (Single Core)").clicked() {
            app.status = app.start_hypervisor_single_core();
        }

        if ui.button("Hypervisor Stop").clicked() {
            app.status = app.stop_hypervisor();
        }
    });

    ui.add_space(4.0);

    // WinOffsets：Start 前下发（进 KernelArgs）或运行时用 hypercall 覆盖。
    ui.horizontal_wrapped(|ui| {
        ui.checkbox(&mut app.push_offsets_on_start, "Start 时传入 WinOffsets")
            .on_hover_text(
                "勾选：Start 之前把按当前 build 选出的内核结构偏移表下发给驱动，\
                 随 KernelArgs 一起进 hypervisor。\n\
                 不勾选：KernelArgs 里 build = 0，依赖偏移的进程检查（TSC 隐藏按 PID）不生效。",
            );

        if ui.button("按版本选表").clicked() {
            app.status = app.refresh_win_offsets();
        }

        if ui.button("Hypercall 设置 WinOffsets").clicked() {
            app.status = app.hypercall_set_win_offsets();
        }
    });

    if !app.win_offsets_status.is_empty() {
        ui.label(&app.win_offsets_status);
    }

    ui.separator();
    ui.heading("设备 / 工具");
    ui.add_space(2.0);

    ui.horizontal_wrapped(|ui| {
        if ui.button("连接设备").clicked() {
            app.status = app.open_device();
        }

        if ui.button("Driver Ping").clicked() {
            app.status = app.ping_driver();
        }

        if ui.button("获取信息").clicked() {
            app.status = app.refresh_info();
        }

        if ui.button("内核参数").clicked() {
            app.status = app.refresh_kernel_args();
        }

        if ui.button("Hypercall Ping").clicked() {
            app.status = app.hypercall_ping();
        }

        if ui.button("Ping (单核)").clicked() {
            app.status = app.hypercall_ping_single_core();
        }
    });

    if let Some(info) = &app.info {
        ui.add_space(4.0);
        ui.label(format!(
            "模式: {}    状态: {}    核心: {}    启动核心: {}",
            info.mode_name(),
            info.state_name(),
            info.cpu_count,
            info.start_processor
        ));
        ui.label(format!(
            "KernelBase: 0x{:016X}    入口: 0x{:016X}",
            info.kernel_base, info.entry_point
        ));
        ui.label(format!(
            "HypercallKey: 0x{:X}    日志大小: {}    LastStatus: 0x{:08X}",
            info.hypercall_key, info.log_size, info.last_status as u32
        ));
    }

    if let Some(args) = &app.kernel_args {
        ui.add_space(4.0);
        ui.label(format!(
            "SystemCr3 0x{:016X}    内核 0x{:016X}/0x{:X}    核心 {}",
            args.system_cr3, args.kernel_base, args.kernel_size, args.cpu_count
        ));
        ui.label(format!(
            "KernelBase 0x{:016X}    Log 0x{:016X}",
            args.kernel_base, args.log_base
        ));
    }
}
