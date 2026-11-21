//! Windows x64 偏移表。
//!
//! 数据来自 `memflow-win32` 的 `offsets/*.toml`（字段名一一对应）；表是**配置**，
//! 改数字不需要改逻辑，但必须跑 `WinOs::attach` 的自检 V1–V4。
//!
//! 选表规则：精确匹配 → 同 major 下**不大于**目标 build 的最近一张 → 失败。
//! 不向上取最近：高 build 的表读低 build 会静默读到相邻字段。

/// `_EPROCESS` / `_KPROCESS` 的字段偏移（随 build 变化）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct WinOffsets {
    /// 本表适用的基准 build。
    pub build: u32,
    pub list_blink: u32,
    pub eproc_link: u32,
    pub kproc_dtb: u32,
    pub eproc_pid: u32,
    pub eproc_name: u32,
    pub eproc_peb: u32,
    pub eproc_section_base: u32,
    pub eproc_exit_status: u32,
    pub eproc_thread_list: u32,
    pub eproc_wow64: u32,
    pub eproc_vad_root: u32,
    /// `_KTHREAD.ApcState.Process`：当前进程 = `gs_base + 本字段`。
    ///
    /// hypervisor 在 `RDTSC` 退出里用它反推当前进程；数值只验证过 19041/19045，
    /// 其它 build 由本表按“向下就近”选择，进程检查失败时按“不匹配”处理。
    pub kthread_process: u32,
}

/// PEB / PEB_LDR_DATA / LDR_DATA_TABLE_ENTRY 的偏移是**架构常量**。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct WinArchOffsets {
    pub peb_ldr: u32,
    pub peb_process_params: u32,
    pub ldr_list: u32,
    pub ldr_data_base: u32,
    pub ldr_data_size: u32,
    pub ldr_data_full_name: u32,
    pub ldr_data_base_name: u32,
}

pub const X64: WinArchOffsets = WinArchOffsets {
    peb_ldr: 0x18,
    peb_process_params: 0x20,
    ldr_list: 0x10,
    ldr_data_base: 0x30,
    ldr_data_size: 0x40,
    ldr_data_full_name: 0x48,
    ldr_data_base_name: 0x58,
};

// 表本体由 	ools/gen_win_offsets.py 生成：memflow 的 offsets/*.toml（X64）
// 加上从 ntoskrnl.exe 导出的 getter 反汇编出来的新 build（例如 26100）。
include!("ntos_offsets_table.rs");
/// 选表：精确 → 向下就近 → `None`。
pub fn select(build: u32) -> Option<&'static WinOffsets> {
    let build = build & 0xFFFF;

    if build == 0 {
        return None;
    }

    TABLE
        .iter()
        .find(|offsets| offsets.build == build)
        .or_else(|| {
            TABLE
                .iter()
                .filter(|offsets| offsets.build < build)
                .max_by_key(|offsets| offsets.build)
        })
}
