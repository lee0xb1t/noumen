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

/// `ExitStatus` 为 `STATUS_PENDING` 表示进程还活着。
pub const STATUS_PENDING: u32 = 0x103;

/// `ImageFileName` 的字节数（不含 NUL）。
pub const IMAGE_FILE_NAME_LEN: usize = 15;

// 表本体由 	ools/gen_win_offsets.py 生成：memflow 的 offsets/*.toml（X64）
// 加上从 ntoskrnl.exe 导出的 getter 反汇编出来的新 build（例如 26100）。
include!("win_offsets_table.rs");
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

/// 表里最大的 build（UI 提示“该版本没有偏移表”时用）。
pub fn newest_build() -> u32 {
    TABLE.last().map(|offsets| offsets.build).unwrap_or(0)
}

/// 全部已收录的 build，供 UI 展示。
pub fn known_builds() -> impl Iterator<Item = u32> {
    TABLE.iter().map(|offsets| offsets.build)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn exact_match_wins() {
        assert_eq!(select(19041).unwrap().build, 19041);
        assert_eq!(select(19045).unwrap().build, 19045);
    }

    #[test]
    fn nearest_lower_is_used() {
        // 19044 没有表，向下取 19041。
        assert_eq!(select(19044).unwrap().build, 19041);
        // 20H2 的 19042 同理。
        assert_eq!(select(19042).unwrap().build, 19041);
    }

    #[test]
    fn builds_are_sorted() {
        let builds: Vec<u32> = known_builds().collect();
        assert!(builds.windows(2).all(|pair| pair[0] < pair[1]));
    }

    #[test]
    fn newer_than_table_uses_latest() {
        // 22621（Win11 22H2）没有表：按“向下就近”取 19045。
        assert_eq!(select(22621).unwrap().build, 19045);
        // 26100（Win11 24H2）有表，直接用。
        assert_eq!(select(26100).unwrap().build, 26100);
        // 比 26100 更新的 build 退到 26100。
        assert_eq!(select(27000).unwrap().build, 26100);
    }

    #[test]
    fn win11_24h2_offsets_come_from_the_binary() {
        // 由 tools/gen_win_offsets.py 从 ntoskrnl.exe(10.0.26100.9457) 的导出反汇编得到：
        //   PsGetProcessId                     -> 48 8b 81 d0 01 00 00
        //   PsGetProcessImageFileName          -> 48 8d 81 38 03 00 00
        //   PsGetProcessPeb                    -> 48 8b 81 e0 02 00 00
        //   PsGetProcessSectionBaseAddress     -> 48 8b 81 b0 02 00 00
        //   PsGetProcessExitStatus             -> 8b 81 54 05 00 00
        //   PsGetProcessWow64Process           -> 48 8b 81 10 03 00 00
        //   PsGetCurrentProcess                -> gs:[188] 然后 [rax+b8]
        let offsets = select(26100).unwrap();

        assert_eq!(offsets.eproc_pid, 0x1d0);
        assert_eq!(offsets.eproc_link, 0x1d8);
        assert_eq!(offsets.eproc_name, 0x338);
        assert_eq!(offsets.eproc_peb, 0x2e0);
        assert_eq!(offsets.eproc_section_base, 0x2b0);
        assert_eq!(offsets.eproc_exit_status, 0x554);
        assert_eq!(offsets.eproc_wow64, 0x310);
        assert_eq!(offsets.kthread_process, 0xb8);
    }

    #[test]
    fn old_or_invalid_build_is_rejected() {
        // 表里最老的是 3790（Server 2003 x64）；比它还老的直接拒绝。
        assert!(select(2600).is_none());
        assert!(select(0).is_none());
    }

    #[test]
    fn win10_1607_offsets_match_the_dump() {
        // 数值来自 `dt nt!_EPROCESS`（dmp\102426-7796-01.dmp, build 14393）。
        let offsets = select(14393).unwrap();

        assert_eq!(offsets.build, 14393);
        assert_eq!(offsets.eproc_pid, 0x2e8);
        assert_eq!(offsets.eproc_link, 0x2f0);
        assert_eq!(offsets.eproc_name, 0x450);
        assert_eq!(offsets.eproc_peb, 0x3f8);
        assert_eq!(offsets.eproc_section_base, 0x3c0);
        assert_eq!(offsets.eproc_exit_status, 0x61c);
    }

    #[test]
    fn win10_22h2_offsets_match_the_binary() {
        // 交叉验证：把真实的 ntoskrnl_win10_22h2.exe（FileVersion 10.0.19041.6456）
        // 丢给 tools/gen_win_offsets.py，从导出反汇编出来的值应该和这张表逐字段一致：
        //   PsGetProcessId                 48 8b 81 40 04 00 00
        //   PsGetProcessImageFileName      48 8d 81 a8 05 00 00
        //   PsGetProcessPeb                48 8b 81 50 05 00 00
        //   PsGetProcessSectionBaseAddress 48 8b 81 20 05 00 00
        //   PsGetProcessExitStatus         8b 81 d4 07 00 00
        //   PsGetProcessWow64Process       48 8b 81 80 05 00 00
        //   PsGetCurrentProcess            gs:[188] 然后 [rax+b8]
        let offsets = select(19041).unwrap();

        assert_eq!(offsets.eproc_pid, 0x440);
        assert_eq!(offsets.eproc_name, 0x5a8);
        assert_eq!(offsets.eproc_peb, 0x550);
        assert_eq!(offsets.eproc_section_base, 0x520);
        assert_eq!(offsets.eproc_exit_status, 0x7d4);
        assert_eq!(offsets.eproc_wow64, 0x580);
        assert_eq!(offsets.kthread_process, 0xb8);

        // 22H2 的 `NtBuildNumber` 是 19045，但 PE 版本资源里写的还是 19041 基线；
        // 两者 EPROCESS 布局相同，所以表里两张的值必须一模一样。
        let h1 = select(19041).unwrap();
        let h2 = select(19045).unwrap();

        assert_eq!(h1.eproc_pid, h2.eproc_pid);
        assert_eq!(h1.eproc_link, h2.eproc_link);
        assert_eq!(h1.eproc_name, h2.eproc_name);
        assert_eq!(h1.eproc_peb, h2.eproc_peb);
        assert_eq!(h1.eproc_section_base, h2.eproc_section_base);
        assert_eq!(h1.eproc_exit_status, h2.eproc_exit_status);
        assert_eq!(h1.eproc_thread_list, h2.eproc_thread_list);
        assert_eq!(h1.eproc_wow64, h2.eproc_wow64);
        assert_eq!(h1.eproc_vad_root, h2.eproc_vad_root);
        assert_eq!(h1.kthread_process, h2.kthread_process);
    }

    #[test]
    fn active_process_links_follows_unique_process_id() {
        // `EPROCESS.ActiveProcessLinks` 紧跟在 `UniqueProcessId` 后面（+8，LIST_ENTRY
        // 的 Flink/Blink），7601/14393/18362/19041/19045/26100 都成立；
        // 生成器也按这个不变量补 `eproc_link`，这里守住它。
        for build in known_builds() {
            let offsets = select(build).unwrap();

            if offsets.eproc_pid == 0 {
                continue;
            }

            assert_eq!(
                offsets.eproc_link,
                offsets.eproc_pid + 8,
                "build {build}: ActiveProcessLinks 不再紧跟 UniqueProcessId"
            );
        }
    }

    #[test]
    fn checked_build_mask() {
        // checked build 的高位不参与匹配。
        assert_eq!(select(0xC000_4A61).unwrap().build, 19041);
    }

    #[test]
    fn arch_constants_match_memflow() {
        assert_eq!(X64.peb_ldr, 0x18);
        assert_eq!(X64.ldr_list, 0x10);
        assert_eq!(X64.ldr_data_base, 0x30);
        assert_eq!(X64.ldr_data_base_name, 0x58);
        assert_eq!(STATUS_PENDING, 0x103);
        assert_eq!(IMAGE_FILE_NAME_LEN, 15);
    }
}
