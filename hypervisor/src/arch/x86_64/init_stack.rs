//! noumen 初始化专用栈（`init_stack.S`）。
//!
//! loader 线程的内核栈只有十几 KiB，而 `_init` 的调用链（VMCS 配置 + EPT
//! 建表 + 日志格式化）会把它用到接近守卫页，触发 #DF → 0x7F。所有初始化
//! 都切到这里的 64 KiB 静态栈上跑，返回前把栈还给 loader。

unsafe extern "C" {
    /// 切到专用栈执行 `init_body(cpu_id)`，返回后恢复调用者的栈。
    ///
    /// `init_body` 是 `main.rs` 导出的 `extern "C" fn(u8) -> u32`，由汇编按名字调用。
    pub fn run_on_init_stack(cpu_id: u8) -> u32;
}
