//! uapp 的 tab 容器：每个 tab 一个模块，负责自己的整页 UI。

pub mod driver_load;
pub mod memory;
pub mod svm;
pub mod tips;
pub mod tsc;
pub mod vmx;

use eframe::egui;

use crate::MyApp;

#[derive(Clone, Copy, PartialEq, Eq)]
pub enum Tab {
    DriverLoad,
    Vmx,
    Svm,
    Tsc,
    Memory,
    Tips,
}

impl Tab {
    pub const ALL: [Tab; 6] = [
        Tab::DriverLoad,
        Tab::Vmx,
        Tab::Svm,
        Tab::Tsc,
        Tab::Memory,
        Tab::Tips,
    ];

    pub const fn label(self) -> &'static str {
        match self {
            Tab::DriverLoad => "环境",
            Tab::Vmx => "VMX/EPT",
            Tab::Svm => "SVM/NPT",
            Tab::Tsc => "TSC",
            Tab::Memory => "内存",
            Tab::Tips => "提示",
        }
    }
}

/// 画当前 tab 的整页内容。调用方负责外层滚动区域与状态栏。
pub fn show(app: &mut MyApp, ui: &mut egui::Ui) {
    match app.tab {
        Tab::DriverLoad => driver_load::show(app, ui),
        Tab::Vmx => vmx::show(app, ui),
        Tab::Svm => svm::show(app, ui),
        Tab::Tsc => tsc::show(app, ui),
        Tab::Memory => memory::show(app, ui),
        Tab::Tips => tips::show(app, ui),
    }
}
