//! 内存 tab：查询/批量/扫描/映射/统计面板。

use eframe::egui;

use crate::MyApp;

pub fn show(app: &mut MyApp, ui: &mut egui::Ui) {
    app.mem_ui.show(ui);
}
