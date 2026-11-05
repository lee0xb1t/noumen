//! VMX/EPT tab：NtCreateFile proxy、通用 EPT hook、用户断点等待、HideSelf。

use eframe::egui;

use crate::{MyApp, command, hypercall};

pub fn show(app: &mut MyApp, ui: &mut egui::Ui) {
    ui.heading("EPT Hook / Breakpoint");
    ui.add_space(2.0);

    ui.heading("NtCreateFile (EPT)");
    ui.add_space(2.0);

    ui.horizontal_wrapped(|ui| {
        if ui.button("Hook NtCreateFile (DetourInternal)").clicked() {
            app.status = app.ept_ntcf_hook(false, false);
        }

        if ui.button("Hook NtCreateFile (DetourTrampoline)").clicked() {
            app.status = app.ept_ntcf_hook(true, false);
        }
    });

    ui.horizontal_wrapped(|ui| {
        if ui.button("移除 NtCreateFile (DetourInternal)").clicked() {
            app.status = app.ept_ntcf_hook(false, true);
        }

        if ui.button("移除 NtCreateFile (DetourTrampoline)").clicked() {
            app.status = app.ept_ntcf_hook(true, true);
        }
    });

    ui.label(
        "DetourInternal = hypervisor 内部 trampoline（VMCall 上报，不记录参数）；\
         DetourTrampoline = 驱动自带 NtCreateFile proxy + origin 页，\
         不需要填 handler VA。",
    );

    if ui.button("读取 NtCreateFile 日志").clicked() {
        app.status = app.ntcf_read_log();
    }

    ui.separator();

    ui.horizontal(|ui| {
        ui.label("目标地址:");
        ui.text_edit_singleline(&mut app.hook_address);
    });

    ui.horizontal(|ui| {
        ui.label("进程 PID (0=内核):");
        ui.text_edit_singleline(&mut app.hook_pid);
    });

    ui.horizontal_wrapped(|ui| {
        ui.radio_value(
            &mut app.hook_kind,
            command::HOOK_KIND_USER_BREAKPOINT,
            "用户断点 (INT3 等待)",
        );
        ui.radio_value(
            &mut app.hook_kind,
            command::HOOK_KIND_DETOUR_INTERNAL,
            "钩子 (trampoline)",
        );
        ui.radio_value(&mut app.hook_kind, command::HOOK_KIND_HIDE, "隐藏页");
    });

    ui.horizontal(|ui| {
        if ui.button("安装 EPT hook").clicked() {
            app.status = app.ept_hook(false);
        }

        if ui.button("移除 EPT hook").clicked() {
            app.status = app.ept_hook(true);
        }

        // 只对用户断点生效：第一次命中被继续后禁用。
        ui.checkbox(&mut app.hook_one_shot, "一次性（命中后禁用）");
    });

    ui.separator();
    ui.heading("用户断点等待");
    ui.add_space(2.0);

    ui.horizontal_wrapped(|ui| {
        ui.label("超时 ms (-1=无限):");
        ui.text_edit_singleline(&mut app.wait_timeout);

        if ui.button("设置超时").clicked() {
            app.status = app.set_wait_timeout();
        }
    });

    ui.horizontal_wrapped(|ui| {
        ui.label("线程过滤 ETHREAD (0=清除):");
        ui.text_edit_singleline(&mut app.wait_filter);

        if ui.button("设置").clicked() {
            app.status = app.set_wait_filter();
        }

        if ui.button("清除").clicked() {
            app.wait_filter.clear();
            app.status = app.set_wait_filter();
        }

        if ui.button("用最新线程").clicked() {
            match app.waits.last().copied() {
                Some(wait) => {
                    app.wait_filter = format!("0x{:X}", wait.thread);
                    app.status = format!("已填入 ETHREAD 0x{:X}；点“设置”让过滤生效", wait.thread);
                }
                None => app.status = String::from("没有等待项"),
            }
        }
    });

    ui.horizontal_wrapped(|ui| {
        if ui.button("读取等待项").clicked() {
            app.status = app.poll_waits(None);
        }

        if ui.button("全部 Continue").clicked() {
            app.status = app.poll_waits(Some(hypercall::WAIT_ACTION_CONTINUE));
        }

        // 单步一条指令，停在下一条；hypervisor 会在下一行补一次性断点。
        if ui.button("单步（下一行）").clicked() {
            app.status = app.single_step();
        }

        // 滞留的“已应答”等待项：线程已经离开断点，应答永远没人
        // 消费，会一直占着槽位。这里给它一个出口。
        if ui.button("取消最新").clicked() {
            match app.waits.last().copied() {
                Some(wait) => app.status = app.cancel_wait(wait.seq),
                None => app.status = String::from("没有等待项"),
            }
        }

        if ui.button("全部取消").clicked() {
            app.status = app.cancel_wait(0);
        }

        // 自动应答做成开关，按钮本身显示状态，避免误点“启动”后静默放行。
        let auto_running = app.wait_worker.is_some();

        let label = if auto_running {
            "自动应答：开（点此停止）"
        } else {
            "自动应答：关（点此启动）"
        };

        if ui.button(label).clicked() {
            app.status = if auto_running {
                app.stop_wait_worker()
            } else {
                app.start_wait_worker()
            };
        }
    });

    if app.wait_worker.is_some() {
        ui.colored_label(
            egui::Color32::from_rgb(0xF0, 0x80, 0x40),
            "自动应答运行中：目标每 10 ms 被自动放行一次，手动 Continue \
         通常找不到 Pending 项（日志里 ACK 带“自动应答”）。",
        );
    }

    // 一个断点一个等待项：seq 稳定，`hits` 是累计命中次数，
    // 状态在 空闲/等待应答/已应答 之间切换。
    let pending = app
        .waits
        .iter()
        .filter(|wait| wait.state == hypercall::WAIT_STATE_PENDING)
        .count();

    ui.label(format!(
        "等待项 {} 条（{pending} 条等待应答）；超时 {}；{}",
        app.waits.len(),
        if app.wait_timeout.trim() == "-1" {
            String::from("无限")
        } else {
            format!("{} ms", app.wait_timeout.trim())
        },
        if app.wait_worker.is_some() {
            "自动应答运行中"
        } else {
            "自动应答关闭"
        }
    ));

    // 列出**全部**等待项：一条断点一行，状态和累计命中一眼可见。
    if !app.waits.is_empty() {
        for wait in app.waits.iter().rev().take(6) {
            ui.label(format!(
                "seq={} {}  hits={}  thread=0x{:X}  cr3=0x{:X}  va=0x{:X}",
                wait.seq,
                hypercall::wait_state_name(wait.state),
                wait.hits,
                wait.thread,
                wait.cr3,
                wait.va
            ));
        }
    }

    if let Some(wait) = app.waits.last().copied() {
        ui.label(format!(
            "最新 seq={} {}  rip=0x{:X}  rsp=0x{:X}",
            wait.seq,
            hypercall::wait_state_name(wait.state),
            wait.rip,
            wait.rsp
        ));
        ui.monospace(wait.gpr_summary());
    } else {
        ui.label("没有等待项（命中用户断点后点“读取等待项”）");
    }

    ui.horizontal_wrapped(|ui| {
        ui.label("改寄存器:");
        ui.text_edit_singleline(&mut app.set_reg_name);
        ui.text_edit_singleline(&mut app.set_reg_value);

        if ui.button("写入").clicked() {
            app.status = app.set_wait_reg();
        }
    });

    if let Some(worker) = &app.wait_worker {
        let lines = worker.drain();

        if !lines.is_empty() {
            app.wait_log = lines.join(" | ");
        }
    }

    if !app.wait_log.is_empty() {
        ui.label(format!("自动应答: {}", app.wait_log));
    }

    ui.separator();
    ui.heading("其它");
    ui.add_space(2.0);

    ui.horizontal_wrapped(|ui| {
        if ui.button("HideSelf").clicked() {
            app.status = app.hide_self();
        }

        if ui.button("读镜像 root/guest").clicked() {
            app.status = app.read_self_pages();
        }

        if ui.button("Resolve NtCreateFile").clicked() {
            app.status = app.resolve_nt_create_file();
        }
    });

    ui.label(format!(
        "镜像页 root={} guest={} 字节",
        app.self_root.len(),
        app.self_guest.len()
    ));
}
