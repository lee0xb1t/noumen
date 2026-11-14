#pragma once

#include "public.h"

#define NOUMEN_PING_MAGIC 0x504F4E47UL

//
// KernelArgs ABI 版本。任何字段增删都必须 +1，并与 hypervisor 的
// `kargs::KARGS_VERSION` 同步。
//
// v3：移除全部 OS 字段（Os/OsBuild/OsBase/OsSize/OsEprocess/OsFlags）。
// 操作系统语义完全由调用者（uapp）负责，这里只剩内存层参数。
// v4：末尾追加 `WinOffsets`（Windows 内核结构字段偏移，56 字节），供 hypervisor
// 自己做进程检查（TSC 选择性隐藏）用；结构 112 → 168 字节。
//
#define NOUMEN_KARGS_VERSION 4

//
// 物理内存区间上限（与 hypervisor 的 `kargs::MAX_PHYS_RANGES` 一致）。
//
#define NOUMEN_MAX_PHYS_RANGES 256

//
// NOUMEN_HYPERCALL_PING_REQUEST.Processor value that lets the driver pick the
// processor: StartProcessor in single-core mode, the calling processor in
// multi-core mode (falling back to StartProcessor when that processor was not
// virtualized).
//
#define NOUMEN_HYPERCALL_PROCESSOR_ANY 0xFFFFFFFFUL

typedef enum _NOUMEN_COMMAND
{
    NoumenCommandPing = 0,
    NoumenCommandGetInfo = 1,
    NoumenCommandGetKernelArgs = 2,
    NoumenCommandSetKernelArgs = 3,
    NoumenCommandSetImagePath = 4,
    NoumenCommandSetLogPath = 5,
    NoumenCommandStartHypervisor = 6,
    NoumenCommandStopHypervisor = 7,
    NoumenCommandHypercallPing = 8,
    NoumenCommandEptHook = 9,
    NoumenCommandResolveNtCreateFile = 10,
    NoumenCommandReadKernelImage = 11,
    //
    // OS 引导信息只通过这个命令交给 uapp；KernelArgs 里不含任何 OS 字段。
    //
    NoumenCommandGetOsInfo = 12,
    //
    // SVM/NPT-only hook with an explicit debug method.
    //
    NoumenCommandSvmHook = 13,
    //
    // NtCreateFile NPT 函数 hook：安装/移除 + 读取 ObjectName 日志。
    //
    // 与 `NoumenCommandSvmHook` 的分工：那条命令是“通用调试 hook”，调用方
    // 自己给 VA/类型；这条命令只做一件事——hook `ntoskrnl!NtCreateFile`，
    // 由驱动解析地址、分配可执行 trampoline、提供 handler 并记录命中。
    // 参数校验、trampoline 生命周期和日志都在驱动里，uapp 只发命令。
    //
    NoumenCommandHookNtCreateFile = 14,
    //
    // 下发 Windows 内核结构偏移表（`NOUMEN_WIN_OFFSETS`，56 字节输入）。
    //
    // 偏移表随 build 变，只有 uapp 有按 build 选表的能力，所以由 uapp 在
    // `Start` **之前**下发；驱动只做保存，在 `Start` 里自动填充完内存字段后
    // 把它合并进 `KernelArgs` 交给 hypervisor。这样既不动 `SetKernelArgs`
    // 的“全量替换”语义，也不用让驱动自己懂 EPROCESS。
    //
    NoumenCommandSetWinOffsets = 15,
    NoumenCommandMax = 16
} NOUMEN_COMMAND;

#pragma pack(push, 8)

typedef struct _NOUMEN_PING
{
    ULONG Value;
    ULONG Reserved;
} NOUMEN_PING, *PNOUMEN_PING;

typedef struct _NOUMEN_PING_RESPONSE
{
    ULONG Value;
    ULONG Magic;
} NOUMEN_PING_RESPONSE, *PNOUMEN_PING_RESPONSE;

typedef struct _NOUMEN_START_HYPERVISOR
{
    ULONG SingleCore;
    ULONG RandomizeCore;
} NOUMEN_START_HYPERVISOR, *PNOUMEN_START_HYPERVISOR;

typedef struct _NOUMEN_HYPERCALL_PING_REQUEST
{
    UINT64 Nonce;
    ULONG Processor;
    ULONG Reserved;
} NOUMEN_HYPERCALL_PING_REQUEST, *PNOUMEN_HYPERCALL_PING_REQUEST;

typedef struct _NOUMEN_HYPERCALL_PING_RESPONSE
{
    UINT64 Reply;
    ULONG Processor;
    ULONG Reserved;
} NOUMEN_HYPERCALL_PING_RESPONSE, *PNOUMEN_HYPERCALL_PING_RESPONSE;

//
// 安装或移除 EPT hook / 隐藏断点。`VirtualAddress` 是虚拟地址；`ProcessId`
// 非 0 时，loader 会 attach 到该进程、probe 并锁住目标页，然后用目标进程的
// CR3 发 VMCALL。这就是 r3 地址能工作的原因；MDL 锁页保证 hook 存活期间页
// 保持驻留、PFN 不变。
// `Kind` 是 NOUMEN_HOOK_KIND_BREAKPOINT / USER_BREAKPOINT / DETOUR_INTERNAL /
// HIDE / DETOUR_TRAMPOLINE。`Handler` 是保留字段：用户断点（4）用它传驱动
// 用 MDL 锁到的目标页物理地址；DETOUR_TRAMPOLINE 的 proxy 由驱动直接提供。
// `Trampoline` 由驱动分配并填好，再交给 hypervisor 写 origin（调用方填 0）。
// `Remove` 置位时忽略 `Handler`/`Trampoline`。
// `Processor` 规则与 ping 请求相同。
//
typedef struct _NOUMEN_EPT_HOOK_REQUEST
{
    UINT64 VirtualAddress;
    ULONG Kind;
    ULONG Remove;
    ULONG Processor;
    ULONG ProcessId;
    // 保留：用户断点的目标页物理地址；其它 kind 填 0。
    UINT64 Handler;
    // DETOUR_TRAMPOLINE：驱动分配的 NonPagedPool origin 页基址。uapp 填 0，
    // 驱动在转发给 hypervisor 之前填好。
    UINT64 Trampoline;
} NOUMEN_EPT_HOOK_REQUEST, *PNOUMEN_EPT_HOOK_REQUEST;

//
// `Reply` 是 hypervisor 的 hook 状态：0 表示成功，否则是 HookError 编号
// （1 = 地址无效，2 = 未映射，3 = 不支持，4 = 无槽位，5 = 无内存，
// 6 = 已 hook，7 = 未找到，8 = hypercall 禁用，9 = 页类型冲突，
// 10 = CPU 不支持 MTF）。
//
typedef struct _NOUMEN_EPT_HOOK_RESPONSE
{
    UINT64 Reply;
    ULONG Processor;
    ULONG Reserved;
} NOUMEN_EPT_HOOK_RESPONSE, *PNOUMEN_EPT_HOOK_RESPONSE;

typedef struct _NOUMEN_READ_KERNEL_IMAGE
{
    ULONG Offset;
    ULONG Length;
} NOUMEN_READ_KERNEL_IMAGE, *PNOUMEN_READ_KERNEL_IMAGE;

//
// SVM/NPT hook request. `Method` is a NOUMEN_SVM_METHOD_* value; `Kind` uses the
// same NOUMEN_HOOK_KIND_* values. The response is NOUMEN_EPT_HOOK_RESPONSE.
//
typedef struct _NOUMEN_SVM_HOOK_REQUEST
{
    UINT64 VirtualAddress;
    UINT64 Handler;
    UINT64 Trampoline;
    ULONG Method;
    ULONG Kind;
    ULONG Remove;
    ULONG Processor;
    ULONG ProcessId;
    ULONG Reserved;
} NOUMEN_SVM_HOOK_REQUEST, *PNOUMEN_SVM_HOOK_REQUEST;

//
// `NoumenCommandHookNtCreateFile` 的 `Action`。
//
#define NOUMEN_NTCF_ACTION_INSTALL  0UL
#define NOUMEN_NTCF_ACTION_REMOVE   1UL
#define NOUMEN_NTCF_ACTION_READ_LOG 2UL

//
// 日志里 `ObjectName` 的最大长度（WCHAR 数）。超长截断并置
// `NOUMEN_NTCF_FLAG_TRUNCATED`；调用者缓冲区读不出来（用户地址缺页、
// 非规范地址）时置 `NOUMEN_NTCF_FLAG_FAULTED`，其余字段仍然有效。
//
#define NOUMEN_NTCF_NAME_CHARS     128

#define NOUMEN_NTCF_FLAG_TRUNCATED 0x01
#define NOUMEN_NTCF_FLAG_FAULTED   0x02

//
// 安装 / 移除 / 读日志共用这一个请求。`READ_LOG` 时 `MaxRecords` 是本次最多
// 返回的条数（0 = 由 OutputLength 决定）；输出是
// `NOUMEN_NT_CREATE_FILE_RESPONSE` 紧跟 `Records` 条
// `NOUMEN_NT_CREATE_FILE_LOG`。
//
typedef struct _NOUMEN_NT_CREATE_FILE_REQUEST
{
    // NOUMEN_NTCF_ACTION_*。
    ULONG Action;
    // READ_LOG：最多返回多少条记录（0 = 按 OutputLength 截断）。
    ULONG MaxRecords;
    // 处理器选择，规则与 ping 请求相同。
    ULONG Processor;
    ULONG Reserved;
} NOUMEN_NT_CREATE_FILE_REQUEST, *PNOUMEN_NT_CREATE_FILE_REQUEST;

//
// 一次 NtCreateFile 命中。handler 在 guest 内核里执行，只读参数和调用者
// 缓冲区、写这条记录，不分配内存。
//
typedef struct _NOUMEN_NT_CREATE_FILE_LOG
{
    // 从 1 开始严格递增；断号表示中间记录被环形缓冲覆盖。
    UINT64 Sequence;
    // 命中时的 RDTSC。
    UINT64 Tsc;
    ULONG ProcessId;
    ULONG ThreadId;
    // NtCreateFile 的第 2 个参数。
    ULONG DesiredAccess;
    // 第 8 个参数：FILE_SUPERSEDE(0) .. FILE_OVERWRITE_IF(5)。
    ULONG CreateDisposition;
    // 第 9 个参数，FILE_*_ON/NO_BUFFERING 等。
    ULONG CreateOptions;
    // 第 6 个参数。
    ULONG FileAttributes;
    // 第 7 个参数。
    ULONG ShareAccess;
    // `ObjectName` 的有效字节数（不含结尾 NUL），0 = 没有名字。
    ULONG NameLength;
    WCHAR ObjectName[NOUMEN_NTCF_NAME_CHARS];
    ULONG Flags;
    ULONG Reserved;
} NOUMEN_NT_CREATE_FILE_LOG, *PNOUMEN_NT_CREATE_FILE_LOG;

typedef struct _NOUMEN_NT_CREATE_FILE_RESPONSE
{
    // hook 的 NtCreateFile 地址（移除后仍是最后一次安装的地址）。
    UINT64 Address;
    // 驱动的 handler 地址；trampoline 会 `call` 它。
    UINT64 Handler;
    // NonPagedPool trampoline。安装时分配，之后复用，卸载驱动时释放。
    UINT64 Trampoline;
    // hypervisor 的原始 hook 状态：0 = 成功，否则是 HookError 编号。
    UINT64 Reply;
    // 累计命中次数（环形缓冲覆盖后仍然增长）。
    UINT64 TotalRecords;
    // 执行安装/移除/读取的处理器。
    ULONG Processor;
    // READ_LOG 写入的记录条数；安装/移除为 0。
    ULONG Records;
    // 驱动侧的安装状态：1 = 已安装。uapp 用它恢复界面状态。
    ULONG Installed;
    ULONG Reserved;
} NOUMEN_NT_CREATE_FILE_RESPONSE, *PNOUMEN_NT_CREATE_FILE_RESPONSE;


typedef struct _NOUMEN_SET_IMAGE_PATH
{
    ULONG Length;
    ULONG Reserved;
    WCHAR Path[NOUMEN_MAX_PATH];
} NOUMEN_SET_IMAGE_PATH, *PNOUMEN_SET_IMAGE_PATH;

typedef struct _NOUMEN_SET_LOG_PATH
{
    ULONG Length;
    ULONG Reserved;
    WCHAR Path[NOUMEN_MAX_PATH];
} NOUMEN_SET_LOG_PATH, *PNOUMEN_SET_LOG_PATH;

// Windows 内核结构偏移（EPROCESS/KTHREAD），由 uapp 按 build 号选表后填入。
// 和 hypervisor 的 `kargs::WinOffsets` 逐字段对应；Build == 0 表示未提供，
// 此时依赖偏移的进程检查（TSC 选择性隐藏）不生效。
typedef struct _NOUMEN_WIN_OFFSETS
{
    UINT32 Build;
    UINT32 ListBlink;
    UINT32 EprocLink;
    UINT32 KprocDtb;
    UINT32 EprocPid;
    UINT32 EprocName;
    UINT32 EprocPeb;
    UINT32 EprocSectionBase;
    UINT32 EprocExitStatus;
    UINT32 EprocThreadList;
    UINT32 EprocWow64;
    UINT32 EprocVadRoot;
    // `_KTHREAD.ApcState.Process` 偏移：当前进程 = gs_base + 本字段。
    UINT32 KthreadProcess;
    UINT32 Reserved5;
} NOUMEN_WIN_OFFSETS, *PNOUMEN_WIN_OFFSETS;

typedef struct _NOUMEN_KERNEL_ARGS
{
    // 必须等于 NOUMEN_KARGS_VERSION；hypervisor 先校验它再解释其它字段。
    UINT32 Version;
    UINT8  CpuCount;
    UINT8  Reserved0[3];
    // 物理内存区间个数（0 = 未提供）。
    UINT32 PhysMapCount;
    UINT32 Reserved1;
    // nonpaged PHYSICAL_MEMORY_RANGE[] 的内核 VA，驱动保活。
    UINT64 PhysMapBase;
    UINT64 SystemCr3;
    UINT64 SystemCr3Virt;
    // noumen 镜像基址（不是 ntoskrnl）。
    UINT64 KernelBase;
    UINT64 SymbolBase;
    UINT64 StringBase;
    UINT64 LogBase;
    UINT32 KernelSize;
    UINT32 SymbolSize;
    UINT32 StringSize;
    UINT32 LogSize;
    UINT16 SymbolCount;
    UINT16 Reserved2;
    UINT32 Reserved3;
    UINT64 KernelPfnBase;
    UINT32 KernelPfnCount;
    UINT32 Reserved4;
    // v4：Windows 内核结构偏移表（TSC 选择性隐藏的进程检查要用）。
    NOUMEN_WIN_OFFSETS WinOffsets;
} NOUMEN_KERNEL_ARGS, *PNOUMEN_KERNEL_ARGS;

typedef struct _NOUMEN_OS_INFO
{
    // 结构版本，当前 1。
    ULONG Version;
    // ntoskrnl!NtBuildNumber（0 = 未知）。
    ULONG Build;
    // NOUMEN_OS_INFO_FLAG_*。
    ULONG Flags;
    ULONG Reserved;
    // ntoskrnl.exe 的内核虚拟基址与映像大小（0 = 未定位）。
    UINT64 NtosBase;
    UINT64 NtosSize;
    // System 的 EPROCESS（0 = 未知）。
    UINT64 SystemEprocess;
} NOUMEN_OS_INFO, *PNOUMEN_OS_INFO;

#define NOUMEN_OS_INFO_VERSION        1
#define NOUMEN_OS_INFO_FLAG_NTOS      0x01
#define NOUMEN_OS_INFO_FLAG_EPROCESS  0x02

typedef struct _NOUMEN_INFO
{
    ULONG ProtocolVersion;
    ULONG State;
    NTSTATUS LastStatus;
    ULONG CpuCount;
    ULONG StartProcessor;
    ULONG SingleCore;
    UINT64 KernelBase;
    UINT64 KernelSize;
    UINT64 EntryPoint;
    UINT64 SymbolBase;
    ULONG SymbolCount;
    ULONG LogSize;
    UINT64 HypercallKey;
} NOUMEN_INFO, *PNOUMEN_INFO;

#pragma pack(pop)

#ifdef _KERNEL_MODE

typedef
NTSTATUS
(*NOUMEN_COMMAND_HANDLER)(
    _In_reads_bytes_(InputLength) const VOID *Input,
    _In_ ULONG InputLength,
    _Out_writes_bytes_(OutputLength) VOID *Output,
    _In_ ULONG OutputLength,
    _Out_ PULONG Written
    );

NTSTATUS
NoumenCommandDispatch(
    _In_ ULONG Command,
    _In_reads_bytes_(InputLength) const VOID *Input,
    _In_ ULONG InputLength,
    _Out_writes_bytes_(OutputLength) VOID *Output,
    _In_ ULONG OutputLength,
    _Out_ PULONG Written
    );

#endif
