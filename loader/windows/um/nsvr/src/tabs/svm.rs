//! SVM/NPT tab：SVM hook + NtCreateFile 日志。

use eframe::egui;

use crate::{MyApp, command};

pub fn show(app: &mut MyApp, ui: &mut egui::Ui) {
    ui.heading("SVM / NPT Hook");
    ui.add_space(2.0);
    ui.label("NPT 没有 execute-only，调试方案决定 trap 来源：");

    ui.horizontal(|ui| {
        ui.label("目标地址:");
        ui.text_edit_singleline(&mut app.svm_address);
    });

    ui.horizontal(|ui| {
        ui.label("进程 PID (0=内核):");
        ui.text_edit_singleline(&mut app.svm_pid);
    });

    ui.horizontal_wrapped(|ui| {
        ui.radio_value(
            &mut app.svm_method,
            command::SvmHookRequest::METHOD_BYTE_PATCH,
            "NPT 页 Hook（过渡: byte-patch）",
        );
    });

    ui.label(
        "SVM 不再提供 VMX 式调试断点/DR0/TF 单步；函数级 trampoline 见下面的 NtCreateFile hook。",
    );

    ui.horizontal_wrapped(|ui| {
        ui.label(
            "SVM/NPT 的通用页 hook 只支持用户断点（CPL3）；内核函数 hook 只提供 NtCreateFile。",
        );
    });

    ui.horizontal_wrapped(|ui| {
        if ui.button("安装 SVM hook").clicked() {
            app.status = app.svm_hook(false);
        }

        if ui.button("移除 SVM hook").clicked() {
            app.status = app.svm_hook(true);
        }

        ui.label("NPT 页 Hook：目前过渡为 byte-patch；函数 hook 见下面的 NtCreateFile");
    });

    ntcf_show(app, ui);
}

fn ntcf_show(app: &mut MyApp, ui: &mut egui::Ui) {
    ui.separator();
    ui.heading("NtCreateFile Hook (NPT)");
    ui.add_space(2.0);
    ui.label(
        "SVM：影子页 INT3 + trampoline（AMD）。VMX 的 EPT proxy hook 也写同一份\
         驱动日志，可以用这里的「读取日志」查看。\
         真实页不改；handler 记录 ObjectName/DesiredAccess 等参数。",
    );

    ui.horizontal_wrapped(|ui| {
        if ui.button("安装 Hook").clicked() {
            // 重新安装前先清掉上一轮的表格，避免把旧记录当成本次命中。
            app.ntcf_logs.clear();
            app.ntcf_total = 0;
            app.status = app.ntcf_hook(false);
        }

        if ui.button("移除 Hook").clicked() {
            app.status = app.ntcf_hook(true);
        }

        if ui.button("读取日志").clicked() {
            app.status = app.ntcf_read_log();
        }

        ui.checkbox(&mut app.ntcf_auto, "自动刷新（500 ms）");
    });

    // 自动刷新：只在面板可见时轮询，避免后台空转。
    if app.ntcf_auto {
        let due = app
            .ntcf_last_poll
            .map(|last| last.elapsed() >= std::time::Duration::from_millis(500))
            .unwrap_or(true);

        if due {
            app.ntcf_last_poll = Some(std::time::Instant::now());

            if let Err(err) = app.ntcf_refresh_log() {
                app.ntcf_auto = false;
                app.status = format!("自动刷新已停止: {err}");
            }
        }

        ui.ctx()
            .request_repaint_after(std::time::Duration::from_millis(500));
    }

    ui.label(format!(
        "状态: {}    NtCreateFile 0x{:016X}    trampoline 0x{:016X}    累计命中 {}    \
     缓存 {} 条",
        if app.ntcf_installed {
            "已安装"
        } else {
            "未安装"
        },
        app.ntcf_address,
        app.ntcf_trampoline,
        app.ntcf_total,
        app.ntcf_logs.len()
    ));

    ui.horizontal(|ui| {
        ui.label("过滤名字:");
        ui.text_edit_singleline(&mut app.ntcf_filter);

        if ui.button("清空").clicked() {
            app.ntcf_filter.clear();
        }
    });

    let filter = app.ntcf_filter.to_lowercase();
    let shown: Vec<&command::NtCreateFileLog> = app
        .ntcf_logs
        .iter()
        .filter(|log| filter.is_empty() || log.name().to_lowercase().contains(&filter))
        .collect();

    ui.label(format!(
        "表格显示 {} 条（最新在上；命中序号断号表示中间的记录已被环形缓冲覆盖）",
        shown.len()
    ));

    egui::ScrollArea::vertical()
        .max_height(300.0)
        .id_salt("ntcf_log_scroll")
        .show(ui, |ui| {
            egui::Grid::new("ntcf_log_grid")
                .striped(true)
                .num_columns(7)
                .show(ui, |ui| {
                    ui.strong("#");
                    ui.strong("PID/TID");
                    ui.strong("Disposition");
                    ui.strong("DesiredAccess");
                    ui.strong("Options");
                    ui.strong("Share/Access");
                    ui.strong("ObjectName");
                    ui.end_row();

                    for log in shown.iter().rev() {
                        ui.label(format!("{}", log.sequence));
                        ui.label(format!("{}/{}", log.process_id, log.thread_id));
                        ui.label(log.disposition_name());

                        let access = ui.label(format!("0x{:08X}", log.desired_access));
                        let names = log.desired_access_names();

                        if !names.is_empty() {
                            access.on_hover_text(names.join("\n"));
                        }

                        ui.label(format!("0x{:08X}", log.create_options));
                        ui.label(format!(
                            "0x{:X}/0x{:X}",
                            log.share_access, log.file_attributes
                        ));

                        let name = log.name();
                        let name = if name.is_empty() {
                            String::from("<无名字>")
                        } else {
                            name
                        };
                        let name = if log.truncated() {
                            format!("{name}…")
                        } else {
                            name
                        };

                        let label = ui.label(name);

                        if log.faulted() {
                            label.on_hover_text(
                                "读调用者的 ObjectName 失败（缺页/坏指针），名字为空",
                            );
                        }

                        ui.end_row();
                    }
                });
        });

    if app.ntcf_logs.is_empty() {
        ui.label("没有记录：安装 hook 后打开任意文件（资源管理器切目录也算）再点「读取日志」。");
    }
}
