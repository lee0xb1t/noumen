//! User-mode client for the Noumen kernel driver.
//!
//! This module mirrors `kernel/public.h` and `kernel/command.h`. A request is a
//! 16-byte [`RequestHeader`] followed by a command payload; a response is a
//! 16-byte [`ResponseHeader`] followed by the result payload.
//!
//! `DeviceIoControl` only reports transport and envelope errors. A command that
//! fails (for example `SetImagePath` after `_start`) still completes the IOCTL
//! successfully and stores the `NTSTATUS` in [`ResponseHeader::status`]; this
//! module turns that into [`Error::Command`].

use std::ffi::OsStr;
use std::fmt;
use std::io;
use std::os::windows::ffi::OsStrExt;
use std::path::Path;
use std::ptr;

use ntapi::ntrtl::RtlNtStatusToDosError;
use windows_sys::Win32::Foundation::{
    CloseHandle, GENERIC_READ, GENERIC_WRITE, HANDLE, INVALID_HANDLE_VALUE,
};
use windows_sys::Win32::Storage::FileSystem::{
    CreateFileW, FILE_ATTRIBUTE_NORMAL, FILE_SHARE_READ, FILE_SHARE_WRITE, OPEN_EXISTING,
};
use windows_sys::Win32::System::IO::DeviceIoControl;

pub const PROTOCOL_VERSION: u32 = 4;
pub const DEVICE_PATH: &str = r"\\.\Noumen";
/// `CTL_CODE(FILE_DEVICE_UNKNOWN, 0x800, METHOD_BUFFERED, FILE_ANY_ACCESS)`.
pub const IOCTL_EXECUTE: u32 = 0x0022_2000;
pub const MAX_IOCTL_OUTPUT: usize = 1024 * 1024;
pub const MAX_PATH_CHARS: usize = 260;
pub const PING_MAGIC: u32 = 0x504F_4E47;
/// `NOUMEN_HYPERCALL_PROCESSOR_ANY`: let the loader pick the processor.
///
/// The uapp passes an explicit `StartProcessor` for its single-core button, so
/// this stays available for other callers of [`Device::hypercall_ping`].
#[allow(dead_code)]
pub const PROCESSOR_ANY: u32 = 0xFFFF_FFFF;

/// `NOUMEN_HOOK_KIND_BREAKPOINT`（硬件断点）已不可用；保留数值用于 ABI 对齐。
pub const HOOK_KIND_BREAKPOINT: u32 = 0;
/// `NOUMEN_HOOK_KIND_DETOUR_INTERNAL`：EPT 页副本里的绝对跳转。
pub const HOOK_KIND_DETOUR_INTERNAL: u32 = 1;
/// `NOUMEN_HOOK_KIND_HIDE`：读写看到全 0 私有页，执行运行真实页。
pub const HOOK_KIND_HIDE: u32 = 2;
/// `NOUMEN_HOOK_KIND_DETOUR_TRAMPOLINE`：驱动自有 NtCreateFile proxy + origin。
pub const HOOK_KIND_DETOUR_TRAMPOLINE: u32 = 3;
/// `NOUMEN_HOOK_KIND_USER_BREAKPOINT`：隐藏 `0xCC`，命中后停在 INT3 等应答。
/// 只支持用户态地址。
pub const HOOK_KIND_USER_BREAKPOINT: u32 = 4;

pub const STATE_UNLOADED: u32 = 0;
pub const STATE_LOADING: u32 = 1;
pub const STATE_RUNNING: u32 = 2;
pub const STATE_FAILED: u32 = 3;

#[repr(u32)]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Command {
    Ping = 0,
    GetInfo = 1,
    GetKernelArgs = 2,
    /// Advanced: replace the whole argument block before `_start`.
    #[allow(dead_code)]
    SetKernelArgs = 3,
    SetImagePath = 4,
    SetLogPath = 5,
    StartHypervisor = 6,
    StopHypervisor = 7,
    /// Ask the loader to run `VMCALL(PING)` on the processor that runs the
    /// guest. Unlike a user-mode `VMCALL`, this works in single-core mode.
    HypercallPing = 8,
    /// 在虚拟地址处安装或移除 EPT hook / 隐藏断点。
    EptHook = 9,
    /// 解析 ntoskrnl!NtCreateFile 的地址。
    ResolveNtCreateFile = 10,
    /// 从 guest 视角读 hypervisor KernelBase（走页表 + EPT）。
    ReadKernelImage = 11,
    /// OS 引导信息（ntoskrnl 基址/大小、NtBuildNumber、System EPROCESS）。
    /// 驱动负责提供，**不放在 KernelArgs 里**。
    GetOsInfo = 12,
    /// SVM/NPT hook with an explicit debug method.
    SvmHook = 13,
    /// NtCreateFile 的 NPT 函数 hook：安装/移除 + 读取命中日志。
    HookNtCreateFile = 14,
    /// 下发 Windows 内核结构偏移表（56 字节）；必须在 `StartHypervisor` 之前。
    SetWinOffsets = 15,
}

impl Command {
    pub const fn as_u32(self) -> u32 {
        self as u32
    }

    pub const fn name(self) -> &'static str {
        match self {
            Self::Ping => "Ping",
            Self::GetInfo => "GetInfo",
            Self::GetKernelArgs => "GetKernelArgs",
            Self::SetKernelArgs => "SetKernelArgs",
            Self::SetImagePath => "SetImagePath",
            Self::SetLogPath => "SetLogPath",
            Self::StartHypervisor => "StartHypervisor",
            Self::StopHypervisor => "StopHypervisor",
            Self::HypercallPing => "HypercallPing",
            Self::EptHook => "EptHook",
            Self::ResolveNtCreateFile => "ResolveNtCreateFile",
            Self::ReadKernelImage => "ReadKernelImage",
            Self::GetOsInfo => "GetOsInfo",
            Self::SvmHook => "SvmHook",
            Self::HookNtCreateFile => "HookNtCreateFile",
            Self::SetWinOffsets => "SetWinOffsets",
        }
    }
}

#[repr(C)]
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct RequestHeader {
    pub protocol_version: u32,
    pub command: u32,
    pub input_length: u32,
    pub output_length: u32,
}

#[repr(C)]
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct ResponseHeader {
    pub protocol_version: u32,
    pub command: u32,
    pub status: i32,
    pub output_length: u32,
}

#[repr(C)]
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Ping {
    pub value: u32,
    pub reserved: u32,
}

#[repr(C)]
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct PingResponse {
    pub value: u32,
    pub magic: u32,
}

#[repr(C)]
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct StartHypervisor {
    pub single_core: u32,
    pub randomize_core: u32,
}

/// Input of [`Command::HypercallPing`]. `processor` is a CPU number or
/// [`PROCESSOR_ANY`].
#[repr(C)]
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct HypercallPingRequest {
    pub nonce: u64,
    pub processor: u32,
    pub reserved: u32,
}

/// Reply of [`Command::HypercallPing`]. `processor` is the CPU the loader
/// pinned the `VMCALL` to; `reply` is the hypervisor's raw answer.
#[repr(C)]
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct HypercallPingResponse {
    pub reply: u64,
    pub processor: u32,
    pub reserved: u32,
}

/// [`Command::EptHook`] 的输入。`kind` 是 [`HOOK_KIND_DETOUR_INTERNAL`]、
/// [`HOOK_KIND_HIDE`]、[`HOOK_KIND_DETOUR_TRAMPOLINE`] 或
/// [`HOOK_KIND_USER_BREAKPOINT`]；硬件断点 `HOOK_KIND_BREAKPOINT` 会返回不支持。
///
/// [`HOOK_KIND_DETOUR_TRAMPOLINE`] 是驱动自有的 NtCreateFile proxy：proxy 和
/// origin 页都由驱动提供，`handler`/`trampoline` 填 0 即可（驱动转发前会填
/// 好 trampoline）。其它 kind 不使用这两个字段。
/// `remove` 非 0 时忽略它们。`processor` 规则与 ping 请求相同。`process_id`
/// 为 `0` 表示内核地址；r3 地址则用它选择进程，用该进程的 CR3/页表解析并锁住
/// 目标页。
#[repr(C)]
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct EptHookRequest {
    pub virtual_address: u64,
    pub kind: u32,
    pub remove: u32,
    pub processor: u32,
    pub process_id: u32,
    /// 保留：用户断点用 MDL 锁到的目标页物理地址；DETOUR_TRAMPOLINE 不用
    /// （proxy 由驱动提供）。
    pub handler: u64,
    /// 驱动填的 origin 页 VA（uapp 填 0）。
    pub trampoline: u64,
}

/// [`Command::EptHook`] 的回复。`reply` 为 `0` 表示成功，否则是 hypervisor
/// 的 `HookError` 编号；见 [`hook_status_name`]。
#[repr(C)]
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct EptHookResponse {
    pub reply: u64,
    pub processor: u32,
    pub reserved: u32,
}

/// [`Command::SvmHook`] input (32 bytes).
#[repr(C)]
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct SvmHookRequest {
    pub virtual_address: u64,
    pub handler: u64,
    pub trampoline: u64,
    pub method: u32,
    pub kind: u32,
    pub remove: u32,
    pub processor: u32,
    pub process_id: u32,
    pub reserved: u32,
}

impl SvmHookRequest {
    pub const METHOD_BYTE_PATCH: u32 = 0;
    pub const METHOD_DR_BREAKPOINT: u32 = 4;

    pub const fn method_name(method: u32) -> &'static str {
        match method {
            0 => "byte-patch",
            1 => "copy-exec",
            2 => "tf-step",
            3 => "nx-trap",
            4 => "dr0-breakpoint",
            _ => "unknown",
        }
    }
}

/// [`Command::ReadKernelImage`] 的输入。
#[repr(C)]
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct ReadKernelImageRequest {
    pub offset: u32,
    pub length: u32,
}

/// `NoumenCommandHookNtCreateFile` 的 `action`。
pub const NTCF_ACTION_INSTALL: u32 = 0;
pub const NTCF_ACTION_REMOVE: u32 = 1;
pub const NTCF_ACTION_READ_LOG: u32 = 2;

/// 日志里 `ObjectName` 的 WCHAR 数（与 `kernel/command.h` 一致）。
pub const NTCF_NAME_CHARS: usize = 128;

/// 驱动侧环形缓冲的条数（与 `NOUMEN_NTCF_LOG_CAPACITY` 一致）。
pub const NTCF_LOG_CAPACITY: usize = 64;

/// 已经安装过（命令 14 的安装返回）。
pub const STATUS_INVALID_DEVICE_STATE: i32 = 0xC000_0184u32 as i32;
/// 移除时没有安装记录。
pub const STATUS_NOT_FOUND: i32 = 0xC000_0225u32 as i32;

/// 名字被截断到 [`NTCF_NAME_CHARS`]。
pub const NTCF_FLAG_TRUNCATED: u32 = 0x01;
/// 读调用者的 `UNICODE_STRING` 时出错（缺页/坏指针），名字为空。
pub const NTCF_FLAG_FAULTED: u32 = 0x02;

/// [`Command::HookNtCreateFile`] 的输入（16 字节）。
#[repr(C)]
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct NtCreateFileRequest {
    /// [`NTCF_ACTION_INSTALL`] / [`NTCF_ACTION_REMOVE`] / [`NTCF_ACTION_READ_LOG`]。
    pub action: u32,
    /// 读日志时最多返回的条数；0 = 由输出缓冲长度决定。
    pub max_records: u32,
    /// 处理器选择，规则与 ping 相同。
    pub processor: u32,
    pub reserved: u32,
}

/// 一次 NtCreateFile 命中（312 字节）。与 `NOUMEN_NT_CREATE_FILE_LOG` 逐字段对应。
#[repr(C)]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct NtCreateFileLog {
    /// 从 1 开始严格递增；断号表示中间记录被环形缓冲覆盖。
    pub sequence: u64,
    /// 命中时的 RDTSC。
    pub tsc: u64,
    pub process_id: u32,
    pub thread_id: u32,
    /// NtCreateFile 的第 2 个参数。
    pub desired_access: u32,
    /// 第 8 个参数：`FILE_SUPERSEDE(0)` .. `FILE_OVERWRITE_IF(5)`。
    pub create_disposition: u32,
    /// 第 9 个参数。
    pub create_options: u32,
    /// 第 6 个参数。
    pub file_attributes: u32,
    /// 第 7 个参数。
    pub share_access: u32,
    /// `object_name` 的有效字节数（不含结尾 NUL）。
    pub name_length: u32,
    pub object_name: [u16; NTCF_NAME_CHARS],
    pub flags: u32,
    pub reserved: u32,
}

impl Default for NtCreateFileLog {
    fn default() -> Self {
        Self {
            sequence: 0,
            tsc: 0,
            process_id: 0,
            thread_id: 0,
            desired_access: 0,
            create_disposition: 0,
            create_options: 0,
            file_attributes: 0,
            share_access: 0,
            name_length: 0,
            object_name: [0; NTCF_NAME_CHARS],
            flags: 0,
            reserved: 0,
        }
    }
}

impl NtCreateFileLog {
    /// `ObjectName`；`NameLength` 是字节数，末尾没有 NUL 也照样截断。
    pub fn name(&self) -> String {
        let chars = (self.name_length as usize / 2).min(NTCF_NAME_CHARS);

        String::from_utf16_lossy(&self.object_name[..chars])
    }

    pub fn truncated(&self) -> bool {
        self.flags & NTCF_FLAG_TRUNCATED != 0
    }

    pub fn faulted(&self) -> bool {
        self.flags & NTCF_FLAG_FAULTED != 0
    }

    pub const fn disposition_name(&self) -> &'static str {
        create_disposition_name(self.create_disposition)
    }

    /// 表格 hover 用：把常用的 `DesiredAccess` 位拆成名字。
    pub fn desired_access_names(&self) -> Vec<&'static str> {
        const FLAGS: &[(u32, &str)] = &[
            (0x0000_0001, "FILE_READ_DATA"),
            (0x0000_0002, "FILE_WRITE_DATA"),
            (0x0000_0004, "FILE_APPEND_DATA"),
            (0x0000_0008, "FILE_READ_EA"),
            (0x0000_0010, "FILE_WRITE_EA"),
            (0x0000_0020, "FILE_EXECUTE"),
            (0x0000_0040, "FILE_DELETE_CHILD"),
            (0x0000_0080, "FILE_READ_ATTRIBUTES"),
            (0x0000_0100, "FILE_WRITE_ATTRIBUTES"),
            (0x0001_0000, "DELETE"),
            (0x0002_0000, "READ_CONTROL"),
            (0x0004_0000, "WRITE_DAC"),
            (0x0008_0000, "WRITE_OWNER"),
            (0x0010_0000, "SYNCHRONIZE"),
            (0x8000_0000, "GENERIC_READ"),
            (0x4000_0000, "GENERIC_WRITE"),
            (0x2000_0000, "GENERIC_EXECUTE"),
            (0x1000_0000, "GENERIC_ALL"),
        ];

        FLAGS
            .iter()
            .filter(|(bit, _)| self.desired_access & bit != 0)
            .map(|(_, name)| *name)
            .collect()
    }
}

/// `FILE_SUPERSEDE(0)` .. `FILE_OVERWRITE_IF(5)`。
pub const fn create_disposition_name(value: u32) -> &'static str {
    match value {
        0 => "SUPERSEDE",
        1 => "OPEN",
        2 => "CREATE",
        3 => "OPEN_IF",
        4 => "OVERWRITE",
        5 => "OVERWRITE_IF",
        _ => "未知",
    }
}

/// [`Command::HookNtCreateFile`] 的回复头（56 字节），读日志时后面紧跟
/// `records` 条 [`NtCreateFileLog`]。
#[repr(C)]
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct NtCreateFileResponse {
    /// hook 的 `NtCreateFile` 地址。
    pub address: u64,
    /// 驱动里的 SVM trampoline target（命令 14）；VMX proxy 用同一份日志。
    pub handler: u64,
    /// SVM 命令 14 的 NonPagedPool trampoline；VMX origin 页不在这里返回。
    pub trampoline: u64,
    /// hypervisor 的 hook 状态：0 = 成功，否则是 `HookError` 编号。
    pub reply: u64,
    /// 累计命中次数。
    pub total_records: u64,
    pub processor: u32,
    /// 本次写入的记录条数。
    pub records: u32,
    /// 驱动侧安装状态：1 = 已安装。
    pub installed: u32,
    pub reserved: u32,
}
/// [`EptHookResponse::reply`] 里 EPT hook 状态的可读名称。
pub const fn hook_status_name(status: u64) -> &'static str {
    match status {
        0 => "成功",
        1 => "地址无效（非规范地址或补丁超出页尾）",
        2 => "地址未映射 / 超出 EPT 标识映射",
        3 => "不支持（用户态 detour、用户断点用了内核地址、无法重定位的指令，或 CPU 无 MTF）",
        4 => "没有空闲 hook 槽位",
        5 => "页池或 EPT 拆分表分配失败",
        6 => "该地址已经有 hook",
        7 => "找不到对应的 hook",
        8 => "hypercall 已禁用（key 为 0）",
        9 => "该页已被另一种 hook 占用",
        10 => "CPU 不支持 monitor trap flag",
        _ => "未知状态",
    }
}


/// The 168-byte structure expected by `SetKernelArgs` / returned by
/// `GetKernelArgs`. 与 `kernel/command.h` 的 `NOUMEN_KERNEL_ARGS` 逐字段对应。
///
/// v3 起只含**内存层**参数；OS 语义（进程/模块/偏移/PE）完全由 uapp 负责。
/// v4 在末尾追加 `win_offsets`：hypervisor 自己要做进程检查（TSC 选择性隐藏）
/// 时必须有字段偏移，但偏移表随 build 变，只能由 uapp 按 `win_offsets::select`
/// 选好后随参数传进去。
#[repr(C)]
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct KernelArgs {
    pub version: u32,
    pub cpu_count: u8,
    pub reserved0: [u8; 3],
    pub phys_map_count: u32,
    pub reserved1: u32,
    pub phys_map_base: u64,
    pub system_cr3: u64,
    pub system_cr3_virt: u64,
    pub kernel_base: u64,
    pub symbol_base: u64,
    pub string_base: u64,
    pub log_base: u64,
    pub kernel_size: u32,
    pub symbol_size: u32,
    pub string_size: u32,
    pub log_size: u32,
    pub symbol_count: u16,
    pub reserved2: u16,
    pub reserved3: u32,
    pub kernel_pfn_base: u64,
    pub kernel_pfn_count: u32,
    pub reserved4: u32,
    /// v4：Windows 内核结构偏移表；`build == 0` 表示未提供。
    pub win_offsets: WinOffsets,
}

/// `NOUMEN_WIN_OFFSETS`：56 字节，字段与 hypervisor 的 `kargs::WinOffsets` 一致。
#[repr(C)]
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct WinOffsets {
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
    pub kthread_process: u32,
    pub reserved5: u32,
}

impl WinOffsets {
    /// 从 `win_offsets::select(build)` 的结果构造；表里没有的 build 返回 `None`。
    pub fn from_table(table: &uapp::win_offsets::WinOffsets) -> Self {
        Self {
            build: table.build,
            list_blink: table.list_blink,
            eproc_link: table.eproc_link,
            kproc_dtb: table.kproc_dtb,
            eproc_pid: table.eproc_pid,
            eproc_name: table.eproc_name,
            eproc_peb: table.eproc_peb,
            eproc_section_base: table.eproc_section_base,
            eproc_exit_status: table.eproc_exit_status,
            eproc_thread_list: table.eproc_thread_list,
            eproc_wow64: table.eproc_wow64,
            eproc_vad_root: table.eproc_vad_root,
            kthread_process: table.kthread_process,
            reserved5: 0,
        }
    }
}

/// `GetOsInfo` 的 40 字节输出。
///
/// OS 引导信息由**驱动**提供（它有内核上下文），不放在 `KernelArgs` 里：
/// `KernelArgs` 是 hypervisor 的内存层契约，只含内存参数。
#[repr(C)]
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct OsInfo {
    pub version: u32,
    pub build: u32,
    pub flags: u32,
    pub reserved: u32,
    pub ntos_base: u64,
    pub ntos_size: u64,
    pub system_eprocess: u64,
}

pub const OS_INFO_FLAG_NTOS: u32 = 0x01;

impl OsInfo {
    pub const fn ntos_ready(&self) -> bool {
        self.flags & OS_INFO_FLAG_NTOS != 0 && self.ntos_base != 0
    }


}

/// The 72-byte `GetInfo` payload.
#[repr(C)]
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Info {
    pub protocol_version: u32,
    pub state: u32,
    pub last_status: i32,
    pub cpu_count: u32,
    pub start_processor: u32,
    pub single_core: u32,
    pub kernel_base: u64,
    pub kernel_size: u64,
    pub entry_point: u64,
    pub symbol_base: u64,
    pub symbol_count: u32,
    pub log_size: u32,
    pub hypercall_key: u64,
}

impl Info {
    pub const fn state_name(&self) -> &'static str {
        match self.state {
            STATE_UNLOADED => "未加载",
            STATE_LOADING => "加载中",
            STATE_RUNNING => "运行中",
            STATE_FAILED => "失败",
            _ => "未知",
        }
    }

    pub const fn is_running(&self) -> bool {
        self.state == STATE_RUNNING
    }

    /// 单核模式只虚拟化 `start_processor`；全核模式虚拟化 `0..cpu_count-1`。
    pub const fn mode_name(&self) -> &'static str {
        if self.single_core != 0 {
            "单核"
        } else {
            "全核"
        }
    }
}

/// Header used by `SetImagePath` and `SetLogPath`: `ULONG Length`, `ULONG
/// Reserved`, then `NOUMEN_MAX_PATH` UTF-16 code units.
#[repr(C)]
#[derive(Clone, Copy, Debug)]
pub struct PathRequest {
    pub length: u32,
    pub reserved: u32,
    pub path: [u16; MAX_PATH_CHARS],
}

const _: () = assert!(std::mem::size_of::<RequestHeader>() == 16);
const _: () = assert!(std::mem::size_of::<ResponseHeader>() == 16);
const _: () = assert!(std::mem::size_of::<Info>() == 72);
const _: () = assert!(std::mem::size_of::<KernelArgs>() == 168);
const _: () = assert!(std::mem::size_of::<WinOffsets>() == 56);
const _: () = assert!(std::mem::size_of::<OsInfo>() == 40);
const _: () = assert!(std::mem::size_of::<StartHypervisor>() == 8);
const _: () = assert!(std::mem::size_of::<HypercallPingRequest>() == 16);
const _: () = assert!(std::mem::size_of::<HypercallPingResponse>() == 16);
const _: () = assert!(std::mem::size_of::<EptHookRequest>() == 40);
const _: () = assert!(std::mem::size_of::<EptHookResponse>() == 16);
const _: () = assert!(std::mem::size_of::<ReadKernelImageRequest>() == 8);
const _: () = assert!(std::mem::size_of::<PathRequest>() == 528);
const _: () = assert!(std::mem::offset_of!(PathRequest, path) == 8);
const _: () = assert!(std::mem::size_of::<NtCreateFileRequest>() == 16);
const _: () = assert!(std::mem::size_of::<NtCreateFileLog>() == 312);
const _: () = assert!(std::mem::offset_of!(NtCreateFileLog, object_name) == 48);
const _: () = assert!(std::mem::offset_of!(NtCreateFileLog, flags) == 304);
const _: () = assert!(std::mem::size_of::<NtCreateFileResponse>() == 56);

/// An open handle to `\\.\Noumen`.
pub struct Device {
    handle: HANDLE,
}

impl Device {
    /// Open the control device exposed by `NoumenCommInitialize`.
    pub fn open() -> Result<Self, Error> {
        let path = wide(DEVICE_PATH);

        let handle = unsafe {
            CreateFileW(
                path.as_ptr(),
                GENERIC_READ | GENERIC_WRITE,
                FILE_SHARE_READ | FILE_SHARE_WRITE,
                ptr::null(),
                OPEN_EXISTING,
                FILE_ATTRIBUTE_NORMAL,
                ptr::null_mut(),
            )
        };

        if handle == INVALID_HANDLE_VALUE {
            return Err(Error::Open(io::Error::last_os_error()));
        }

        Ok(Self { handle })
    }

    /// Round-trip a value through the driver. Does not require a running
    /// hypervisor.
    pub fn ping(&self, value: u32) -> Result<u32, Error> {
        let request = Ping { value, reserved: 0 };
        let payload = self.execute(
            Command::Ping,
            as_bytes(&request),
            std::mem::size_of::<PingResponse>(),
        )?;
        let response: PingResponse = read_struct(&payload)?;

        if response.magic != PING_MAGIC {
            return Err(Error::InvalidResponse(format!(
                "Ping magic 期望 0x{PING_MAGIC:08X}, 收到 0x{:08X}",
                response.magic
            )));
        }

        Ok(response.value)
    }

    pub fn get_info(&self) -> Result<Info, Error> {
        let payload = self.execute(Command::GetInfo, &[], std::mem::size_of::<Info>())?;
        read_struct(&payload)
    }

    pub fn resolve_nt_create_file(&self) -> Result<u64, Error> {
        let payload = self.execute(
            Command::ResolveNtCreateFile,
            &[],
            std::mem::size_of::<u64>(),
        )?;

        read_struct(&payload)
    }

    /// 从 guest 视角读 hypervisor 的 KernelBase；HideSelf 后应返回全 0。
    pub fn read_kernel_image(&self, offset: u32, length: u32) -> Result<Vec<u8>, Error> {
        let request = ReadKernelImageRequest { offset, length };
        let payload = self.execute(
            Command::ReadKernelImage,
            as_bytes(&request),
            length as usize,
        )?;

        if payload.len() != length as usize {
            return Err(Error::TruncatedResponse {
                expected: length as usize,
                actual: payload.len(),
            });
        }

        Ok(payload)
    }

    pub fn get_kernel_args(&self) -> Result<KernelArgs, Error> {
        let payload = self.execute(
            Command::GetKernelArgs,
            &[],
            std::mem::size_of::<KernelArgs>(),
        )?;
        read_struct(&payload)
    }

    /// OS 引导信息：ntoskrnl 基址/大小、NtBuildNumber、System EPROCESS。
    ///
    /// 由驱动解析（`RtlPcToFileHeader` + `PsInitialSystemProcess`），uapp 只用它
    /// 做起点，后续的导出/偏移/遍历仍全部在 uapp。
    pub fn get_os_info(&self) -> Result<OsInfo, Error> {
        let payload = self.execute(Command::GetOsInfo, &[], std::mem::size_of::<OsInfo>())?;
        read_struct(&payload)
    }

    /// Advanced escape hatch: the driver expects a complete structure and
    /// disables its automatic filling once this succeeds.
    #[allow(dead_code)]
    pub fn set_kernel_args(&self, args: &KernelArgs) -> Result<(), Error> {
        self.execute(Command::SetKernelArgs, as_bytes(args), 0)?;
        Ok(())
    }

    /// 下发 Windows 内核结构偏移表；`StartHypervisor` 之前调用。
    ///
    /// 驱动只保存，`Start` 时合并进 `KernelArgs.WinOffsets`。传 `build == 0`
    /// 等于清除（hypervisor 侧据此关掉依赖偏移的进程检查）。
    pub fn set_win_offsets(&self, offsets: &WinOffsets) -> Result<(), Error> {
        self.execute(Command::SetWinOffsets, as_bytes(offsets), 0)?;
        Ok(())
    }

    /// Set the ELF image path. Only valid before `_start` has run.
    pub fn set_image_path(&self, path: &Path) -> Result<(), Error> {
        self.set_path(Command::SetImagePath, path)
    }

    /// Set the log dump path. Valid before and after `_start`.
    pub fn set_log_path(&self, path: &Path) -> Result<(), Error> {
        self.set_path(Command::SetLogPath, path)
    }

    /// Start the hypervisor. The driver defaults to `\??\C:\noumen` unless
    /// [`Device::set_image_path`] was called first.
    pub fn start_hypervisor(&self, single_core: bool, randomize_core: bool) -> Result<(), Error> {
        let request = StartHypervisor {
            single_core: u32::from(single_core),
            randomize_core: u32::from(randomize_core),
        };
        self.execute(Command::StartHypervisor, as_bytes(&request), 0)?;
        Ok(())
    }

    /// 让驱动逐个核退出 VMX（`VMCALL(STOP)`）。
    pub fn stop_hypervisor(&self) -> Result<(), Error> {
        self.execute(Command::StopHypervisor, &[], 0)?;
        Ok(())
    }

    /// Ask the loader to issue `VMCALL(PING)` on a processor that runs the
    /// guest.
    ///
    /// A raw user-mode `VMCALL` is only a VM exit on a CPU in VMX non-root
    /// operation. In single-core mode that is exactly one processor
    /// (`StartProcessor`), so issuing it from an arbitrary UI thread raises
    /// `#UD` and terminates the process. The loader knows the right CPU,
    /// pins to it under the hypervisor lock and returns the raw reply.
    ///
    /// `processor` is either a concrete CPU number or [`PROCESSOR_ANY`], which
    /// lets the loader choose: `StartProcessor` in single-core mode, the
    /// calling CPU in multi-core mode.
    pub fn hypercall_ping(
        &self,
        nonce: u64,
        processor: u32,
    ) -> Result<HypercallPingResponse, Error> {
        let request = HypercallPingRequest {
            nonce,
            processor,
            reserved: 0,
        };
        let payload = self.execute(
            Command::HypercallPing,
            as_bytes(&request),
            std::mem::size_of::<HypercallPingResponse>(),
        )?;
        read_struct(&payload)
    }

    /// 在运行 guest 的处理器上安装（`remove == false`）或移除
    /// `virtual_address` 处的 EPT hook / 隐藏断点。
    ///
    /// `process_id` 为 `0` 表示内核虚拟地址。r3 地址则由 loader attach 到该
    /// 进程，在 `VMCALL` 之前 probe 并锁住目标页，于是 hypervisor 用目标 CR3
    /// 解析，hook 存活期间页也不会被换出。
    pub fn ept_hook(
        &self,
        virtual_address: u64,
        kind: u32,
        remove: bool,
        processor: u32,
        process_id: u32,
    ) -> Result<EptHookResponse, Error> {
        let request = EptHookRequest {
            virtual_address,
            kind,
            remove: u32::from(remove),
            processor,
            process_id,
            handler: 0,
            trampoline: 0,
        };
        let payload = self.execute(
            Command::EptHook,
            as_bytes(&request),
            std::mem::size_of::<EptHookResponse>(),
        )?;
        read_struct(&payload)
    }

    pub fn svm_hook(
        &self,
        virtual_address: u64,
        method: u32,
        kind: u32,
        remove: bool,
        processor: u32,
        process_id: u32,
    ) -> Result<EptHookResponse, Error> {
        let request = SvmHookRequest {
            virtual_address,
            handler: 0,
            trampoline: 0,
            method,
            kind,
            remove: u32::from(remove),
            processor,
            process_id,
            reserved: 0,
        };
        let payload = self.execute(
            Command::SvmHook,
            as_bytes(&request),
            std::mem::size_of::<EptHookResponse>(),
        )?;
        read_struct(&payload)
    }

    /// 安装（[`NTCF_ACTION_INSTALL`]）或移除（[`NTCF_ACTION_REMOVE`]）
    /// `ntoskrnl!NtCreateFile` 的 NPT 函数 hook。
    ///
    /// 地址解析、trampoline 分配和 handler 都在驱动里；`response.reply` 是
    /// hypervisor 的 hook 状态（0 = 成功），非 0 时用
    /// [`hook_status_name`] 解释。
    pub fn nt_create_file_hook(
        &self,
        action: u32,
        processor: u32,
    ) -> Result<NtCreateFileResponse, Error> {
        let request = NtCreateFileRequest {
            action,
            max_records: 0,
            processor,
            reserved: 0,
        };
        let payload = self.execute(
            Command::HookNtCreateFile,
            as_bytes(&request),
            std::mem::size_of::<NtCreateFileResponse>(),
        )?;

        read_struct(&payload)
    }

    /// 读取命中日志。返回值是回复头和最多 `max_records` 条记录
    /// （按时间从旧到新）。
    pub fn read_nt_create_file_log(
        &self,
        max_records: u32,
    ) -> Result<(NtCreateFileResponse, Vec<NtCreateFileLog>), Error> {
        let request = NtCreateFileRequest {
            action: NTCF_ACTION_READ_LOG,
            max_records,
            processor: PROCESSOR_ANY,
            reserved: 0,
        };

        let header_size = std::mem::size_of::<NtCreateFileResponse>();
        let record_size = std::mem::size_of::<NtCreateFileLog>();
        let output_length = header_size + record_size * max_records as usize;

        let payload = self.execute(
            Command::HookNtCreateFile,
            as_bytes(&request),
            output_length,
        )?;

        let response: NtCreateFileResponse = read_struct(&payload)?;

        // 驱动写的条数不应该超过负载，但不要相信它：以实际长度为准。
        let available = payload.len().saturating_sub(header_size) / record_size;
        let count = (response.records as usize).min(available);
        let mut records = Vec::with_capacity(count);

        for index in 0..count {
            let start = header_size + index * record_size;
            records.push(read_struct(&payload[start..start + record_size])?);
        }

        Ok((response, records))
    }

    /// Send a raw request and return the response payload.
    ///
    /// `output_length` is the desired payload size; it is capped at
    /// [`MAX_IOCTL_OUTPUT`], like the driver does. A non-negative
    /// `ResponseHeader::status` is required for a payload to be returned.
    pub fn execute(
        &self,
        command: Command,
        input: &[u8],
        output_length: usize,
    ) -> Result<Vec<u8>, Error> {
        let input_length = u32::try_from(input.len())
            .map_err(|_| Error::InvalidArgument(String::from("输入负载超过 4 GiB")))?;
        let output_length = output_length.min(MAX_IOCTL_OUTPUT);
        let output_length_u32 = u32::try_from(output_length)
            .map_err(|_| Error::InvalidArgument(String::from("输出长度超过 4 GiB")))?;

        let header = RequestHeader {
            protocol_version: PROTOCOL_VERSION,
            command: command.as_u32(),
            input_length,
            output_length: output_length_u32,
        };

        let mut request = Vec::with_capacity(std::mem::size_of::<RequestHeader>() + input.len());
        request.extend_from_slice(as_bytes(&header));
        request.extend_from_slice(input);

        let request_length = u32::try_from(request.len())
            .map_err(|_| Error::InvalidArgument(String::from("请求超过 4 GiB")))?;
        let response_length = std::mem::size_of::<ResponseHeader>() + output_length;
        let mut response = vec![0u8; response_length];
        let mut returned = 0u32;

        let ok = unsafe {
            DeviceIoControl(
                self.handle,
                IOCTL_EXECUTE,
                request.as_ptr().cast(),
                request_length,
                response.as_mut_ptr().cast(),
                response_length as u32,
                &mut returned,
                ptr::null_mut(),
            )
        };

        if ok == 0 {
            return Err(Error::Io(io::Error::last_os_error()));
        }

        let returned = returned as usize;

        if returned < std::mem::size_of::<ResponseHeader>() {
            return Err(Error::TruncatedResponse {
                expected: std::mem::size_of::<ResponseHeader>(),
                actual: returned,
            });
        }

        let header: ResponseHeader = read_struct(&response[..returned])?;

        if header.protocol_version != PROTOCOL_VERSION {
            return Err(Error::Protocol {
                expected: PROTOCOL_VERSION,
                actual: header.protocol_version,
            });
        }

        if header.command != command.as_u32() {
            return Err(Error::CommandMismatch {
                expected: command,
                actual: header.command,
            });
        }

        if header.status < 0 {
            return Err(Error::Command {
                command,
                status: header.status,
            });
        }

        let payload_length = header.output_length as usize;
        let available = returned - std::mem::size_of::<ResponseHeader>();

        if payload_length > available {
            return Err(Error::TruncatedResponse {
                expected: std::mem::size_of::<ResponseHeader>() + payload_length,
                actual: returned,
            });
        }

        Ok(response[std::mem::size_of::<ResponseHeader>()
            ..std::mem::size_of::<ResponseHeader>() + payload_length]
            .to_vec())
    }

    fn set_path(&self, command: Command, path: &Path) -> Result<(), Error> {
        let path = encode_path(path)?;
        let mut request = PathRequest {
            length: (path.len() * std::mem::size_of::<u16>()) as u32,
            reserved: 0,
            path: [0; MAX_PATH_CHARS],
        };
        request.path[..path.len()].copy_from_slice(&path);

        self.execute(command, as_bytes(&request), 0)?;
        Ok(())
    }
}

impl Drop for Device {
    fn drop(&mut self) {
        if self.handle != INVALID_HANDLE_VALUE {
            unsafe {
                CloseHandle(self.handle);
            }
        }
    }
}

#[derive(Debug)]
pub enum Error {
    Open(io::Error),
    Io(io::Error),
    InvalidArgument(String),
    InvalidPath(String),
    InvalidResponse(String),
    Protocol { expected: u32, actual: u32 },
    CommandMismatch { expected: Command, actual: u32 },
    Command { command: Command, status: i32 },
    TruncatedResponse { expected: usize, actual: usize },
}

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Open(err) => write!(f, "打开 {DEVICE_PATH} 失败: {err}"),
            Self::Io(err) => write!(f, "DeviceIoControl 失败: {err}"),
            Self::InvalidArgument(message) => write!(f, "参数错误: {message}"),
            Self::InvalidPath(message) => write!(f, "路径无效: {message}"),
            Self::InvalidResponse(message) => write!(f, "响应无效: {message}"),
            Self::Protocol { expected, actual } => {
                write!(f, "协议版本不匹配: 期望 {expected}, 收到 {actual}")
            }
            Self::CommandMismatch { expected, actual } => write!(
                f,
                "响应命令不匹配: 期望 {} ({}), 收到 {}",
                expected.name(),
                expected.as_u32(),
                actual
            ),
            Self::Command { command, status } => {
                write!(f, "{} 失败: {}", command.name(), ntstatus_message(*status))
            }
            Self::TruncatedResponse { expected, actual } => {
                write!(f, "响应过短: 需要 {expected} 字节, 实际 {actual} 字节")
            }
        }
    }
}

impl std::error::Error for Error {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Open(err) | Self::Io(err) => Some(err),
            _ => None,
        }
    }
}

fn ntstatus_message(status: i32) -> String {
    let win32 = unsafe { RtlNtStatusToDosError(status) };

    if win32 == 0 {
        return format!("NTSTATUS 0x{status:08X}");
    }

    format!(
        "NTSTATUS 0x{status:08X} ({})",
        io::Error::from_raw_os_error(win32 as i32)
    )
}

fn wide(value: &str) -> Vec<u16> {
    OsStr::new(value)
        .encode_wide()
        .chain(std::iter::once(0))
        .collect()
}

fn encode_path(path: &Path) -> Result<Vec<u16>, Error> {
    let wide: Vec<u16> = path.as_os_str().encode_wide().collect();

    if wide.is_empty() {
        return Err(Error::InvalidPath(String::from("路径为空")));
    }

    if wide.contains(&0) {
        return Err(Error::InvalidPath(String::from("路径包含内嵌 NUL")));
    }

    if wide.len() > MAX_PATH_CHARS {
        return Err(Error::InvalidPath(format!(
            "路径超过 {MAX_PATH_CHARS} 个 UTF-16 字符"
        )));
    }

    Ok(wide)
}

fn as_bytes<T>(value: &T) -> &[u8] {
    unsafe {
        std::slice::from_raw_parts((value as *const T).cast::<u8>(), std::mem::size_of::<T>())
    }
}

fn read_struct<T: Copy>(bytes: &[u8]) -> Result<T, Error> {
    if bytes.len() < std::mem::size_of::<T>() {
        return Err(Error::TruncatedResponse {
            expected: std::mem::size_of::<T>(),
            actual: bytes.len(),
        });
    }

    Ok(unsafe { ptr::read_unaligned(bytes.as_ptr().cast::<T>()) })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn wire_layout_matches_kernel_headers() {
        assert_eq!(std::mem::size_of::<RequestHeader>(), 16);
        assert_eq!(std::mem::size_of::<ResponseHeader>(), 16);
        assert_eq!(std::mem::size_of::<Info>(), 72);
        assert_eq!(std::mem::size_of::<KernelArgs>(), 112);
        assert_eq!(std::mem::offset_of!(KernelArgs, phys_map_base), 0x10);
        assert_eq!(std::mem::offset_of!(KernelArgs, system_cr3), 0x18);
                assert_eq!(std::mem::size_of::<OsInfo>(), 40);
        assert_eq!(std::mem::offset_of!(OsInfo, ntos_base), 0x10);
        assert_eq!(std::mem::size_of::<StartHypervisor>(), 8);
        assert_eq!(std::mem::size_of::<HypercallPingRequest>(), 16);
        assert_eq!(std::mem::size_of::<HypercallPingResponse>(), 16);
        assert_eq!(std::mem::size_of::<EptHookRequest>(), 40);
        assert_eq!(std::mem::size_of::<EptHookResponse>(), 16);
        assert_eq!(std::mem::size_of::<ReadKernelImageRequest>(), 8);
        assert_eq!(std::mem::size_of::<PathRequest>(), 528);
        assert_eq!(std::mem::offset_of!(PathRequest, path), 8);
        assert_eq!(std::mem::size_of::<NtCreateFileRequest>(), 16);
        assert_eq!(std::mem::size_of::<NtCreateFileLog>(), 312);
        assert_eq!(std::mem::offset_of!(NtCreateFileLog, object_name), 48);
        assert_eq!(std::mem::offset_of!(NtCreateFileLog, flags), 304);
        assert_eq!(std::mem::size_of::<NtCreateFileResponse>(), 56);
    }

    #[test]
    fn command_numbers_match_the_driver() {
        assert_eq!(Command::HypercallPing.as_u32(), 8);
        assert_eq!(Command::EptHook.as_u32(), 9);
        assert_eq!(Command::HookNtCreateFile.as_u32(), 14);
    }

    #[test]
    fn nt_create_file_log_decodes_names_and_dispositions() {
        let mut log = NtCreateFileLog::default();

        assert_eq!(log.name(), "");
        assert!(!log.truncated());
        assert!(!log.faulted());

        let name: Vec<u16> = "\\??\\C:\\Windows\\test.txt".encode_utf16().collect();
        log.object_name[..name.len()].copy_from_slice(&name);
        log.name_length = (name.len() * 2) as u32;
        log.create_disposition = 3;

        assert_eq!(log.name(), "\\??\\C:\\Windows\\test.txt");
        assert_eq!(log.disposition_name(), "OPEN_IF");
        assert_eq!(create_disposition_name(5), "OVERWRITE_IF");
        assert_eq!(create_disposition_name(99), "未知");

        log.flags = NTCF_FLAG_TRUNCATED | NTCF_FLAG_FAULTED;
        assert!(log.truncated());
        assert!(log.faulted());

        log.desired_access = 0x0012_0089;
        assert!(log.desired_access_names().contains(&"FILE_READ_DATA"));
        assert!(log.desired_access_names().contains(&"SYNCHRONIZE"));
        assert!(!log.desired_access_names().contains(&"GENERIC_WRITE"));
    }

    #[test]
    fn hook_status_names_cover_the_hypervisor_numbers() {
        assert_eq!(hook_status_name(0), "成功");
        assert_ne!(hook_status_name(1), "未知状态");
        assert_ne!(hook_status_name(10), "未知状态");
    }

    #[test]
    fn paths_are_utf16_without_a_terminator() {
        let path = encode_path(Path::new(r"C:\noumen")).unwrap();

        assert_eq!(path.len(), 9);
        assert_eq!(path[0], b'C' as u16);
        assert_eq!(path[1], b':' as u16);
        assert!(!path.contains(&0));
    }

    #[test]
    fn invalid_paths_are_rejected_before_the_ioctl() {
        assert!(encode_path(Path::new("")).is_err());
        assert!(encode_path(Path::new("a\0b")).is_err());
    }

    #[test]
    fn ioctl_code_matches_the_kernel_ctl_code() {
        assert_eq!(IOCTL_EXECUTE, 0x0022_2000);
    }
}
