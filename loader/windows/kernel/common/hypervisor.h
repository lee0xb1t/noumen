#pragma once

#include <ntddk.h>
#include "elf.h"
#include "elfloader.h"
#include "command.h"

#define NOUMEN_MAX_PROCESSORS     64

typedef struct _NOUMEN_HYPERVISOR_STATE
{
    volatile LONG State;
    NTSTATUS LastStatus;
    ULONG CpuCount;
    ULONG StartProcessor;
    BOOLEAN SingleCore;
    UINT64 HypercallKey;
    BOOLEAN EntryInvoked;
    BOOLEAN KernelArgsInitialized;
    BOOLEAN ImageFromMemory;
    NOUMEN_KERNEL_ARGS KernelArgs;
    //
    // uapp 通过 `NoumenCommandSetWinOffsets` 下发的偏移表；`Start` 里合并进
    // `KernelArgs.WinOffsets`。`WinOffsetsProvided` 为 FALSE 时保持全 0
    // （hypervisor 侧据此认为“没有偏移表”，进程检查按不匹配处理）。
    //
    BOOLEAN WinOffsetsProvided;
    NOUMEN_WIN_OFFSETS WinOffsets;
    NOUMEN_ELF_IMAGE Image;
} NOUMEN_HYPERVISOR_STATE, *PNOUMEN_HYPERVISOR_STATE;

extern NOUMEN_HYPERVISOR_STATE g_NoumenHypervisor;
extern UNICODE_STRING g_NoumenImagePath;

int
call_elf2(
    _In_ void *Function,
    _In_ void *Parameter1,
    _In_ void *Parameter2
    );

VOID
NoumenHypervisorInitialize(
    VOID
    );

NTSTATUS
NoumenHypervisorStart(
    _In_ BOOLEAN SingleCore,
    _In_ BOOLEAN RandomizeCore
    );

//
// Devirtualizes every CPU that was virtualized (one affinity-pinned STOP
// hypercall per CPU). On success the state goes back to Unloaded and the image
// is freed on driver unload; a later Start requires reloading the driver.
//
NTSTATUS
NoumenHypervisorStop(
    VOID
    );

//
// Issues VMCALL(PING) on a processor that runs the guest and returns its raw
// reply. `Processor` is either a concrete processor number or
// NOUMEN_HYPERCALL_PROCESSOR_ANY, in which case the driver selects one:
// StartProcessor in single-core mode, the calling processor in multi-core mode
// (StartProcessor if the caller was not virtualized).
//
// A VMCALL from a processor that is not in VMX non-root operation raises #UD
// (or fails in VMX root operation without a VM exit), so the selection and the
// instruction must happen together. `ActualProcessor` may be NULL when the
// caller does not need the selected number. Returns STATUS_UNSUCCESSFUL when
// the calling thread could not be moved to the selected processor; the VMCALL
// is then not executed.
//
NTSTATUS
NoumenHypervisorHypercallPing(
    _In_ UINT64 Nonce,
    _In_ ULONG Processor,
    _Out_ PUINT64 Reply,
    _Out_opt_ PULONG ActualProcessor
    );

//
// 在运行 guest 的处理器上安装或移除 `VirtualAddress` 处的 EPT hook / 隐藏
// 断点；处理器选择方式与 ping hypercall 相同。`Kind` 是
// NOUMEN_HOOK_KIND_BREAKPOINT / USER_BREAKPOINT / DETOUR_INTERNAL / HIDE /
// DETOUR_TRAMPOLINE，`Remove` 置位时忽略。
//
// `ProcessId` 非 0 时，loader 查找进程、attach 到它、probe 并锁住目标页，并在
// 发 VMCALL 期间保持 attach，因此 hypervisor 看到的是目标进程的 CR3。r3 地址
// 必须这样做。锁页还会在 hook 存活期间固定该页，使页框号在换出/换入后不变。
//
// `DETOUR_TRAMPOLINE` 是驱动自有的 NtCreateFile proxy hook：驱动分配 origin
// 页、把 proxy（NoumenNtCreateFileHandler）和 origin VA 交给 hypervisor；
// `Handler` 参数被忽略，`Trampoline` 是驱动填好的 origin 页基址。
//
// `Reply` 接收 hypervisor 的原始 hook 状态：0 成功，否则是 command.h 里的
// HookError 编号。非 0 状态仍算传输成功。
//
NTSTATUS
NoumenHypervisorEptHook(
    _In_ UINT64 VirtualAddress,
    _In_ ULONG Kind,
    _In_ BOOLEAN Remove,
    _In_ ULONG Processor,
    _In_ ULONG ProcessId,
    _In_ UINT64 Handler,
    _In_ UINT64 Trampoline,
    _Out_ PUINT64 Reply,
    _Out_opt_ PULONG ActualProcessor
    );

//
// SVM/NPT hook with an explicit debug method (NOUMEN_SVM_METHOD_*).
// DR breakpoints use the linear VA directly and do not need a locked page.
//
// `Method == NOUMEN_SVM_METHOD_NPT_HOOK`（5）是 NPT 函数 hook：`Handler` 和
// `Trampoline` 必须是**内核** VA，且 trampoline 必须是非分页可执行内存——
// hypervisor 会往里写代码。这两个地址只有驱动能分配，所以正常入口是
// `NoumenHypervisorHookNtCreateFile`（命令 14）；这里保留通用入口是为了以后
// 加别的目标函数。
//
NTSTATUS
NoumenHypervisorSvmHook(
    _In_ UINT64 VirtualAddress,
    _In_ UINT64 Handler,
    _In_ UINT64 Trampoline,
    _In_ ULONG Method,
    _In_ ULONG Kind,
    _In_ BOOLEAN Remove,
    _In_ ULONG Processor,
    _In_ ULONG ProcessId,
    _Out_ PUINT64 Reply,
    _Out_opt_ PULONG ActualProcessor
    );

//
// 解析 ntoskrnl 导出的 NtCreateFile 地址，供 uapp 安装测试断点。
//
NTSTATUS
NoumenHypervisorResolveNtCreateFile(
    _Out_ PUINT64 Address
    );

//
// NtCreateFile 的 NPT 函数 hook（`NoumenCommandHookNtCreateFile` 的实现）。
//
// `Action` 是 NOUMEN_NTCF_ACTION_INSTALL / REMOVE / READ_LOG：
//
// - INSTALL：解析 `NtCreateFile`，确保 NonPagedPool trampoline 已分配，
//   然后发 hypercall 0x3A，把 handler/trampoline 交给 SVM 后端。返回的
//   `Response->Reply` 是 hypervisor 的 HookError（0 = 成功）；非 0 也是
//   “传输成功”，由调用方展示。
// - REMOVE：发 hypercall 0x31，成功则清除安装状态。trampoline 保留复用。
// - READ_LOG：把环形缓冲里最新的 `MaxRecords` 条记录复制到 `Records`。
//
// handler 在 guest 内核里执行，只记录 ObjectName/DesiredAccess/
// CreateDisposition 等参数，不阻塞、不修改调用。详细栈布局见 hypervisor.c。
//
NTSTATUS
NoumenHypervisorHookNtCreateFile(
    _In_ ULONG Action,
    _In_ ULONG Processor,
    _In_ ULONG MaxRecords,
    _Out_ PNOUMEN_NT_CREATE_FILE_RESPONSE Response,
    _Out_writes_bytes_opt_(MaxRecords * sizeof(NOUMEN_NT_CREATE_FILE_LOG)) PNOUMEN_NT_CREATE_FILE_LOG Records,
    _Out_ PULONG RecordCount
    );

//
// 从 guest 视角读 hypervisor 镜像的 KernelBase（走 guest 页表 + EPT）。
// HideSelf 之后应读到全 0；root 侧 READ_SELF hypercall 才读真实页。
//
NTSTATUS
NoumenHypervisorReadKernelImage(
    _In_ ULONG Offset,
    _In_ ULONG Length,
    _Out_writes_bytes_(Length) PVOID Output
    );

//
// OS 引导信息：ntoskrnl 基址/大小、NtBuildNumber、System EPROCESS。
//
// 操作系统语义由 uapp 负责；驱动只提供这一份“启动线索”，并且**不放进
// KernelArgs**（那是 hypervisor 的内存层契约）。
//
NTSTATUS
NoumenHypervisorGetOsInfo(
    _Out_ PNOUMEN_OS_INFO Info
    );

NTSTATUS
NoumenHypervisorGetKernelArgs(
    _Out_ PNOUMEN_KERNEL_ARGS Args
    );

NTSTATUS
NoumenHypervisorSetKernelArgs(
    _In_ const NOUMEN_KERNEL_ARGS *Args
    );

//
// 保存 uapp 按 build 选好的 Windows 结构偏移表；在下次 `Start` 时合并进
// `KernelArgs.WinOffsets`。`Build == 0` 视为清除。
//
NTSTATUS
NoumenHypervisorSetWinOffsets(
    _In_ const NOUMEN_WIN_OFFSETS *Offsets
    );

NTSTATUS
NoumenHypervisorSetImagePath(
    _In_reads_bytes_(Length) const WCHAR *Path,
    _In_ ULONG Length
    );

NTSTATUS
NoumenHypervisorLoadImageFromMemory(
    _In_reads_bytes_(Size) const VOID *Data,
    _In_ ULONG Size
    );

VOID
NoumenHypervisorCleanup(
    VOID
    );

