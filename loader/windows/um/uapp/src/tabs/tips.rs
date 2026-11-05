//! 提示 tab：纯文本说明。

use eframe::egui;

use crate::MyApp;

pub fn show(_app: &mut MyApp, ui: &mut egui::Ui) {
    ui.heading("提示");
    ui.add_space(2.0);

    ui.label("环境 / 驱动 / Hypervisor");
    ui.monospace(
        "驱动服务名、路径、加载/卸载；Hypervisor 镜像与日志路径；\
         启动/停止、连接设备、Ping/内核参数。",
    );

    ui.separator();
    ui.label("VMX/EPT");
    ui.monospace(
        "NtCreateFile proxy（DetourTrampoline）：目标页 fake 副本跳驱动里的\
         NoumenNtCreateFileHandler，proxy 通过 hypervisor 写好的 origin 页\
         执行原函数、记录参数并透传返回值；不需要 handler VA，origin 页\
         数据读为 0、执行走真实页。\
         “断点”是隐藏 0xCC + 恢复原页 + MTF 单步。\
         “用户断点”命中后线程停在 INT3 等应答，超时自动 Continue。\
         等待超时 -1 = 无限；线程过滤只放行指定 ETHREAD。",
    );

    ui.separator();
    ui.label("SVM/NPT");
    ui.monospace(
        "只支持用户断点。byte-patch 用真实页 0xCC + clean 数据页，\
         驱动会 attach 目标进程并锁页，把物理页作为 hint 传给 hypervisor。\
         dr0 使用线性地址的硬件执行断点，命中后线程停在原指令（parked），\
         guest 读 DR0/DR6/DR7 看不到断点。copy-exec/tf-step/nx-trap 尚未实现。\
         SVM 等待使用独立的 0x33..0x38 hypercall。",
    );

    ui.separator();
    ui.label("内存");
    ui.monospace("内存查询/批量读写/扫描/映射/统计；只对 KernelArgs 描述的内存有效。");
}
