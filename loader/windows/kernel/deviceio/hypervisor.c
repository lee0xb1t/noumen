#include "driver.h"

#define NOUMEN_PHYS_MAP 1

C_ASSERT(sizeof(NOUMEN_KERNEL_ARGS) == 168);

#define NOUMEN_IMAGE_PATH_CHARS (NOUMEN_MAX_PATH + 8)

NOUMEN_HYPERVISOR_STATE g_NoumenHypervisor;

static WCHAR g_NoumenImagePathBuffer[NOUMEN_IMAGE_PATH_CHARS];
UNICODE_STRING g_NoumenImagePath;
static const WCHAR g_DefaultImagePath[] = L"\\??\\C:\\noumen";

static KMUTEX g_HypervisorLock;

//
// 内核镜像所有页的 PFN。表由 ELF loader 追加在镜像尾部，位于
// ImageBase..ImageBase+ImageSize 内；HideSelf 会连同镜像页一起隐藏它。
//
static PVOID g_KernelPfnList;
static ULONG g_KernelPfnCount;

static
VOID
NoumenFreeKernelPfnList(
    VOID
    )
{
    g_KernelPfnList = NULL;
    g_KernelPfnCount = 0;
    g_NoumenHypervisor.KernelArgs.KernelPfnBase = 0;
    g_NoumenHypervisor.KernelArgs.KernelPfnCount = 0;
}

static
NTSTATUS
NoumenBuildKernelPfnList(
    VOID
    )
{
    NoumenFreeKernelPfnList();

    if (g_NoumenHypervisor.Image.PfnTable == NULL ||
        g_NoumenHypervisor.Image.PfnCount == 0) {
        return STATUS_INVALID_IMAGE_FORMAT;
    }

    g_KernelPfnList = g_NoumenHypervisor.Image.PfnTable;
    g_KernelPfnCount = g_NoumenHypervisor.Image.PfnCount;

    return STATUS_SUCCESS;
}

//
// 目标页在另一个进程里被 probe + 锁页的 hook 记录。MDL 让页保持驻留（PFN
// 不会变化），进程引用让 EPROCESS 在记录释放前不会被回收。
//
#define NOUMEN_MAX_EPT_HOOKS 32
#define NOUMEN_EPT_HOOK_TAG  'kHmN'

typedef struct _NOUMEN_EPT_HOOK_RECORD
{
    BOOLEAN InUse;
    ULONG ProcessId;
    UINT64 VirtualAddress;
    UINT64 PagePhysical;
    PMDL Mdl;
    PEPROCESS Process;
} NOUMEN_EPT_HOOK_RECORD, *PNOUMEN_EPT_HOOK_RECORD;

static NOUMEN_EPT_HOOK_RECORD g_EptHookRecords[NOUMEN_MAX_EPT_HOOKS];
static KMUTEX g_EptHookLock;

//
// SVM/NPT byte-patch hooks keep their own locked-page records. They are kept
// separate from the EPT list so the two backends never share teardown state.
//
typedef struct _NOUMEN_SVM_HOOK_RECORD
{
    BOOLEAN InUse;
    ULONG ProcessId;
    UINT64 VirtualAddress;
    UINT64 PagePhysical;
    PMDL Mdl;
    PEPROCESS Process;
} NOUMEN_SVM_HOOK_RECORD, *PNOUMEN_SVM_HOOK_RECORD;

static NOUMEN_SVM_HOOK_RECORD g_SvmHookRecords[NOUMEN_MAX_EPT_HOOKS];
static KMUTEX g_SvmHookLock;

//
// ---- NtCreateFile hooks --------------------------------------------------
//
// 两条路径共用下面的命中日志，但执行模型不同：
//
// 1) SVM 命令 14（0x3A）：“影子页 INT3 + trampoline”。驱动分配一页
//    NonPagedPool，hypervisor 写入“保存寄存器 → call handler → 执行被覆盖的
//    原指令 → 跳回 NtCreateFile+stolen”；handler 只记录，原函数在 trampoline
//    里继续执行。
//
//    legacy handler（NoumenNtCreateFileLegacyHandler，guest 内核代码）
//        ↑ call
//    NonPagedPool trampoline
//
// 2) VMX EPT DetourTrampoline：代理见下面的 NOUMEN_EPT_PROXY_STATE。
//    驱动提供 proxy 和 origin 页，proxy 调 origin 执行原函数并透传返回值。
//
// SVM trampoline 必须是 guest 可执行的非分页内存：安装时分配，之后复用。
// **不能**在移除 hook 时立刻释放——其它 vCPU 可能还在里面执行 handler 或跳回
// 指令。释放时机见 `NoumenNtCreateFileRelease`。VMX origin 的释放时机见
// `NoumenEptProxyHookRemove`。
//
#define NOUMEN_NTCF_LOG_CAPACITY 64
#define NOUMEN_NTCF_POOL_TAG 'fCtN'

typedef struct _NOUMEN_NTCF_STATE
{
    BOOLEAN Installed;
    UINT64 Address;
    UINT64 Trampoline;
} NOUMEN_NTCF_STATE, *PNOUMEN_NTCF_STATE;

static NOUMEN_NTCF_STATE g_Ntcf;
static KMUTEX g_NtcfStateLock;

//
// 命中日志：64 条环形缓冲。handler 在 guest 内核里用自旋锁写，IOCTL 读快照。
// 缓冲区在驱动加载时分配（约 20 KiB）；分配失败只会让安装返回
// STATUS_INSUFFICIENT_RESOURCES，其它命令照常工作。
//
static PNOUMEN_NT_CREATE_FILE_LOG g_NtcfLog;
static KSPIN_LOCK g_NtcfLogLock;
static volatile LONG64 g_NtcfSequence;

//
// ---- NtCreateFile EPT proxy（VMX DetourTrampoline）-----------------------
//
// 和命令 14 的 SVM/NPT 版本不同，这条路径不改真实页、也不用 0xCC：
//
//   目标页 fake 副本入口：jmp NoumenNtCreateFileHandler（proxy）
//   proxy：记录参数 → call g_Ept.Origin → 把返回值原样返回
//   g_Ept.Origin（NonPagedPool 页，数据读全 0、执行走真实页）：
//       被覆盖的原函数指令 + jmp NtCreateFile+stolen
//
// 页由驱动分配并保存 base VA；unhook 时先恢复入口、等所有核 retire，再等
// 在 origin 里的调用全部退出（InFlight），最后释放。
//
#define NOUMEN_EPT_PROXY_POOL_TAG 'gEtN'

typedef
NTSTATUS
(NTAPI *NOUMEN_NTCF_ORIGIN)(
    _Out_ PHANDLE FileHandle,
    _In_ ULONG DesiredAccess,
    _In_opt_ POBJECT_ATTRIBUTES ObjectAttributes,
    _Out_ PIO_STATUS_BLOCK IoStatusBlock,
    _In_opt_ PLARGE_INTEGER AllocationSize,
    _In_ ULONG FileAttributes,
    _In_ ULONG ShareAccess,
    _In_ ULONG CreateDisposition,
    _In_ ULONG CreateOptions,
    _In_opt_ PVOID EaBuffer,
    _In_ ULONG EaLength
    );

typedef struct _NOUMEN_EPT_PROXY_STATE
{
    volatile LONG InFlight;
    volatile LONG Busy;
    volatile LONG Closing;
    BOOLEAN Installed;
    BOOLEAN OriginHidden;
    BOOLEAN Stale;
    UINT64 Target;
    UINT64 Proxy;
    UINT64 Origin;
    UINT64 OriginRaw;
} NOUMEN_EPT_PROXY_STATE, *PNOUMEN_EPT_PROXY_STATE;

static NOUMEN_EPT_PROXY_STATE g_Ept;
static KMUTEX g_EptLock;

__declspec(guard(nocf))
static
NTSTATUS
NTAPI
NoumenNtCreateFileHandler(
    _Out_ PHANDLE FileHandle,
    _In_ ULONG DesiredAccess,
    _In_opt_ POBJECT_ATTRIBUTES ObjectAttributes,
    _Out_ PIO_STATUS_BLOCK IoStatusBlock,
    _In_opt_ PLARGE_INTEGER AllocationSize,
    _In_ ULONG FileAttributes,
    _In_ ULONG ShareAccess,
    _In_ ULONG CreateDisposition,
    _In_ ULONG CreateOptions,
    _In_opt_ PVOID EaBuffer,
    _In_ ULONG EaLength
    );

static
NTSTATUS
NoumenEptProxyHookInstall(
    _In_ UINT64 VirtualAddress,
    _In_ ULONG Processor,
    _Out_ PUINT64 Reply,
    _Out_opt_ PULONG ActualProcessor
    );

static
NTSTATUS
NoumenEptProxyHookRemove(
    _In_ UINT64 VirtualAddress,
    _In_ ULONG Processor,
    _Out_ PUINT64 Reply,
    _Out_opt_ PULONG ActualProcessor
    );

static
VOID
NoumenEptProxyRelease(
    _In_ BOOLEAN RemoveFromHypervisor
    );

static
VOID
NoumenNtCreateFileRelease(
    _In_ BOOLEAN RemoveFromHypervisor
    );

//
// 移除所有已跟踪的 hook（并释放它们锁住的页）。定义在 hypercall 辅助函数
// 之后；Stop 需要在拿 g_HypervisorLock 之前调用它。
//
static
VOID
NoumenEptHooksReleaseAll(
    _In_ BOOLEAN RemoveFromHypervisor
    );

static
VOID
NoumenSvmHooksReleaseAll(
    _In_ BOOLEAN RemoveFromHypervisor
    );

//
// 移除属于某个进程的 hook；由进程退出通知调用，避免已终止的目标继续锁着
// 自己的页。
//
static
VOID
NoumenEptHooksReleasePid(
    _In_ ULONG ProcessId
    );

static
VOID
NoumenEptProcessExitNotify(
    _In_ PEPROCESS Process,
    _In_ HANDLE ProcessId,
    _In_opt_ PPS_CREATE_NOTIFY_INFO CreateInfo
    )
{
    UNREFERENCED_PARAMETER(Process);

    //
    // 进程创建和退出共用这个回调；这里只关心退出。
    // 文档规定回调在 PASSIVE_LEVEL；如果将来在更高 IRQL 运行，就不能拿
    // mutex 或发 VMCALL，此时跳过清理。
    //
    if (CreateInfo != NULL) {
        return;
    }

    if (KeGetCurrentIrql() != PASSIVE_LEVEL) {
        return;
    }

    NoumenEptHooksReleasePid(HandleToUlong(ProcessId));
}

static
NTSTATUS
NoumenInitProcessor(
    _In_ ULONG ProcessorIndex,
    _Out_ int *Result
    )
{
    KAFFINITY previousAffinity;

    if (Result == NULL) {
        return STATUS_INVALID_PARAMETER;
    }

    previousAffinity = KeSetSystemAffinityThreadEx((KAFFINITY)(1ULL << ProcessorIndex));

    *Result = call_elf2(
        g_NoumenHypervisor.Image.InitFunction,
        (VOID *)(ULONG_PTR)ProcessorIndex,
        NULL);

    KeRevertToUserAffinityThreadEx(previousAffinity);

    return STATUS_SUCCESS;
}

static
NTSTATUS
NoumenStartLocked(
    _In_ BOOLEAN SingleCore,
    _In_ ULONG ProcessorIndex,
    _In_ ULONG CpuCount
    );

typedef struct _NOUMEN_START_CONTEXT
{
    KEVENT Completion;
    NTSTATUS Status;
    BOOLEAN SingleCore;
    ULONG ProcessorIndex;
    ULONG CpuCount;
} NOUMEN_START_CONTEXT, *PNOUMEN_START_CONTEXT;

static
VOID
NoumenStartThread(
    _In_ PVOID Context
    )
{
    PNOUMEN_START_CONTEXT startContext;
    KAFFINITY previousAffinity;

    startContext = (PNOUMEN_START_CONTEXT)Context;

    previousAffinity = KeSetSystemAffinityThreadEx(
        (KAFFINITY)(1ULL << startContext->ProcessorIndex));

    g_NoumenHypervisor.StartProcessor = KeGetCurrentProcessorNumber();

    startContext->Status = NoumenStartLocked(
        startContext->SingleCore,
        startContext->ProcessorIndex,
        startContext->CpuCount);

    KeRevertToUserAffinityThreadEx(previousAffinity);

    KeSetEvent(&startContext->Completion, IO_NO_INCREMENT, FALSE);
    PsTerminateSystemThread(STATUS_SUCCESS);
}

VOID
NoumenHypervisorInitialize(
    VOID
    )
{
    RtlZeroMemory(&g_NoumenHypervisor, sizeof(g_NoumenHypervisor));

    g_NoumenHypervisor.State = NoumenStateUnloaded;
    g_NoumenHypervisor.LastStatus = STATUS_SUCCESS;
    g_NoumenHypervisor.HypercallKey = NOUMEN_HYPERCALL_KEY;
    g_NoumenHypervisor.KernelArgsInitialized = FALSE;
    g_NoumenHypervisor.WinOffsetsProvided = FALSE;
    RtlZeroMemory(&g_NoumenHypervisor.WinOffsets, sizeof(g_NoumenHypervisor.WinOffsets));
    g_NoumenHypervisor.ImageFromMemory = FALSE;

    RtlZeroMemory(g_NoumenImagePathBuffer, sizeof(g_NoumenImagePathBuffer));
    RtlCopyMemory(g_NoumenImagePathBuffer, g_DefaultImagePath, sizeof(g_DefaultImagePath));

    g_NoumenImagePath.Buffer = g_NoumenImagePathBuffer;
    g_NoumenImagePath.Length = (USHORT)(sizeof(g_DefaultImagePath) - sizeof(WCHAR));
    g_NoumenImagePath.MaximumLength = (USHORT)sizeof(g_DefaultImagePath);

    KeInitializeMutex(&g_HypervisorLock, 0);
    KeInitializeMutex(&g_EptHookLock, 0);
    KeInitializeMutex(&g_SvmHookLock, 0);

    RtlZeroMemory(g_EptHookRecords, sizeof(g_EptHookRecords));

    //
    // NtCreateFile hook 的状态与命中日志。日志缓冲在这里分配一次；分配失败
    // 不影响驱动加载，只让安装命令返回 STATUS_INSUFFICIENT_RESOURCES。
    //
    RtlZeroMemory(&g_Ntcf, sizeof(g_Ntcf));
    KeInitializeMutex(&g_NtcfStateLock, 0);
    KeInitializeSpinLock(&g_NtcfLogLock);
    g_NtcfSequence = 0;

    RtlZeroMemory(&g_Ept, sizeof(g_Ept));
    KeInitializeMutex(&g_EptLock, 0);

    g_NtcfLog = ExAllocatePoolWithTag(
        NonPagedPoolNx,
        sizeof(NOUMEN_NT_CREATE_FILE_LOG) * NOUMEN_NTCF_LOG_CAPACITY,
        NOUMEN_NTCF_POOL_TAG);

    if (g_NtcfLog != NULL) {
        RtlZeroMemory(
            g_NtcfLog,
            sizeof(NOUMEN_NT_CREATE_FILE_LOG) * NOUMEN_NTCF_LOG_CAPACITY);
    }

    //
    // 目标进程可能在页被锁住并 hook 期间退出。回调会移除 hook 并解锁 MDL。
    // 不满足 notify-routine 节要求的驱动可能注册失败；此时 Stop/卸载仍会
    // 清理 hook。
    //
    (VOID)PsSetCreateProcessNotifyRoutineEx(NoumenEptProcessExitNotify, FALSE);
}

static
ULONG
NoumenActiveProcessorCount(
    VOID
    )
{
    ULONG count;

    count = KeQueryActiveProcessorCount(NULL);

    if (count == 0) {
        count = 1;
    }

    if (count > NOUMEN_MAX_PROCESSORS) {
        count = NOUMEN_MAX_PROCESSORS;
    }

    return count;
}

static
ULONG
NoumenSelectProcessor(
    _In_ BOOLEAN RandomizeCore,
    _In_ ULONG ProcessorCount
    )
{
    LARGE_INTEGER counter;

    if (!RandomizeCore || ProcessorCount <= 1) {
        return 0;
    }

    counter = KeQueryPerformanceCounter(NULL);

    return (ULONG)((ULONG64)counter.QuadPart % ProcessorCount);
}

//
// ---- OS 引导信息 ---------------------------------------------------------
//
// 只填 `KernelArgs`，不解析、不遍历：hypervisor 不解释这些字段，uapp 通过
// `GetKernelArgs` 读到同一份后自己做偏移表、EPROCESS/PEB 遍历。
//
// 为什么用“从 &PsInitialSystemProcess 往下扫 MZ”而不是 PsLoadedModuleList：
// 后者没有稳定的 WDK 类型（KLDR_DATA_TABLE_ENTRY 在 km 头里不可见），而扫
// 页对齐的 MZ + PE 签名不依赖任何结构布局，代价只有几十次内存读。
//


//
// 把物理区间表写进镜像尾部的预留区（`PhysMapSize == 0` 时什么也不做，
// 于是 `PhysMapCount` 保持 0，`KernelArgs.PhysMapBase` 填 0）。
//
static
VOID
NoumenStorePhysMap(
    _Inout_ PNOUMEN_ELF_IMAGE Image,
    _In_ const PHYSICAL_MEMORY_RANGE *Ranges,
    _In_ ULONG Count
    )
{
    if (Image == NULL || Ranges == NULL || Count == 0 || Image->PhysMapSize == 0) {
        return;
    }

    RtlCopyMemory(
        (PUCHAR)Image->ImageBase + Image->PhysMapOffset,
        Ranges,
        (SIZE_T)Count * sizeof(PHYSICAL_MEMORY_RANGE));

    Image->PhysMapCount = Count;
}
static
NTSTATUS
NoumenStartLocked(
    _In_ BOOLEAN SingleCore,
    _In_ ULONG ProcessorIndex,
    _In_ ULONG CpuCount
    )
{
    NOUMEN_KERNEL_ARGS kernelArgs;
    PHYSICAL_ADDRESS cr3Physical;
    PHYSICAL_MEMORY_RANGE *ranges;
    ULONG physCount;
    ULONG physBytes;
    NTSTATUS status;
    ULONG i;
    int result;

    InterlockedExchange(&g_NoumenHypervisor.State, NoumenStateLoading);
    g_NoumenHypervisor.LastStatus = STATUS_SUCCESS;

    if (!g_NoumenHypervisor.ImageFromMemory) {
        NoumenElfFree(&g_NoumenHypervisor.Image);

#if NOUMEN_PHYS_MAP
        ranges = MmGetPhysicalMemoryRanges();
        physCount = 0;

        while (ranges != NULL && physCount < NOUMEN_MAX_PHYS_RANGES &&
               ranges[physCount].NumberOfBytes.QuadPart != 0) {
            physCount++;
        }

        physBytes = physCount != 0
            ? (physCount + 1) * (ULONG)sizeof(PHYSICAL_MEMORY_RANGE)
            : 0;
#else
        ranges = NULL;
        physCount = 0;
        physBytes = 0;
#endif

        status = NoumenElfLoadFromFile(&g_NoumenImagePath, physBytes, &g_NoumenHypervisor.Image);

#if NOUMEN_PHYS_MAP
        if (NT_SUCCESS(status)) {
            NoumenStorePhysMap(&g_NoumenHypervisor.Image, ranges, physCount);
        }

        if (ranges != NULL) {
            ExFreePool(ranges);
            ranges = NULL;
        }
#endif


        if (!NT_SUCCESS(status)) {
            g_NoumenHypervisor.LastStatus = status;
            InterlockedExchange(&g_NoumenHypervisor.State, NoumenStateFailed);
            return status;
        }
    }

    if (g_NoumenHypervisor.Image.ImageBase == NULL ||
        g_NoumenHypervisor.Image.EntryPoint == NULL) {
        status = STATUS_INVALID_IMAGE_FORMAT;
        g_NoumenHypervisor.LastStatus = status;
        InterlockedExchange(&g_NoumenHypervisor.State, NoumenStateFailed);
        return status;
    }

    status = NoumenBuildKernelPfnList();

    if (!NT_SUCCESS(status)) {
        g_NoumenHypervisor.LastStatus = status;
        InterlockedExchange(&g_NoumenHypervisor.State, NoumenStateFailed);
        return status;
    }

    kernelArgs = g_NoumenHypervisor.KernelArgs;

    //
    // 无论这份参数来自 Start 的自动填充还是 SetKernelArgs，都盖一次版本号：
    // hypervisor 会先校验它，v1 的 96 字节结构与 v2 不能混用。
    //
    kernelArgs.Version = NOUMEN_KARGS_VERSION;

    if (!g_NoumenHypervisor.KernelArgsInitialized) {
        kernelArgs.SystemCr3 = __readcr3() & 0x000FFFFFFFFFF000ULL;

        cr3Physical.QuadPart = kernelArgs.SystemCr3;
        kernelArgs.SystemCr3Virt = (UINT64)(ULONG_PTR)MmGetVirtualForPhysical(cr3Physical);

        kernelArgs.KernelBase = (UINT64)(ULONG_PTR)g_NoumenHypervisor.Image.ImageBase;
        kernelArgs.KernelSize = g_NoumenHypervisor.Image.ImageSize;
        kernelArgs.SymbolBase = (UINT64)(ULONG_PTR)g_NoumenHypervisor.Image.SymbolTable;
        kernelArgs.SymbolSize = g_NoumenHypervisor.Image.SymbolTableSize;
        kernelArgs.SymbolCount = g_NoumenHypervisor.Image.SymbolCount;
        kernelArgs.StringBase = (UINT64)(ULONG_PTR)g_NoumenHypervisor.Image.StringTable;
        kernelArgs.StringSize = g_NoumenHypervisor.Image.StringTableSize;

        kernelArgs.LogBase = (UINT64)(ULONG_PTR)NoumenLogBuffer();
        kernelArgs.LogSize = NoumenLogBufferSize();

        kernelArgs.Version = NOUMEN_KARGS_VERSION;

        // 只填内存层参数：物理区间供 `MEM_MAP`。OS 语义由 uapp 负责。
        // PhysMap 可选：预留了尾部就指向镜像尾部，否则 Base = 0（未提供）。
        // hypervisor 的 MEM_MAP 在 Base = 0 时返回 0 段，物理访问退化为
        // pa < DIRECT_MAP_SIZE 直映射。
        if (g_NoumenHypervisor.Image.PhysMapCount != 0) {
            kernelArgs.PhysMapBase =
                (UINT64)(ULONG_PTR)g_NoumenHypervisor.Image.ImageBase +
                g_NoumenHypervisor.Image.PhysMapOffset;
            kernelArgs.PhysMapCount = g_NoumenHypervisor.Image.PhysMapCount;
        } else {
            kernelArgs.PhysMapBase = 0;
            kernelArgs.PhysMapCount = 0;
        }
    }

#if !NOUMEN_PANIC_DETAIL
    kernelArgs.SymbolBase = 0;
    kernelArgs.SymbolSize = 0;
    kernelArgs.SymbolCount = 0;
    kernelArgs.StringBase = 0;
    kernelArgs.StringSize = 0;
#endif

    kernelArgs.CpuCount = (UINT8)CpuCount;
    kernelArgs.KernelPfnBase = (UINT64)(ULONG_PTR)g_KernelPfnList;
    kernelArgs.KernelPfnCount = g_KernelPfnCount;

    //
    // uapp 若下发过偏移表，就并进 KernelArgs；没下发时保持全 0，hypervisor
    // 据此认为“没有偏移表”，依赖偏移的进程检查按不匹配处理（不隐藏）。
    //
    if (g_NoumenHypervisor.WinOffsetsProvided) {
        kernelArgs.WinOffsets = g_NoumenHypervisor.WinOffsets;
    } else {
        RtlZeroMemory(&kernelArgs.WinOffsets, sizeof(kernelArgs.WinOffsets));
    }

    g_NoumenHypervisor.CpuCount = CpuCount;
    g_NoumenHypervisor.SingleCore = SingleCore;
    g_NoumenHypervisor.KernelArgs = kernelArgs;
    g_NoumenHypervisor.KernelArgsInitialized = TRUE;

    g_NoumenHypervisor.EntryInvoked = TRUE;

    result = call_elf2(
        g_NoumenHypervisor.Image.EntryPoint,
        &g_NoumenHypervisor.KernelArgs,
        (VOID *)(ULONG_PTR)g_NoumenHypervisor.HypercallKey);

    if (result != 0) {
        status = STATUS_UNSUCCESSFUL;
        goto Failed;
    }

    if (g_NoumenHypervisor.Image.InitFunction != NULL) {
        if (SingleCore) {
            status = NoumenInitProcessor(ProcessorIndex, &result);

            if (!NT_SUCCESS(status)) {
                goto Failed;
            }

            if (result != 0) {
                status = STATUS_UNSUCCESSFUL;
                goto Failed;
            }
        } else {
            for (i = 0; i < CpuCount; i++) {
                status = NoumenInitProcessor(i, &result);

                if (!NT_SUCCESS(status)) {
                    goto Failed;
                }

                if (result != 0) {
                    status = STATUS_UNSUCCESSFUL;
                    goto Failed;
                }
            }
        }
    }

    g_NoumenHypervisor.LastStatus = STATUS_SUCCESS;
    InterlockedExchange(&g_NoumenHypervisor.State, NoumenStateRunning);

    return STATUS_SUCCESS;

Failed:
    g_NoumenHypervisor.LastStatus = status;
    InterlockedExchange(&g_NoumenHypervisor.State, NoumenStateFailed);

    return status;
}

NTSTATUS
NoumenHypervisorStop(
    VOID
    )
{
    KAFFINITY previousAffinity;
    NTSTATUS status;
    ULONG active;
    ULONG count;
    ULONG cpu;
    ULONG i;

    //
    // 在 hypervisor 仍在运行时移除已跟踪的 hook，然后释放它们锁住的页。
    // 这必须在拿 g_HypervisorLock 之前完成：每次移除 hypercall 都会拿它。
    //
    NoumenNtCreateFileRelease(TRUE);
    NoumenEptProxyRelease(TRUE);
    NoumenSvmHooksReleaseAll(TRUE);
    NoumenEptHooksReleaseAll(TRUE);

    status = KeWaitForMutexObject(&g_HypervisorLock, Executive, KernelMode, FALSE, NULL);

    if (!NT_SUCCESS(status)) {
        return status;
    }

    if (InterlockedCompareExchange(&g_NoumenHypervisor.State, 0, 0) != NoumenStateRunning) {
        KeReleaseMutex(&g_HypervisorLock, FALSE);
        return STATUS_INVALID_DEVICE_STATE;
    }

    //
    // Keep the log thread alive while the CPUs leave VMX, but stop it from
    // pinging: a VMCALL on an already devirtualized CPU would #UD. The thread
    // keeps flushing the log, so the breadcrumbs written by the hypervisor
    // survive a crash in the middle of the stop loop.
    //
    NoumenLogEnablePing(FALSE);
    (VOID)NoumenLogDumpToFile();

    active = KeQueryActiveProcessorCount(NULL);

    if (g_NoumenHypervisor.SingleCore) {
        // Only one CPU was virtualized (StartProcessor, which honours
        // RandomizeCore), so stop exactly once. Looping over CpuCount here would
        // VMCALL an already devirtualized CPU and #UD.
        count = 1;
    } else {
        count = g_NoumenHypervisor.CpuCount != 0 ? g_NoumenHypervisor.CpuCount : 1;
        if (count > active) {
            count = active;
        }
    }

    for (i = 0; i < count; i++) {
        // Single-core mode virtualized StartProcessor only; the rest never had VMX.
        cpu = g_NoumenHypervisor.SingleCore ? g_NoumenHypervisor.StartProcessor : i;

        if (cpu >= active) {
            continue;
        }

        previousAffinity = KeSetSystemAffinityThreadEx((KAFFINITY)(1ULL << cpu));

        //
        // Returns with this CPU already outside VMX operation, continuing right
        // after the STOP hypercall as ordinary Windows code.
        //
        (VOID)NoumenHypercallStop(g_NoumenHypervisor.HypercallKey);

        (VOID)NoumenLogDumpToFile();

        KeRevertToUserAffinityThreadEx(previousAffinity);
    }

    NoumenLogEnablePing(TRUE);
    NoumenLogStop();

    //
    // No CPU is in VMX operation any more: nothing will execute the image again,
    // so it may be freed when the driver unloads.
    //
    g_NoumenHypervisor.EntryInvoked = FALSE;
    g_NoumenHypervisor.LastStatus = STATUS_SUCCESS;
    NoumenFreeKernelPfnList();
    InterlockedExchange(&g_NoumenHypervisor.State, NoumenStateUnloaded);

    KeReleaseMutex(&g_HypervisorLock, FALSE);

    return STATUS_SUCCESS;
}

static
NTSTATUS
NoumenSelectPingProcessor(
    _In_ ULONG Requested,
    _Out_ PULONG Selected
    )
{
    ULONG count;
    ULONG cpu;

    count = g_NoumenHypervisor.CpuCount;

    if (Requested == NOUMEN_HYPERCALL_PROCESSOR_ANY) {
        if (g_NoumenHypervisor.SingleCore) {
            //
            // Only StartProcessor ever ran VMXON; a VMCALL anywhere else would
            // #UD.
            //
            cpu = g_NoumenHypervisor.StartProcessor;
        } else {
            //
            // Every effective processor (0 .. CpuCount-1) ran _init, so probe
            // the one that handles this request. Fall back to StartProcessor
            // when the caller runs outside that set: SetKernelArgs can make
            // CpuCount smaller than the number of active processors.
            //
            cpu = KeGetCurrentProcessorNumber();

            if (cpu >= count) {
                cpu = g_NoumenHypervisor.StartProcessor;
            }
        }
    } else {
        cpu = Requested;

        if (g_NoumenHypervisor.SingleCore) {
            if (cpu != g_NoumenHypervisor.StartProcessor) {
                return STATUS_INVALID_PARAMETER;
            }
        } else if (cpu >= count) {
            return STATUS_INVALID_PARAMETER;
        }
    }

    if (cpu >= sizeof(KAFFINITY) * 8) {
        return STATUS_INVALID_PARAMETER;
    }

    *Selected = cpu;

    return STATUS_SUCCESS;
}

static
BOOLEAN
NoumenPinCurrentThread(
    _In_ ULONG Processor,
    _Out_ PKAFFINITY PreviousAffinity
    )
{
    LARGE_INTEGER interval;
    ULONG attempt;

    //
    // Zero is a valid return value: it means this thread had no previous system
    // affinity, which is the normal state of a user thread. It is NOT a
    // failure, and KeRevertToUserAffinityThreadEx(0) restores the original
    // user affinity. Treating it as an error skipped the revert and left the
    // thread pinned to StartProcessor -- the first call failed and every later
    // call succeeded with the affinity stuck on that CPU.
    //
    *PreviousAffinity = KeSetSystemAffinityThreadEx((KAFFINITY)(1ULL << Processor));

    if (KeGetCurrentProcessorNumber() == Processor) {
        return TRUE;
    }

    //
    // The affinity mask is in place, but the thread can still be running on its
    // old processor until it is rescheduled. Yield at PASSIVE and re-check:
    // executing VMCALL before the move completes would run it on a processor
    // that never executed _init. At DISPATCH_LEVEL the move is deferred until
    // the IRQL drops, so it cannot be waited for here.
    //
    if (KeGetCurrentIrql() > APC_LEVEL) {
        return FALSE;
    }

    interval.QuadPart = -10000; // 1 ms

    for (attempt = 0; attempt < 50; attempt++) {
        (VOID)KeDelayExecutionThread(KernelMode, FALSE, &interval);

        if (KeGetCurrentProcessorNumber() == Processor) {
            return TRUE;
        }
    }

    return FALSE;
}

//
// 必须在已虚拟化的处理器上执行的 hypercall 的公共前奏：与 Start/Stop 串行化，
// 选择处理器，并把调用线程 pin 到那里。对应的
// NoumenHypervisorEndHypercall 负责释放。
//
static
NTSTATUS
NoumenHypervisorBeginHypercall(
    _In_ ULONG Processor,
    _Out_ PKAFFINITY PreviousAffinity,
    _Out_ PULONG SelectedProcessor
    )
{
    NTSTATUS status;

    status = KeWaitForMutexObject(&g_HypervisorLock, Executive, KernelMode, FALSE, NULL);

    if (!NT_SUCCESS(status)) {
        return status;
    }

    if (InterlockedCompareExchange(&g_NoumenHypervisor.State, 0, 0) != NoumenStateRunning) {
        KeReleaseMutex(&g_HypervisorLock, FALSE);
        return STATUS_INVALID_DEVICE_STATE;
    }

    status = NoumenSelectPingProcessor(Processor, SelectedProcessor);

    if (!NT_SUCCESS(status)) {
        KeReleaseMutex(&g_HypervisorLock, FALSE);
        return status;
    }

    if (!NoumenPinCurrentThread(*SelectedProcessor, PreviousAffinity)) {
        KeRevertToUserAffinityThreadEx(*PreviousAffinity);
        KeReleaseMutex(&g_HypervisorLock, FALSE);
        return STATUS_UNSUCCESSFUL;
    }

    return STATUS_SUCCESS;
}

static
VOID
NoumenHypervisorEndHypercall(
    _In_ KAFFINITY PreviousAffinity
    )
{
    KeRevertToUserAffinityThreadEx(PreviousAffinity);
    KeReleaseMutex(&g_HypervisorLock, FALSE);
}

//
// 驱动自用的内核 VA hook 安装/移除：不做 MDL 锁页，也不登记 r3 记录。
// origin 页和 NtCreateFile 本身都是内核地址，只需要处理器选择 + VMCALL。
//
static
NTSTATUS
NoumenEptKernelHook(
    _In_ UINT64 VirtualAddress,
    _In_ ULONG Kind,
    _In_ UINT64 Handler,
    _In_ UINT64 Trampoline,
    _In_ ULONG Processor,
    _Out_ PUINT64 Reply,
    _Out_opt_ PULONG ActualProcessor
    )
{
    KAFFINITY previousAffinity;
    ULONG cpu = 0;
    NTSTATUS status;

    status = NoumenHypervisorBeginHypercall(Processor, &previousAffinity, &cpu);

    if (!NT_SUCCESS(status)) {
        return status;
    }

    *Reply = NoumenHypercallHookInstall(
        g_NoumenHypervisor.HypercallKey,
        VirtualAddress,
        Kind,
        Handler,
        Trampoline);

    NoumenHypervisorEndHypercall(previousAffinity);

    if (ActualProcessor != NULL) {
        *ActualProcessor = cpu;
    }

    return STATUS_SUCCESS;
}

static
NTSTATUS
NoumenEptKernelUnhook(
    _In_ UINT64 VirtualAddress,
    _In_ ULONG Processor,
    _Out_ PUINT64 Reply,
    _Out_opt_ PULONG ActualProcessor
    )
{
    KAFFINITY previousAffinity;
    ULONG cpu = 0;
    NTSTATUS status;

    status = NoumenHypervisorBeginHypercall(Processor, &previousAffinity, &cpu);

    if (!NT_SUCCESS(status)) {
        return status;
    }

    *Reply = NoumenHypercallHookRemove(
        g_NoumenHypervisor.HypercallKey,
        VirtualAddress);

    NoumenHypervisorEndHypercall(previousAffinity);

    if (ActualProcessor != NULL) {
        *ActualProcessor = cpu;
    }

    return STATUS_SUCCESS;
}

//
// 让每个被虚拟化的核都应用最新一代 hook（与用户断点提交前的按核轮询相同）。
// 移除 proxy hook 后必须跑一遍，否则落后核的 EPT leaf 还指着 proxy 跳转。
//
static
VOID
NoumenEptApplyOnAllProcessors(
    VOID
    )
{
    ULONG active = KeQueryActiveProcessorCount(NULL);
    ULONG cpus = g_NoumenHypervisor.SingleCore
        ? 1
        : (g_NoumenHypervisor.CpuCount != 0 ? g_NoumenHypervisor.CpuCount : 1);
    ULONG index;

    if (InterlockedCompareExchange(&g_NoumenHypervisor.State, 0, 0) != NoumenStateRunning) {
        return;
    }

    if (cpus > active) {
        cpus = active;
    }

    for (index = 0; index < cpus; index++) {
        ULONG target = g_NoumenHypervisor.SingleCore
            ? g_NoumenHypervisor.StartProcessor
            : index;
        ULONG selected = 0;
        KAFFINITY affinity = 0;

        if (target >= active) {
            continue;
        }

        if (!NT_SUCCESS(NoumenHypervisorBeginHypercall(target, &affinity, &selected))) {
            continue;
        }

        (VOID)NoumenHypercallHookApply(g_NoumenHypervisor.HypercallKey);

        NoumenHypervisorEndHypercall(affinity);
    }
}

//
// 轮询 HOOK_RETIRED：所有 online vCPU 都应用/retire 了当前代才返回 TRUE。
// hypervisor 已经停止时没有 EPT 视图可恢复，直接返回 TRUE。
//
static
BOOLEAN
NoumenEptWaitRetired(
    _In_ ULONG TimeoutMs
    )
{
    ULONG elapsed;

    for (elapsed = 0; elapsed < TimeoutMs; elapsed++) {
        KAFFINITY previousAffinity;
        ULONG cpu = 0;
        UINT64 retired;

        if (InterlockedCompareExchange(&g_NoumenHypervisor.State, 0, 0) != NoumenStateRunning) {
            return TRUE;
        }

        if (!NT_SUCCESS(
                NoumenHypervisorBeginHypercall(
                    NOUMEN_HYPERCALL_PROCESSOR_ANY,
                    &previousAffinity,
                    &cpu))) {
            KeStallExecutionProcessor(1000);
            continue;
        }

        retired = NoumenHypercallHookRetired(g_NoumenHypervisor.HypercallKey);

        NoumenHypervisorEndHypercall(previousAffinity);

        if (retired != 0) {
            return TRUE;
        }

        KeStallExecutionProcessor(1000);
    }

    return FALSE;
}

//
// 等所有已经进入 origin 的调用退出。proxy 在入口 InterlockedIncrement、
// 原函数返回后 Decrement；remove 先摘掉目标页入口，不会再有新调用进来。
//
static
BOOLEAN
NoumenEptWaitQuiescent(
    _In_ ULONG TimeoutMs
    )
{
    LARGE_INTEGER interval;
    ULONG elapsed;

    interval.QuadPart = -10000; // 1 ms

    for (elapsed = 0; elapsed < TimeoutMs; elapsed++) {
        if (InterlockedCompareExchange(&g_Ept.InFlight, 0, 0) == 0) {
            return TRUE;
        }

        if (KeGetCurrentIrql() == PASSIVE_LEVEL) {
            KeDelayExecutionThread(KernelMode, FALSE, &interval);
        } else {
            KeStallExecutionProcessor(1000);
        }
    }

    return InterlockedCompareExchange(&g_Ept.InFlight, 0, 0) == 0;
}

//
// 这条 proxy 路径只实现 VMX（EPT DetourTrampoline）。SVM 的 NtCreateFile
// 仍然走命令 14 的 0x3A trampoline；在 AMD 上误点 EPT 面板时明确报错，
// 而不是装一个没有 origin 的跳转。
//
static
BOOLEAN
NoumenEptProxySupported(
    VOID
    )
{
    int regs[4];

    __cpuid(regs, 0);

    // "GenuineIntel": EBX/EDX/ECX = "Genu"/"ineI"/"ntel".
    return regs[1] == 0x756E6547 &&
        regs[3] == 0x49656E69 &&
        regs[2] == 0x6C65746E;
}

NTSTATUS
NoumenHypervisorHypercallPing(
    _In_ UINT64 Nonce,
    _In_ ULONG Processor,
    _Out_ PUINT64 Reply,
    _Out_opt_ PULONG ActualProcessor
    )
{
    KAFFINITY previousAffinity;
    NTSTATUS status;
    ULONG cpu;
    UINT64 pong;

    if (Reply == NULL) {
        return STATUS_INVALID_PARAMETER;
    }

    status = NoumenHypervisorBeginHypercall(Processor, &previousAffinity, &cpu);

    if (!NT_SUCCESS(status)) {
        return status;
    }

    pong = NoumenHypercallPing(g_NoumenHypervisor.HypercallKey, Nonce);

    NoumenHypervisorEndHypercall(previousAffinity);

    //
    // Return the raw reply: the caller compares it against PONG_MAGIC ^ Nonce.
    // A reply equal to the key means the VMCALL did not cause a VM exit, which
    // is diagnostic information, not a transport error.
    //
    *Reply = pong;

    if (ActualProcessor != NULL) {
        *ActualProcessor = cpu;
    }

    return STATUS_SUCCESS;
}

static
PNOUMEN_EPT_HOOK_RECORD
NoumenEptHookFindRecordLocked(
    _In_ ULONG ProcessId,
    _In_ UINT64 VirtualAddress
    )
{
    ULONG i;

    for (i = 0; i < NOUMEN_MAX_EPT_HOOKS; i++) {
        if (g_EptHookRecords[i].InUse &&
            g_EptHookRecords[i].ProcessId == ProcessId &&
            g_EptHookRecords[i].VirtualAddress == VirtualAddress) {
            return &g_EptHookRecords[i];
        }
    }

    return NULL;
}

static
UINT64
NoumenHypervisorVmcallRemovePage(
    _In_ UINT64 PagePhysical
    )
{
    KAFFINITY previousAffinity;
    NTSTATUS status;
    ULONG cpu;
    UINT64 reply;

    status = NoumenHypervisorBeginHypercall(
        NOUMEN_HYPERCALL_PROCESSOR_ANY,
        &previousAffinity,
        &cpu);

    if (!NT_SUCCESS(status)) {
        return 0;
    }

    reply = NoumenHypercallHookRemovePage(
        g_NoumenHypervisor.HypercallKey,
        PagePhysical);

    NoumenHypervisorEndHypercall(previousAffinity);

    return reply;
}

static
UINT64
NoumenHypervisorVmcallRemove(
    _In_ UINT64 VirtualAddress
    )
{
    KAFFINITY previousAffinity;
    NTSTATUS status;
    ULONG cpu;
    UINT64 reply;

    status = NoumenHypervisorBeginHypercall(
        NOUMEN_HYPERCALL_PROCESSOR_ANY,
        &previousAffinity,
        &cpu);

    if (!NT_SUCCESS(status)) {
        return 0;
    }

    reply = NoumenHypercallHookRemove(
        g_NoumenHypervisor.HypercallKey,
        VirtualAddress);

    NoumenHypervisorEndHypercall(previousAffinity);

    return reply;
}

static
NTSTATUS
NoumenEptHookInstall(
    _In_ UINT64 VirtualAddress,
    _In_ ULONG Kind,
    _In_ ULONG Processor,
    _In_ ULONG ProcessId,
    _In_ UINT64 Handler,
    _Out_ PUINT64 Reply,
    _Out_opt_ PULONG ActualProcessor
    )
{
    KAPC_STATE apcState;
    KAFFINITY previousAffinity;
    PEPROCESS process = NULL;
    PMDL mdl = NULL;
    BOOLEAN attached = FALSE;
    BOOLEAN locked = FALSE;
    UINT64 pagePhysical = 0;
    UINT64 reply = 0;
    ULONG cpu = 0;
    ULONG i;
    NTSTATUS status;

    if (ProcessId != 0) {
        status = PsLookupProcessByProcessId(ULongToHandle(ProcessId), &process);

        if (!NT_SUCCESS(status)) {
            return status;
        }

        if (PsGetProcessExitStatus(process) != STATUS_PENDING) {
            status = STATUS_PROCESS_IS_TERMINATING;
            goto Cleanup;
        }

        KeStackAttachProcess(process, &apcState);
        attached = TRUE;

        //
        // 按需换入并锁住目标页。MDL 一直持有到 hook 被移除，这正是页框号在
        // 换出/换入后保持稳定的原因。从页基址开始锁，即使地址未页对齐也只
        // 锁住一个页。
        //
        mdl = IoAllocateMdl(
            (PVOID)(VirtualAddress & ~0xFFFULL),
            PAGE_SIZE,
            FALSE,
            FALSE,
            NULL);

        if (mdl == NULL) {
            status = STATUS_INSUFFICIENT_RESOURCES;
            goto Cleanup;
        }

        __try {
            MmProbeAndLockPages(
                mdl,
                ((VirtualAddress >> 47) & 1) != 0 ? KernelMode : UserMode,
                IoReadAccess);
            locked = TRUE;
        } __except (EXCEPTION_EXECUTE_HANDLER) {
            locked = FALSE;
        }

        if (!locked) {
            status = STATUS_ACCESS_VIOLATION;
            goto Cleanup;
        }

        pagePhysical = ((UINT64)MmGetMdlPfnArray(mdl)[0]) << PAGE_SHIFT;
    }

    //
    // 仍处于 attach 状态（r3 地址）。用户断点另外把**锁页得到的物理页**传给
    // hypervisor（复用 Handler 槽位，DetourTrampoline 之外该参数没有别的用途）：
    // KVA shadow 下 attach 拿到的是内核 CR3，用户页未必在它里面映射，靠
    // VMCALL 时刻的 CR3 翻译可能落到别的页上——那会让断点装上了却永远不命中。
    //
    status = NoumenHypervisorBeginHypercall(Processor, &previousAffinity, &cpu);

    if (!NT_SUCCESS(status)) {
        goto Cleanup;
    }

    reply = NoumenHypercallHookInstall(
        g_NoumenHypervisor.HypercallKey,
        VirtualAddress,
        Kind,
        Kind == NOUMEN_HOOK_KIND_USER_BREAKPOINT && pagePhysical != 0
            ? pagePhysical
            : Handler,
        0);

    NoumenHypervisorEndHypercall(previousAffinity);

    //
    // 用户断点是“字节补丁 + clean 副本”两段式：上面的 VMCALL 只建好副本、
    // 把**本核** leaf 切到 clean 视图；这里按核轮询一次，让每个核都在自己的
    // VM exit 里应用这一代（切到 clean 视图），最后才写 0xCC。
    //
    // 顺序反了（先写 0xCC 再切页）就会出现“某核数据读已经能看到补丁字节”的
    // 窗口。轮询用亲和迁移逐核跑，和 STOP 用的是同一套做法。
    //
    if (Kind == NOUMEN_HOOK_KIND_USER_BREAKPOINT && reply == 0) {
        ULONG active = KeQueryActiveProcessorCount(NULL);
        ULONG cpus = g_NoumenHypervisor.SingleCore
            ? 1
            : (g_NoumenHypervisor.CpuCount != 0 ? g_NoumenHypervisor.CpuCount : 1);
        ULONG index;

        if (cpus > active) {
            cpus = active;
        }

        for (index = 0; index < cpus; index++) {
            ULONG target = g_NoumenHypervisor.SingleCore
                ? g_NoumenHypervisor.StartProcessor
                : index;
            ULONG selected = 0;
            KAFFINITY syncAffinity = 0;

            if (target >= active) {
                continue;
            }

            if (!NT_SUCCESS(NoumenHypervisorBeginHypercall(target, &syncAffinity, &selected))) {
                continue;
            }

            (VOID)NoumenHypercallHookApply(g_NoumenHypervisor.HypercallKey);
            NoumenHypervisorEndHypercall(syncAffinity);
        }

        reply = NoumenHypercallUserBreakpointCommit(g_NoumenHypervisor.HypercallKey, VirtualAddress);
    }

    if (attached) {
        KeUnstackDetachProcess(&apcState);
        attached = FALSE;
    }

    if (reply != 0 || pagePhysical == 0) {
        //
        // hypervisor 拒绝了 hook，或者这是内核地址、不需要跟踪页。两种
        // 情况都没有记录了。
        //
        status = STATUS_SUCCESS;
        goto Cleanup;
    }

    //
    // 进程可能在锁页和 VMCALL 之间退出。退出回调那时还看不到记录，因此
    // 这里主动撤销安装，而不是把 hook 留在一个没人映射的页上。
    //
    if (PsGetProcessExitStatus(process) != STATUS_PENDING) {
        (VOID)NoumenHypervisorVmcallRemove(VirtualAddress);
        status = STATUS_PROCESS_IS_TERMINATING;
        goto Cleanup;
    }

    status = KeWaitForMutexObject(&g_EptHookLock, Executive, KernelMode, FALSE, NULL);

    if (!NT_SUCCESS(status)) {
        (VOID)NoumenHypervisorVmcallRemove(VirtualAddress);
        goto Cleanup;
    }

    for (i = 0; i < NOUMEN_MAX_EPT_HOOKS; i++) {
        if (!g_EptHookRecords[i].InUse) {
            break;
        }
    }

    if (i == NOUMEN_MAX_EPT_HOOKS) {
        KeReleaseMutex(&g_EptHookLock, FALSE);
        (VOID)NoumenHypervisorVmcallRemove(VirtualAddress);
        status = STATUS_INSUFFICIENT_RESOURCES;
        goto Cleanup;
    }

    g_EptHookRecords[i].InUse = TRUE;
    g_EptHookRecords[i].ProcessId = ProcessId;
    g_EptHookRecords[i].VirtualAddress = VirtualAddress;
    g_EptHookRecords[i].PagePhysical = pagePhysical;
    g_EptHookRecords[i].Mdl = mdl;
    g_EptHookRecords[i].Process = process;

    KeReleaseMutex(&g_EptHookLock, FALSE);

    mdl = NULL;
    process = NULL;
    status = STATUS_SUCCESS;

Cleanup:
    if (attached) {
        KeUnstackDetachProcess(&apcState);
    }

    if (mdl != NULL) {
        if (locked) {
            MmUnlockPages(mdl);
        }

        IoFreeMdl(mdl);
    }

    if (process != NULL) {
        ObDereferenceObject(process);
    }

    *Reply = reply;

    if (NT_SUCCESS(status) && ActualProcessor != NULL) {
        *ActualProcessor = cpu;
    }

    return status;
}

static
NTSTATUS
NoumenEptHookRemove(
    _In_ UINT64 VirtualAddress,
    _In_ ULONG Processor,
    _In_ ULONG ProcessId,
    _Out_ PUINT64 Reply,
    _Out_opt_ PULONG ActualProcessor
    )
{
    KAPC_STATE apcState;
    KAFFINITY previousAffinity;
    PEPROCESS process = NULL;
    PMDL mdl = NULL;
    BOOLEAN attached = FALSE;
    UINT64 reply = 0;
    ULONG cpu = 0;
    NTSTATUS status;

    //
    // 先把已跟踪的记录从表里取出，然后在**不持有**注册锁的情况下发
    // hypercall。记录里的进程引用会被复用来 attach，这样两个进程共享同一
    // 虚拟地址时，hypervisor 能按安装时的 CR3 匹配。
    //
    if (NT_SUCCESS(KeWaitForMutexObject(&g_EptHookLock, Executive, KernelMode, FALSE, NULL))) {
        PNOUMEN_EPT_HOOK_RECORD record =
            NoumenEptHookFindRecordLocked(ProcessId, VirtualAddress);

        if (record != NULL) {
            mdl = record->Mdl;
            process = record->Process;
            record->Mdl = NULL;
            record->Process = NULL;
            record->InUse = FALSE;
        }

        KeReleaseMutex(&g_EptHookLock, FALSE);
    }

    if (process == NULL && ProcessId != 0) {
        //
        // 安装失败，或者 hook 不是通过受跟踪路径创建的，可能没有记录。
        //
        (VOID)PsLookupProcessByProcessId(ULongToHandle(ProcessId), &process);
    }

    if (process != NULL && PsGetProcessExitStatus(process) == STATUS_PENDING) {
        KeStackAttachProcess(process, &apcState);
        attached = TRUE;
    }

    status = NoumenHypervisorBeginHypercall(Processor, &previousAffinity, &cpu);

    if (!NT_SUCCESS(status)) {
        goto Cleanup;
    }

    reply = NoumenHypercallHookRemove(
        g_NoumenHypervisor.HypercallKey,
        VirtualAddress);

    NoumenHypervisorEndHypercall(previousAffinity);

Cleanup:
    if (attached) {
        KeUnstackDetachProcess(&apcState);
    }

    if (process != NULL) {
        ObDereferenceObject(process);
    }

    if (mdl != NULL) {
        MmUnlockPages(mdl);
        IoFreeMdl(mdl);
    }

    *Reply = reply;

    if (NT_SUCCESS(status) && ActualProcessor != NULL) {
        *ActualProcessor = cpu;
    }

    return status;
}

static
VOID
NoumenEptHooksReleaseAll(
    _In_ BOOLEAN RemoveFromHypervisor
    )
{
    NOUMEN_EPT_HOOK_RECORD records[NOUMEN_MAX_EPT_HOOKS];
    ULONG count = 0;
    ULONG i;

    if (!NT_SUCCESS(KeWaitForMutexObject(&g_EptHookLock, Executive, KernelMode, FALSE, NULL))) {
        return;
    }

    for (i = 0; i < NOUMEN_MAX_EPT_HOOKS; i++) {
        if (g_EptHookRecords[i].InUse) {
            records[count++] = g_EptHookRecords[i];
            RtlZeroMemory(&g_EptHookRecords[i], sizeof(g_EptHookRecords[i]));
        }
    }

    KeReleaseMutex(&g_EptHookLock, FALSE);

    for (i = 0; i < count; i++) {
        if (RemoveFromHypervisor &&
            InterlockedCompareExchange(&g_NoumenHypervisor.State, 0, 0) == NoumenStateRunning) {
            (VOID)NoumenHypervisorVmcallRemovePage(records[i].PagePhysical);
        }

        if (records[i].Mdl != NULL) {
            MmUnlockPages(records[i].Mdl);
            IoFreeMdl(records[i].Mdl);
        }

        if (records[i].Process != NULL) {
            ObDereferenceObject(records[i].Process);
        }
    }
}

static
VOID
NoumenEptHooksReleasePid(
    _In_ ULONG ProcessId
    )
{
    NOUMEN_EPT_HOOK_RECORD record;
    KAFFINITY previousAffinity;
    NTSTATUS status;
    BOOLEAN found;
    ULONG cpu;
    ULONG i;

    //
    // 一次只取一条记录：进程退出回调运行在内核栈上，剩余空间不值得赌，
    // 而记录数量是有上限的。
    //
    for (;;) {
        RtlZeroMemory(&record, sizeof(record));
        found = FALSE;

        if (!NT_SUCCESS(KeWaitForMutexObject(&g_EptHookLock, Executive, KernelMode, FALSE, NULL))) {
            return;
        }

        for (i = 0; i < NOUMEN_MAX_EPT_HOOKS; i++) {
            if (g_EptHookRecords[i].InUse &&
                g_EptHookRecords[i].ProcessId == ProcessId) {
                record = g_EptHookRecords[i];
                RtlZeroMemory(&g_EptHookRecords[i], sizeof(g_EptHookRecords[i]));
                found = TRUE;
                break;
            }
        }

        KeReleaseMutex(&g_EptHookLock, FALSE);

        if (!found) {
            return;
        }

        //
        // 进程正在退出，无法 attach；改为按记录的虚拟地址移除。即使没有
        // CR3 偏好，hypervisor 也能按安装时记录匹配。
        //
        if (InterlockedCompareExchange(&g_NoumenHypervisor.State, 0, 0) == NoumenStateRunning) {
            status = NoumenHypervisorBeginHypercall(
                NOUMEN_HYPERCALL_PROCESSOR_ANY,
                &previousAffinity,
                &cpu);

            if (NT_SUCCESS(status)) {
                (VOID)NoumenHypercallHookRemove(
                    g_NoumenHypervisor.HypercallKey,
                    record.VirtualAddress);

                NoumenHypervisorEndHypercall(previousAffinity);
            }
        }

        if (record.Mdl != NULL) {
            MmUnlockPages(record.Mdl);
            IoFreeMdl(record.Mdl);
        }

        if (record.Process != NULL) {
            ObDereferenceObject(record.Process);
        }
    }
}

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
    )
{
    UNREFERENCED_PARAMETER(Trampoline);

    if (Reply == NULL) {
        return STATUS_INVALID_PARAMETER;
    }

    if (Kind > NOUMEN_HOOK_KIND_USER_BREAKPOINT) {
        return STATUS_INVALID_PARAMETER;
    }

    // 硬件断点（kind 0）已不可用：明确返回“不支持”，不要安装。
    if (Kind == NOUMEN_HOOK_KIND_BREAKPOINT) {
        return STATUS_NOT_SUPPORTED;
    }

    //
    // DETOUR_TRAMPOLINE 是驱动自有的 NtCreateFile proxy：proxy 和 origin 页
    // 都由驱动提供，uapp 不再填 handler VA。安装/移除完全走这里。
    //
    if (Kind == NOUMEN_HOOK_KIND_DETOUR_TRAMPOLINE) {
        if (Remove) {
            return NoumenEptProxyHookRemove(
                VirtualAddress,
                Processor,
                Reply,
                ActualProcessor);
        }

        return NoumenEptProxyHookInstall(
            VirtualAddress,
            Processor,
            Reply,
            ActualProcessor);
    }

    *Reply = 0;

    //
    // detour 的 stub/trampoline/handler 位于内核地址空间；用户进程的 CR3
    // 不映射它。隐藏断点和 Hide 没有这个要求。
    //
    if (!Remove &&
        Kind == NOUMEN_HOOK_KIND_DETOUR_INTERNAL &&
        ((VirtualAddress >> 47) & 1) == 0) {
        return STATUS_NOT_SUPPORTED;
    }

    //
    // 用户断点命中后停在 INT3 等 uapp 应答，只有在 CPL3（用户态）执行时才能
    // 保证调试器有机会被调度，因此只接受低半区地址。
    //
    if (!Remove &&
        Kind == NOUMEN_HOOK_KIND_USER_BREAKPOINT &&
        ((VirtualAddress >> 47) & 1) != 0) {
        return STATUS_NOT_SUPPORTED;
    }

    if (Remove) {
        return NoumenEptHookRemove(VirtualAddress, Processor, ProcessId, Reply, ActualProcessor);
    }

    return NoumenEptHookInstall(
        VirtualAddress,
        Kind,
        Processor,
        ProcessId,
        Handler,
        Reply,
        ActualProcessor);
}

static BOOLEAN
NoumenSvmHookRecordAdd(
    _In_ ULONG ProcessId,
    _In_ UINT64 VirtualAddress,
    _In_ UINT64 PagePhysical,
    _In_ PMDL Mdl,
    _In_ PEPROCESS Process
    )
{
    ULONG i;

    if (!NT_SUCCESS(KeWaitForMutexObject(&g_SvmHookLock, Executive, KernelMode, FALSE, NULL))) {
        return FALSE;
    }

    for (i = 0; i < NOUMEN_MAX_EPT_HOOKS; i++) {
        if (!g_SvmHookRecords[i].InUse) {
            g_SvmHookRecords[i].InUse = TRUE;
            g_SvmHookRecords[i].ProcessId = ProcessId;
            g_SvmHookRecords[i].VirtualAddress = VirtualAddress;
            g_SvmHookRecords[i].PagePhysical = PagePhysical;
            g_SvmHookRecords[i].Mdl = Mdl;
            g_SvmHookRecords[i].Process = Process;
            KeReleaseMutex(&g_SvmHookLock, FALSE);
            return TRUE;
        }
    }

    KeReleaseMutex(&g_SvmHookLock, FALSE);
    return FALSE;
}

static VOID
NoumenSvmHookRecordRelease(
    _In_ ULONG ProcessId,
    _In_ UINT64 VirtualAddress
    )
{
    ULONG i;

    if (!NT_SUCCESS(KeWaitForMutexObject(&g_SvmHookLock, Executive, KernelMode, FALSE, NULL))) {
        return;
    }

    for (i = 0; i < NOUMEN_MAX_EPT_HOOKS; i++) {
        if (g_SvmHookRecords[i].InUse &&
            g_SvmHookRecords[i].ProcessId == ProcessId &&
            g_SvmHookRecords[i].VirtualAddress == VirtualAddress) {
            PMDL mdl = g_SvmHookRecords[i].Mdl;
            PEPROCESS process = g_SvmHookRecords[i].Process;

            g_SvmHookRecords[i].InUse = FALSE;
            g_SvmHookRecords[i].Mdl = NULL;
            g_SvmHookRecords[i].Process = NULL;
            KeReleaseMutex(&g_SvmHookLock, FALSE);

            if (mdl != NULL) {
                MmUnlockPages(mdl);
                IoFreeMdl(mdl);
            }

            if (process != NULL) {
                ObDereferenceObject(process);
            }

            return;
        }
    }

    KeReleaseMutex(&g_SvmHookLock, FALSE);
}

//
// Remove every SVM byte-patch hook and release its locked page. Called from
// Stop before the CPUs leave virtualization, so the remove hypercall still has
// a hypervisor to talk to.
//
static VOID
NoumenSvmHooksReleaseAll(
    _In_ BOOLEAN RemoveFromHypervisor
    )
{
    ULONG i;

    for (i = 0; i < NOUMEN_MAX_EPT_HOOKS; i++) {
        ULONG pid;
        UINT64 va;

        if (!NT_SUCCESS(KeWaitForMutexObject(&g_SvmHookLock, Executive, KernelMode, FALSE, NULL))) {
            return;
        }

        if (!g_SvmHookRecords[i].InUse) {
            KeReleaseMutex(&g_SvmHookLock, FALSE);
            continue;
        }

        pid = g_SvmHookRecords[i].ProcessId;
        va = g_SvmHookRecords[i].VirtualAddress;
        KeReleaseMutex(&g_SvmHookLock, FALSE);

        if (RemoveFromHypervisor) {
            (VOID)NoumenHypercallSvmHookRemove(
                g_NoumenHypervisor.HypercallKey,
                va);
        }

        NoumenSvmHookRecordRelease(pid, va);
    }
}

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
    )
{
    KAFFINITY previousAffinity;
    ULONG cpu = 0;
    UINT64 reply = 0;
    NTSTATUS status;

    if (Reply == NULL) {
        return STATUS_INVALID_PARAMETER;
    }

    if (Method > NOUMEN_SVM_METHOD_NPT_HOOK ||
        Kind > NOUMEN_HOOK_KIND_USER_BREAKPOINT) {
        return STATUS_INVALID_PARAMETER;
    }

    *Reply = 0;

    //
    // A user breakpoint installed as a real-page byte patch must use the
    // driver-locked physical page: under KVA shadow the CR3 at VMCALL is the
    // kernel CR3 and the user page may not be mapped there. Encode the PFN in
    // the high bits of Kind (Kind itself stays in the low byte).
    //
    if (!Remove &&
        (Method == NOUMEN_SVM_METHOD_BYTE_PATCH ||
         Method == NOUMEN_SVM_METHOD_NPT_HOOK) &&
        ProcessId != 0) {
        KAPC_STATE apcState;
        PEPROCESS process = NULL;
        PMDL mdl = NULL;
        UINT64 pagePhysical = 0;

        status = PsLookupProcessByProcessId(ULongToHandle(ProcessId), &process);

        if (!NT_SUCCESS(status)) {
            return status;
        }

        KeStackAttachProcess(process, &apcState);

        mdl = IoAllocateMdl(
            (PVOID)(VirtualAddress & ~0xFFFULL),
            PAGE_SIZE,
            FALSE,
            FALSE,
            NULL);

        if (mdl != NULL) {
            __try {
                MmProbeAndLockPages(mdl, UserMode, IoReadAccess);
                pagePhysical = ((UINT64)MmGetMdlPfnArray(mdl)[0]) << PAGE_SHIFT;
            }
            __except (EXCEPTION_EXECUTE_HANDLER) {
                pagePhysical = 0;
            }
        }

        if (pagePhysical != 0) {
            Kind |= (ULONG)((pagePhysical >> PAGE_SHIFT) << 8);

            if (!NoumenSvmHookRecordAdd(
                    ProcessId,
                    VirtualAddress,
                    pagePhysical,
                    mdl,
                    process)) {
                MmUnlockPages(mdl);
                IoFreeMdl(mdl);
                KeUnstackDetachProcess(&apcState);
                ObDereferenceObject(process);
                return STATUS_INSUFFICIENT_RESOURCES;
            }

            // Ownership of the MDL and EPROCESS reference moved to the record.
            mdl = NULL;
            process = NULL;
        }

        if (mdl != NULL) {
            if (pagePhysical != 0) {
                MmUnlockPages(mdl);
            }

            IoFreeMdl(mdl);
        }

        KeUnstackDetachProcess(&apcState);

        if (process != NULL) {
            ObDereferenceObject(process);
        }
    }

    status = NoumenHypervisorBeginHypercall(Processor, &previousAffinity, &cpu);

    if (!NT_SUCCESS(status)) {
        return status;
    }

    if (Remove) {
        reply = NoumenHypercallSvmHookRemove(
            g_NoumenHypervisor.HypercallKey,
            VirtualAddress);

        if (reply == 0) {
            NoumenSvmHookRecordRelease(ProcessId, VirtualAddress);
        }
    } else if (Method == NOUMEN_SVM_METHOD_NPT_HOOK) {
        if (Handler == 0 || Trampoline == 0) {
            NoumenHypervisorEndHypercall(previousAffinity);
            return STATUS_INVALID_PARAMETER;
        }

        reply = NoumenHypercallSvmHookInstallNpt(
            g_NoumenHypervisor.HypercallKey,
            VirtualAddress,
            Handler,
            Trampoline,
            Kind);
    } else {
        reply = NoumenHypercallSvmHookInstall(
            g_NoumenHypervisor.HypercallKey,
            VirtualAddress,
            Method,
            Kind);
    }

    NoumenHypervisorEndHypercall(previousAffinity);

    *Reply = reply;

    if (ActualProcessor != NULL) {
        *ActualProcessor = cpu;
    }

    return STATUS_SUCCESS;
}

//
// ---- NtCreateFile NPT hook（实现） ---------------------------------------
//
// 顺序：日志/状态 → handler（guest 内核代码）→ trampoline 分配 →
// 安装/移除 → 读日志。
//

static
VOID
NoumenNtCreateFileResetLog(
    VOID
    )
{
    KIRQL oldIrql;

    if (g_NtcfLog == NULL) {
        return;
    }

    KeAcquireSpinLock(&g_NtcfLogLock, &oldIrql);

    RtlZeroMemory(
        g_NtcfLog,
        sizeof(NOUMEN_NT_CREATE_FILE_LOG) * NOUMEN_NTCF_LOG_CAPACITY);
    g_NtcfSequence = 0;

    KeReleaseSpinLock(&g_NtcfLogLock, oldIrql);
}

//
// handler 的栈参数偏移。trampoline（hypervisor 侧
// `src/arch/x86_64/svm/hook.rs::install_npt`）在 `call handler` 之前压入 15 个
// 通用寄存器：
//
//     push rax rcx rdx rbx rbp rsi rdi   ; 7 个
//     push r8 .. r15                     ; 8 个
//     mov  rax, handler
//     call rax
//
// 所以 handler 入口的 rsp = NtCreateFile 入口的 rsp - 0x78 - 8，RCX/RDX/R8/R9
// 仍然是原来的 4 个参数，而**栈上参数整体比标准位置高 0x80**：
//
//     handler rsp + 0xA8 -> AllocationSize      （第 5 个）
//     handler rsp + 0xB0 -> FileAttributes      （第 6 个）
//     handler rsp + 0xB8 -> ShareAccess         （第 7 个）
//     handler rsp + 0xC0 -> CreateDisposition   （第 8 个）
//     handler rsp + 0xC8 -> CreateOptions       （第 9 个）
//
// `_AddressOfReturnAddress()` 就是 handler 入口的 rsp，所以下面的宏读到的
// 正是原来的栈参数。trampoline 随后恢复全部寄存器并执行被覆盖的原指令，
// 因此 handler 可以写成普通 x64 函数（volatile XMM 允许被破坏——任何函数
// 调用都会这样）。
//
// 改 trampoline 的寄存器保存序列时，必须同步修改这里。
//
#define NOUMEN_NTCF_STACK_ARG(Offset) \
    (*(volatile ULONG64 *)((PUCHAR)_AddressOfReturnAddress() + (Offset)))

#define NOUMEN_NTCF_ARG_FILE_ATTRIBUTES    0xB0
#define NOUMEN_NTCF_ARG_SHARE_ACCESS       0xB8
#define NOUMEN_NTCF_ARG_CREATE_DISPOSITION 0xC0
#define NOUMEN_NTCF_ARG_CREATE_OPTIONS     0xC8

static
VOID
NoumenNtCreateFileRecord(
    _In_ ULONG DesiredAccess,
    _In_opt_ POBJECT_ATTRIBUTES ObjectAttributes,
    _In_ ULONG FileAttributes,
    _In_ ULONG ShareAccess,
    _In_ ULONG CreateDisposition,
    _In_ ULONG CreateOptions
    )
{
    NOUMEN_NT_CREATE_FILE_LOG entry;
    KIRQL oldIrql;
    ULONG flags = 0;
    ULONG length = 0;

    if (g_NtcfLog == NULL) {
        return;
    }

    RtlZeroMemory(&entry, sizeof(entry));

    entry.DesiredAccess = DesiredAccess;
    entry.FileAttributes = FileAttributes;
    entry.ShareAccess = ShareAccess;
    entry.CreateDisposition = CreateDisposition;
    entry.CreateOptions = CreateOptions;
    entry.ProcessId = HandleToUlong(PsGetCurrentProcessId());
    entry.ThreadId = HandleToUlong(PsGetCurrentThreadId());
    entry.Tsc = (UINT64)__rdtsc();

    //
    // 名字来自调用者：用户态调用时 ObjectName->Buffer 是用户地址。**必须在
    // 拿自旋锁之前读**——KeAcquireSpinLock 把 IRQL 提到 DISPATCH_LEVEL，那里
    // 发生缺页会 bugcheck。SEH 兜住缺页、非规范地址和跨进程的坏指针。
    //
    if (ObjectAttributes != NULL) {
        __try {
            PUNICODE_STRING name = ObjectAttributes->ObjectName;

            if (name != NULL && name->Buffer != NULL && name->Length != 0) {
                length = name->Length;

                if (length > sizeof(entry.ObjectName)) {
                    length = sizeof(entry.ObjectName);
                    flags |= NOUMEN_NTCF_FLAG_TRUNCATED;
                }

                RtlCopyMemory(entry.ObjectName, name->Buffer, length);
            }
        } __except (EXCEPTION_EXECUTE_HANDLER) {
            length = 0;
            flags |= NOUMEN_NTCF_FLAG_FAULTED;
        }
    }

    entry.NameLength = length;
    entry.Flags = flags;

    KeAcquireSpinLock(&g_NtcfLogLock, &oldIrql);

    entry.Sequence = (UINT64)(g_NtcfSequence + 1);

    RtlCopyMemory(
        &g_NtcfLog[(ULONG)(g_NtcfSequence % NOUMEN_NTCF_LOG_CAPACITY)],
        &entry,
        sizeof(entry));

    g_NtcfSequence = (LONG64)entry.Sequence;

    KeReleaseSpinLock(&g_NtcfLogLock, oldIrql);
}

//
// SVM/NPT 命令 14 的 trampoline 里 `call rax` 的目标。参数是 NtCreateFile 的
// 前 4 个；后 5 个用固定偏移从栈上读（trampoline 压了 15 个 GPR）。只记录，
// 不修改任何参数、不改返回值。VMX 的 proxy 用下面的
// NoumenNtCreateFileHandler，两者不要混用。
//
static
VOID
NoumenNtCreateFileLegacyHandler(
    _In_ PVOID FileHandle,
    _In_ ULONG DesiredAccess,
    _In_opt_ POBJECT_ATTRIBUTES ObjectAttributes,
    _In_ PVOID IoStatusBlock
    )
{
    ULONG fileAttributes;
    ULONG shareAccess;
    ULONG createDisposition;
    ULONG createOptions;

    UNREFERENCED_PARAMETER(FileHandle);
    UNREFERENCED_PARAMETER(IoStatusBlock);

    fileAttributes = (ULONG)NOUMEN_NTCF_STACK_ARG(NOUMEN_NTCF_ARG_FILE_ATTRIBUTES);
    shareAccess = (ULONG)NOUMEN_NTCF_STACK_ARG(NOUMEN_NTCF_ARG_SHARE_ACCESS);
    createDisposition =
        (ULONG)NOUMEN_NTCF_STACK_ARG(NOUMEN_NTCF_ARG_CREATE_DISPOSITION);
    createOptions = (ULONG)NOUMEN_NTCF_STACK_ARG(NOUMEN_NTCF_ARG_CREATE_OPTIONS);

    NoumenNtCreateFileRecord(
        DesiredAccess,
        ObjectAttributes,
        fileAttributes,
        shareAccess,
        createDisposition,
        createOptions);
}

//
// VMX DetourTrampoline 的 proxy。fake 副本入口用 14 字节绝对跳转进入这里，
// 不是 `call`：栈上是 NtCreateFile 调用者原来的返回地址和全部参数，所以按
// 标准 Windows x64 签名读参数即可。
//
// 调用顺序：
//   InFlight++ → 记录参数 → call g_Ept.Origin → InFlight-- → 返回 NTSTATUS
//
// origin 页在 hook 存活期间不会被释放；unhook 先置 Closing、恢复目标页入口，
// 再等 InFlight 归零，所以这里看到 Closing 时只需立刻返回失败（极窄窗口，
// 不会碰 origin）。
//
__declspec(guard(nocf))
static
NTSTATUS
NTAPI
NoumenNtCreateFileHandler(
    _Out_ PHANDLE FileHandle,
    _In_ ULONG DesiredAccess,
    _In_opt_ POBJECT_ATTRIBUTES ObjectAttributes,
    _Out_ PIO_STATUS_BLOCK IoStatusBlock,
    _In_opt_ PLARGE_INTEGER AllocationSize,
    _In_ ULONG FileAttributes,
    _In_ ULONG ShareAccess,
    _In_ ULONG CreateDisposition,
    _In_ ULONG CreateOptions,
    _In_opt_ PVOID EaBuffer,
    _In_ ULONG EaLength
    )
{
    NOUMEN_NTCF_ORIGIN origin;
    NTSTATUS status;

    InterlockedIncrement(&g_Ept.InFlight);

    if (InterlockedCompareExchange(&g_Ept.Closing, 0, 0) != 0) {
        InterlockedDecrement(&g_Ept.InFlight);
        return STATUS_UNSUCCESSFUL;
    }

    origin = (NOUMEN_NTCF_ORIGIN)(ULONG_PTR)g_Ept.Origin;

    if (origin == NULL) {
        InterlockedDecrement(&g_Ept.InFlight);
        return STATUS_UNSUCCESSFUL;
    }

    NoumenNtCreateFileRecord(
        DesiredAccess,
        ObjectAttributes,
        FileAttributes,
        ShareAccess,
        CreateDisposition,
        CreateOptions);

    status = origin(
        FileHandle,
        DesiredAccess,
        ObjectAttributes,
        IoStatusBlock,
        AllocationSize,
        FileAttributes,
        ShareAccess,
        CreateDisposition,
        CreateOptions,
        EaBuffer,
        EaLength);

    InterlockedDecrement(&g_Ept.InFlight);

    return status;
}

static
NTSTATUS
NoumenNtCreateFileAllocateTrampoline(
    VOID
    )
{
    if (g_Ntcf.Trampoline != 0) {
        return STATUS_SUCCESS;
    }

    //
    // 一整页 NonPagedPool：hypervisor 会往里写“保存寄存器 + call handler
    // + 被覆盖原指令 + 跳回”的代码（不到 512 字节）。页对齐的整页分配保证
    // 写入不会越出本次分配；物理地址也必须在 64 GiB 的 direct map 内，否则
    // hypervisor 会返回 NotMapped。用普通 NonPagedPool（不要 NonPagedPoolExecute
    // 这个别名），页本身仍然可执行。
    //
    g_Ntcf.Trampoline = (UINT64)(ULONG_PTR)ExAllocatePoolWithTag(
        NonPagedPool,
        PAGE_SIZE,
        NOUMEN_NTCF_POOL_TAG);

    if (g_Ntcf.Trampoline == 0) {
        return STATUS_INSUFFICIENT_RESOURCES;
    }

    RtlZeroMemory((PVOID)(ULONG_PTR)g_Ntcf.Trampoline, PAGE_SIZE);

    return STATUS_SUCCESS;
}

//
// 摘掉 hook（可选地通知 hypervisor），并清空安装状态。
//
// trampoline **不在这里释放**：移除 hypercall 只保证之后不会再新建入口，另一个
// vCPU 可能仍在 trampoline 里执行 handler / 跳回指令。页在驱动卸载时释放
// （见 NoumenHypervisorCleanup）。
//
static
VOID
NoumenNtCreateFileRelease(
    _In_ BOOLEAN RemoveFromHypervisor
    )
{
    KAFFINITY previousAffinity;
    ULONG cpu;
    UINT64 address;
    BOOLEAN installed;
    NTSTATUS status;

    if (!NT_SUCCESS(
            KeWaitForMutexObject(&g_NtcfStateLock, Executive, KernelMode, FALSE, NULL))) {
        return;
    }

    installed = g_Ntcf.Installed;
    address = g_Ntcf.Address;
    g_Ntcf.Installed = FALSE;
    g_Ntcf.Address = 0;

    KeReleaseMutex(&g_NtcfStateLock, FALSE);

    if (!installed || !RemoveFromHypervisor) {
        return;
    }

    //
    // 复用 Begin/End 的处理器选择与亲和性迁移（VMCALL 必须在虚拟化过的 CPU
    // 上执行）。hypervisor 已经不在运行时这里会失败，等价于 STOP 已经把视图
    // 恢复，不需要额外处理。
    //
    status = NoumenHypervisorBeginHypercall(
        NOUMEN_HYPERCALL_PROCESSOR_ANY,
        &previousAffinity,
        &cpu);

    if (!NT_SUCCESS(status)) {
        return;
    }

    (VOID)NoumenHypercallSvmHookRemove(g_NoumenHypervisor.HypercallKey, address);

    NoumenHypervisorEndHypercall(previousAffinity);
}

//
// 释放 trampoline 和日志缓冲。只能在“hook 不会再被执行”时调用：驱动卸载，
// 且 hypervisor 已经停止（STOP 已经把所有 NPT 视图恢复成真实页）。
//
static
VOID
NoumenNtCreateFileFreeResources(
    VOID
    )
{
    PVOID trampoline;
    PVOID log;
    PVOID origin = NULL;
    PVOID originRaw = NULL;

    if (!NT_SUCCESS(
            KeWaitForMutexObject(&g_NtcfStateLock, Executive, KernelMode, FALSE, NULL))) {
        return;
    }

    trampoline = (PVOID)(ULONG_PTR)g_Ntcf.Trampoline;
    g_Ntcf.Trampoline = 0;

    KeReleaseMutex(&g_NtcfStateLock, FALSE);

    //
    // EPT proxy 的 origin 页。Normal Stop/卸载已经由 NoumenEptProxyRelease
    // 释放；这里处理“停掉 hypervisor 后才卸载驱动”时还留着的那一页。
    // 还有调用在 origin 里时不释放（泄漏一页比 use-after-free 安全）。
    //
    if (InterlockedCompareExchange(&g_Ept.InFlight, 0, 0) == 0 &&
        NT_SUCCESS(
            KeWaitForMutexObject(&g_EptLock, Executive, KernelMode, FALSE, NULL))) {
        if (g_Ept.Origin != 0) {
            originRaw = (PVOID)(ULONG_PTR)g_Ept.OriginRaw;
            origin = (PVOID)(ULONG_PTR)g_Ept.Origin;
            g_Ept.Origin = 0;
            g_Ept.OriginRaw = 0;
            g_Ept.Target = 0;
            g_Ept.Proxy = 0;
            g_Ept.Installed = FALSE;
            g_Ept.OriginHidden = FALSE;
        }

        KeReleaseMutex(&g_EptLock, FALSE);
    }

    //
    // 先清空指针再释放：即使还有 handler 意外进来，也只会提前返回。
    //
    log = (PVOID)g_NtcfLog;
    g_NtcfLog = NULL;

    if (trampoline != NULL) {
        ExFreePoolWithTag(trampoline, NOUMEN_NTCF_POOL_TAG);
    }

    if (originRaw != NULL) {
        ExFreePoolWithTag(originRaw, NOUMEN_EPT_PROXY_POOL_TAG);
    } else if (origin != NULL) {
        ExFreePoolWithTag(origin, NOUMEN_EPT_PROXY_POOL_TAG);
    }

    if (log != NULL) {
        ExFreePoolWithTag(log, NOUMEN_NTCF_POOL_TAG);
    }
}

static
NTSTATUS
NoumenNtCreateFileReadLog(
    _In_ ULONG MaxRecords,
    _Out_writes_bytes_opt_(MaxRecords * sizeof(NOUMEN_NT_CREATE_FILE_LOG)) PNOUMEN_NT_CREATE_FILE_LOG Records,
    _Out_ PULONG RecordCount,
    _Out_ PNOUMEN_NT_CREATE_FILE_RESPONSE Response
    )
{
    KIRQL oldIrql;
    LONG64 total;
    ULONG count;
    ULONG start;
    ULONG i;

    *RecordCount = 0;

    if (g_NtcfLog == NULL) {
        return STATUS_DEVICE_NOT_READY;
    }

    if (MaxRecords != 0 && Records == NULL) {
        return STATUS_INVALID_PARAMETER;
    }

    KeAcquireSpinLock(&g_NtcfLogLock, &oldIrql);

    total = g_NtcfSequence;
    count = total > NOUMEN_NTCF_LOG_CAPACITY
        ? NOUMEN_NTCF_LOG_CAPACITY
        : (ULONG)total;

    if (count > MaxRecords) {
        count = MaxRecords;
    }

    // 最新 count 条；最旧的一条在 (total - count) % CAPACITY。
    start = (ULONG)((total - (LONG64)count) % NOUMEN_NTCF_LOG_CAPACITY);

    for (i = 0; i < count; i++) {
        RtlCopyMemory(
            &Records[i],
            &g_NtcfLog[(start + i) % NOUMEN_NTCF_LOG_CAPACITY],
            sizeof(NOUMEN_NT_CREATE_FILE_LOG));
    }

    KeReleaseSpinLock(&g_NtcfLogLock, oldIrql);

    Response->TotalRecords = (UINT64)total;
    Response->Records = count;

    *RecordCount = count;

    return STATUS_SUCCESS;
}

NTSTATUS
NoumenHypervisorHookNtCreateFile(
    _In_ ULONG Action,
    _In_ ULONG Processor,
    _In_ ULONG MaxRecords,
    _Out_ PNOUMEN_NT_CREATE_FILE_RESPONSE Response,
    _Out_writes_bytes_opt_(MaxRecords * sizeof(NOUMEN_NT_CREATE_FILE_LOG)) PNOUMEN_NT_CREATE_FILE_LOG Records,
    _Out_ PULONG RecordCount
    )
{
    KAFFINITY previousAffinity;
    UINT64 address = 0;
    UINT64 reply = 0;
    ULONG cpu = 0;
    NTSTATUS status;

    if (Response == NULL || RecordCount == NULL) {
        return STATUS_INVALID_PARAMETER;
    }

    RtlZeroMemory(Response, sizeof(*Response));
    *RecordCount = 0;

    //
    // 读日志不碰 hypervisor，也不要求 hook 正在运行：移除之后仍然可以看最后
    // 一批命中。
    //
    if (Action == NOUMEN_NTCF_ACTION_READ_LOG) {
        Response->Address = g_Ntcf.Address;
        Response->Handler = (UINT64)(ULONG_PTR)NoumenNtCreateFileLegacyHandler;
        Response->Trampoline = g_Ntcf.Trampoline;
        Response->Processor = KeGetCurrentProcessorNumber();
        Response->Installed = g_Ntcf.Installed ? 1 : 0;

        return NoumenNtCreateFileReadLog(
            MaxRecords,
            Records,
            RecordCount,
            Response);
    }

    if (Action != NOUMEN_NTCF_ACTION_INSTALL && Action != NOUMEN_NTCF_ACTION_REMOVE) {
        return STATUS_INVALID_PARAMETER;
    }

    status = KeWaitForMutexObject(&g_NtcfStateLock, Executive, KernelMode, FALSE, NULL);

    if (!NT_SUCCESS(status)) {
        return status;
    }

    if (Action == NOUMEN_NTCF_ACTION_INSTALL) {
        if (g_Ntcf.Installed) {
            KeReleaseMutex(&g_NtcfStateLock, FALSE);
            return STATUS_INVALID_DEVICE_STATE;
        }

        //
        // 没有日志缓冲就没有安装的意义：handler 会直接返回，界面永远读到 0
        // 条记录，比明确失败更难排查。
        //
        if (g_NtcfLog == NULL) {
            KeReleaseMutex(&g_NtcfStateLock, FALSE);
            return STATUS_INSUFFICIENT_RESOURCES;
        }

        status = NoumenHypervisorResolveNtCreateFile(&address);

        if (!NT_SUCCESS(status)) {
            KeReleaseMutex(&g_NtcfStateLock, FALSE);
            return status;
        }

        status = NoumenNtCreateFileAllocateTrampoline();

        if (!NT_SUCCESS(status)) {
            KeReleaseMutex(&g_NtcfStateLock, FALSE);
            return status;
        }

        // 每次安装都从空日志、Sequence 1 开始。
        NoumenNtCreateFileResetLog();
    } else {
        if (!g_Ntcf.Installed) {
            KeReleaseMutex(&g_NtcfStateLock, FALSE);
            return STATUS_NOT_FOUND;
        }

        address = g_Ntcf.Address;
    }

    KeReleaseMutex(&g_NtcfStateLock, FALSE);

    status = NoumenHypervisorBeginHypercall(Processor, &previousAffinity, &cpu);

    if (!NT_SUCCESS(status)) {
        return status;
    }

    if (Action == NOUMEN_NTCF_ACTION_INSTALL) {
        //
        // 0x3A：va = NtCreateFile，handler = 驱动里的记录函数，trampoline =
        // 刚分配的可执行页，kind = 普通断点（NPT 函数 hook 只用低字节）。
        //
        reply = NoumenHypercallSvmHookInstallNpt(
            g_NoumenHypervisor.HypercallKey,
            address,
            (UINT64)(ULONG_PTR)NoumenNtCreateFileLegacyHandler,
            g_Ntcf.Trampoline,
            NOUMEN_HOOK_KIND_BREAKPOINT);
    } else {
        reply = NoumenHypercallSvmHookRemove(
            g_NoumenHypervisor.HypercallKey,
            address);
    }

    NoumenHypervisorEndHypercall(previousAffinity);

    if (NT_SUCCESS(
            KeWaitForMutexObject(&g_NtcfStateLock, Executive, KernelMode, FALSE, NULL))) {
        if (Action == NOUMEN_NTCF_ACTION_INSTALL) {
            if (reply == 0) {
                if (InterlockedCompareExchange(
                        &g_NoumenHypervisor.State, 0, 0) == NoumenStateRunning) {
                    g_Ntcf.Installed = TRUE;
                    g_Ntcf.Address = address;
                } else {
                    //
                    // Stop 抢在 hypercall 和提交之间：hook 已经随 STOP 一起被
                    // 清掉（hook::shutdown），不要记成“已安装”。
                    //
                    status = STATUS_UNSUCCESSFUL;
                }
            }
        } else if (reply == 0) {
            g_Ntcf.Installed = FALSE;
            g_Ntcf.Address = 0;
        }

        Response->Trampoline = g_Ntcf.Trampoline;

        KeReleaseMutex(&g_NtcfStateLock, FALSE);
    }

    Response->Address = address;
    Response->Handler = (UINT64)(ULONG_PTR)NoumenNtCreateFileLegacyHandler;
    Response->Reply = reply;
    Response->Processor = cpu;
    Response->TotalRecords = (UINT64)g_NtcfSequence;
    Response->Records = 0;
    Response->Installed = g_Ntcf.Installed ? 1 : 0;

    *RecordCount = 0;

    return status;
}

//
// ---- NtCreateFile EPT proxy（实现）---------------------------------------
//
// 安装顺序：
//   1. 分配/复用 origin 页（NonPagedPool，一整页）；
//   2. 先用 Hide hook 把该页的数据视图映射到全 0 页（执行仍走真实页）；
//   3. 再装 DETOUR_TRAMPOLINE：hypervisor 把被覆盖的指令 + 跳回
//      NtCreateFile+stolen 写进 origin，把目标页 fake 副本入口写成跳 proxy；
//   4. 清空命中日志，标记 Installed。
//
// 移除顺序反过来，并且要等所有核 retire、origin 里的调用全部退出，才释放页。
//
static
NTSTATUS
NoumenEptProxyHookInstall(
    _In_ UINT64 VirtualAddress,
    _In_ ULONG Processor,
    _Out_ PUINT64 Reply,
    _Out_opt_ PULONG ActualProcessor
    )
{
    NTSTATUS status;
    UINT64 ntcf = 0;
    UINT64 origin = 0;
    UINT64 originRaw = 0;
    UINT64 hideReply = 0;
    UINT64 hookReply = 0;
    UINT64 ignoredReply = 0;
    ULONG cpu = 0;
    ULONG ignoredCpu = 0;
    BOOLEAN hideInstalled = FALSE;
    BOOLEAN hideRemoved = FALSE;

    if (Reply == NULL) {
        return STATUS_INVALID_PARAMETER;
    }

    *Reply = 0;

    if (!NoumenEptProxySupported()) {
        return STATUS_NOT_SUPPORTED;
    }

    if (((VirtualAddress >> 47) & 1) == 0) {
        return STATUS_NOT_SUPPORTED;
    }

    //
    // proxy 固定是 NoumenNtCreateFileHandler，只对 NtCreateFile 有意义。
    // 地址由 uapp 解析后传入；这里再解析一次做一致性校验，防止把 proxy
    // 装到别的函数上。
    //
    status = NoumenHypervisorResolveNtCreateFile(&ntcf);

    if (!NT_SUCCESS(status)) {
        return status;
    }

    if (ntcf != VirtualAddress) {
        return STATUS_INVALID_PARAMETER;
    }

    if (g_NtcfLog == NULL) {
        return STATUS_INSUFFICIENT_RESOURCES;
    }

    status = KeWaitForMutexObject(&g_EptLock, Executive, KernelMode, FALSE, NULL);

    if (!NT_SUCCESS(status)) {
        return status;
    }

    if (g_Ept.Installed || g_Ept.Busy) {
        KeReleaseMutex(&g_EptLock, FALSE);
        return STATUS_INVALID_DEVICE_STATE;
    }

    //
    // 上一次 remove 没有等到 quiescent/retire 时 Closing 保持 1，origin 页
    // 仍可能被调用；这时重装会清零/复用那页，必须拒绝。
    //
    if (InterlockedCompareExchange(&g_Ept.Closing, 0, 0) != 0) {
        KeReleaseMutex(&g_EptLock, FALSE);
        return STATUS_INVALID_DEVICE_STATE;
    }

    if (g_Ept.Stale) {
        //
        // 上次 remove 没等到所有核 retire / origin 调用退出。stale fake 页
        // 仍可能跳到 proxy，复用/清零 origin 会破坏正在执行的调用。
        //
        KeReleaseMutex(&g_EptLock, FALSE);
        return STATUS_INVALID_DEVICE_STATE;
    }

    g_Ept.Busy = 1;

    if (g_Ept.Origin == 0) {
        PVOID raw = ExAllocatePoolWithTag(
            NonPagedPool,
            PAGE_SIZE * 2,
            NOUMEN_EPT_PROXY_POOL_TAG);

        if (raw == NULL) {
            g_Ept.Busy = 0;
            KeReleaseMutex(&g_EptLock, FALSE);
            return STATUS_INSUFFICIENT_RESOURCES;
        }

        //
        // 整页对齐，保证 origin 代码（stolen + 14 字节跳回）不会跨页。
        // 多分配一页只用于对齐；保存原始指针供释放。
        //
        g_Ept.OriginRaw = (UINT64)(ULONG_PTR)raw;
        g_Ept.Origin = (g_Ept.OriginRaw + PAGE_SIZE - 1) & ~(UINT64)(PAGE_SIZE - 1);
    }

    origin = g_Ept.Origin;
    originRaw = g_Ept.OriginRaw;

    RtlZeroMemory((PVOID)(ULONG_PTR)origin, PAGE_SIZE);

    g_Ept.Target = VirtualAddress;
    g_Ept.Proxy = (UINT64)(ULONG_PTR)NoumenNtCreateFileHandler;
    g_Ept.Installed = FALSE;
    g_Ept.OriginHidden = FALSE;
    g_Ept.Stale = FALSE;
    InterlockedExchange(&g_Ept.Closing, 0);
    InterlockedExchange(&g_Ept.InFlight, 0);

    KeReleaseMutex(&g_EptLock, FALSE);

    //
    // 1) 先把 origin 页藏起来（数据读全 0、执行真实页）。即使这一步之后
    //    安装失败，也只需要把 Hide hook 撤掉。
    //
    status = NoumenEptKernelHook(
        origin,
        NOUMEN_HOOK_KIND_HIDE,
        0,
        0,
        Processor,
        &hideReply,
        &cpu);

    if (!NT_SUCCESS(status)) {
        goto Failed;
    }

    if (hideReply != 0) {
        //
        // hypervisor 拒绝了 Hide（例如槽位/内存不足）。传输本身成功，把
        // HookError 放进 Reply，让 uapp 显示具体原因。
        //
        hookReply = hideReply;
        status = STATUS_SUCCESS;
        goto Failed;
    }

    hideInstalled = TRUE;

    //
    // 2) 装 proxy hook。hypervisor 在这里写 origin。
    //
    status = NoumenEptKernelHook(
        VirtualAddress,
        NOUMEN_HOOK_KIND_DETOUR_TRAMPOLINE,
        g_Ept.Proxy,
        origin,
        Processor,
        &hookReply,
        &cpu);

    if (!NT_SUCCESS(status)) {
        if (NT_SUCCESS(
                NoumenEptKernelUnhook(origin, Processor, &ignoredReply, &ignoredCpu)) &&
            ignoredReply == 0) {
            hideRemoved = TRUE;
        }

        goto Failed;
    }

    if (hookReply != 0) {
        if (NT_SUCCESS(
                NoumenEptKernelUnhook(origin, Processor, &ignoredReply, &ignoredCpu)) &&
            ignoredReply == 0) {
            hideRemoved = TRUE;
        }

        status = STATUS_SUCCESS;
        goto Failed;
    }

    NoumenNtCreateFileResetLog();

    //
    // 让所有已虚拟化的核立即应用 origin 的 Hide 和 proxy hook，缩小
    // “其它核还没 sync、仍执行真实 NtCreateFile / 看到 origin 真实字节”的窗口。
    //
    NoumenEptApplyOnAllProcessors();

    if (NT_SUCCESS(
            KeWaitForMutexObject(&g_EptLock, Executive, KernelMode, FALSE, NULL))) {
        g_Ept.Installed = TRUE;
        g_Ept.OriginHidden = TRUE;
        g_Ept.Busy = 0;
        KeReleaseMutex(&g_EptLock, FALSE);
    }

    *Reply = hookReply;

    if (ActualProcessor != NULL) {
        *ActualProcessor = cpu;
    }

    return STATUS_SUCCESS;

Failed:
    if (NT_SUCCESS(
            KeWaitForMutexObject(&g_EptLock, Executive, KernelMode, FALSE, NULL))) {
        g_Ept.Installed = FALSE;
        g_Ept.OriginHidden = FALSE;
        g_Ept.Target = 0;
        g_Ept.Proxy = 0;
        g_Ept.Busy = 0;
        KeReleaseMutex(&g_EptLock, FALSE);
    }

    //
    // Hide hook 装上之后，只有确认已经撤掉才能释放 origin 页；撤不掉就把
    // 页留在 g_Ept.Origin（Closing 保持 1），下一次安装会拒绝而不是复用。
    //
    if (origin != 0 && (!hideInstalled || hideRemoved)) {
        if (NT_SUCCESS(
                KeWaitForMutexObject(&g_EptLock, Executive, KernelMode, FALSE, NULL))) {
            if (g_Ept.Origin == origin) {
                g_Ept.Origin = 0;
                g_Ept.OriginRaw = 0;
            }

            KeReleaseMutex(&g_EptLock, FALSE);
        }

        ExFreePoolWithTag(
            (PVOID)(ULONG_PTR)(originRaw != 0 ? originRaw : origin),
            NOUMEN_EPT_PROXY_POOL_TAG);
    } else if (hideInstalled && !hideRemoved) {
        KeWaitForMutexObject(&g_EptLock, Executive, KernelMode, FALSE, NULL);
        g_Ept.Stale = TRUE;
        KeReleaseMutex(&g_EptLock, FALSE);
        InterlockedExchange(&g_Ept.Closing, 1);
    }

    *Reply = hookReply;

    if (ActualProcessor != NULL) {
        *ActualProcessor = cpu;
    }

    return status;
}

static
NTSTATUS
NoumenEptProxyHookRemove(
    _In_ UINT64 VirtualAddress,
    _In_ ULONG Processor,
    _Out_ PUINT64 Reply,
    _Out_opt_ PULONG ActualProcessor
    )
{
    NTSTATUS status;
    UINT64 hookReply = 0;
    UINT64 hideReply = 0;
    UINT64 origin;
    UINT64 originRaw;
    ULONG cpu = 0;
    ULONG ignoredCpu = 0;
    BOOLEAN installed;
    BOOLEAN retired;
    BOOLEAN quiescent;
    NTSTATUS hideStatus;

    if (Reply == NULL) {
        return STATUS_INVALID_PARAMETER;
    }

    *Reply = 0;

    if (!NoumenEptProxySupported()) {
        return STATUS_NOT_SUPPORTED;
    }

    status = KeWaitForMutexObject(&g_EptLock, Executive, KernelMode, FALSE, NULL);

    if (!NT_SUCCESS(status)) {
        return status;
    }

    if (g_Ept.Busy) {
        KeReleaseMutex(&g_EptLock, FALSE);
        return STATUS_INVALID_DEVICE_STATE;
    }

    installed = g_Ept.Installed;

    if (installed && g_Ept.Target != VirtualAddress) {
        KeReleaseMutex(&g_EptLock, FALSE);
        return STATUS_INVALID_PARAMETER;
    }

    origin = g_Ept.Origin;
    originRaw = g_Ept.OriginRaw;
    g_Ept.Busy = 1;

    KeReleaseMutex(&g_EptLock, FALSE);

    status = NoumenEptKernelUnhook(
        VirtualAddress,
        Processor,
        &hookReply,
        &cpu);

    if (!NT_SUCCESS(status)) {
        //
        // 传输失败：hook 很可能还活着，恢复 proxy 的正常执行。
        //
        KeWaitForMutexObject(&g_EptLock, Executive, KernelMode, FALSE, NULL);
        g_Ept.Installed = installed;
        g_Ept.Busy = 0;
        KeReleaseMutex(&g_EptLock, FALSE);
        return status;
    }

    if (NT_SUCCESS(
            KeWaitForMutexObject(&g_EptLock, Executive, KernelMode, FALSE, NULL))) {
        g_Ept.Installed = FALSE;
        KeReleaseMutex(&g_EptLock, FALSE);
    }

    //
    // 先让每个核都恢复目标页 leaf，但暂时不关 proxy：还在 retire 的核如果
    // 命中旧 fake 页，origin 仍然有效，不会让 NtCreateFile 失败。
    //
    NoumenEptApplyOnAllProcessors();
    retired = NoumenEptWaitRetired(500);

    if (!retired) {
        //
        // 有核没在超时内应用 retire。留着 origin 和 proxy 可用，只标记
        // Stale 阻止下一次安装复用/清零这页。
        //
        KeWaitForMutexObject(&g_EptLock, Executive, KernelMode, FALSE, NULL);
        g_Ept.Stale = TRUE;
        g_Ept.Busy = 0;
        KeReleaseMutex(&g_EptLock, FALSE);

        *Reply = hookReply;

        if (ActualProcessor != NULL) {
            *ActualProcessor = cpu;
        }

        return STATUS_SUCCESS;
    }

    //
    // 所有核都不再执行 fake 页了：从这一刻起 proxy 只可能被已经进入的调用
    // 使用。等它们退出后就不会再有人碰 origin。
    //
    InterlockedExchange(&g_Ept.Closing, 1);
    quiescent = NoumenEptWaitQuiescent(5000);

    if (!quiescent) {
        KeWaitForMutexObject(&g_EptLock, Executive, KernelMode, FALSE, NULL);
        g_Ept.Stale = TRUE;
        g_Ept.Busy = 0;
        KeReleaseMutex(&g_EptLock, FALSE);

        *Reply = hookReply;

        if (ActualProcessor != NULL) {
            *ActualProcessor = cpu;
        }

        return STATUS_SUCCESS;
    }

    //
    // 最后摘掉 origin 页的 Hide hook，再确认一次所有核都恢复了。
    //
    if (origin != 0) {
        hideStatus = NoumenEptKernelUnhook(
            origin,
            Processor,
            &hideReply,
            &ignoredCpu);

        //
        // Hide hook 没摘掉时不能释放它映射的页。hypervisor 已经停止时
        // EPT 不再生效，可以安全释放；否则保留页并标记 Stale。
        //
        if (InterlockedCompareExchange(&g_NoumenHypervisor.State, 0, 0) ==
                NoumenStateRunning &&
            ((!NT_SUCCESS(hideStatus)) || (hideReply != 0 && hideReply != 7))) {
            KeWaitForMutexObject(&g_EptLock, Executive, KernelMode, FALSE, NULL);
            g_Ept.Stale = TRUE;
            g_Ept.Busy = 0;
            KeReleaseMutex(&g_EptLock, FALSE);

            *Reply = hookReply;

            if (ActualProcessor != NULL) {
                *ActualProcessor = cpu;
            }

            return STATUS_SUCCESS;
        }

        NoumenEptApplyOnAllProcessors();
        retired = NoumenEptWaitRetired(500);

        if (!retired) {
            KeWaitForMutexObject(&g_EptLock, Executive, KernelMode, FALSE, NULL);
            g_Ept.Stale = TRUE;
            g_Ept.Busy = 0;
            KeReleaseMutex(&g_EptLock, FALSE);

            *Reply = hookReply;

            if (ActualProcessor != NULL) {
                *ActualProcessor = cpu;
            }

            return STATUS_SUCCESS;
        }
    }

    if (NT_SUCCESS(
            KeWaitForMutexObject(&g_EptLock, Executive, KernelMode, FALSE, NULL))) {
        if (g_Ept.Origin == origin) {
            g_Ept.Origin = 0;
            g_Ept.OriginRaw = 0;
        }

        g_Ept.Target = 0;
        g_Ept.Proxy = 0;
        g_Ept.OriginHidden = FALSE;
        g_Ept.Stale = FALSE;
        g_Ept.Busy = 0;
        KeReleaseMutex(&g_EptLock, FALSE);
    }

    if (origin != 0) {
        ExFreePoolWithTag(
            (PVOID)(ULONG_PTR)(originRaw != 0 ? originRaw : origin),
            NOUMEN_EPT_PROXY_POOL_TAG);
    }

    InterlockedExchange(&g_Ept.Closing, 0);

    *Reply = hookReply;

    if (ActualProcessor != NULL) {
        *ActualProcessor = cpu;
    }

    return STATUS_SUCCESS;
}

//
// Stop / 卸载时的兜底：hypervisor 还在运行就摘 hook 并等 retire，然后释放页。
// 只在确认没有 proxy 调用时才释放；否则保留（驱动即将卸载，泄漏一页比蓝屏好）。
//
static
VOID
NoumenEptProxyRelease(
    _In_ BOOLEAN RemoveFromHypervisor
    )
{
    UINT64 reply = 0;
    UINT64 ignoredReply = 0;
    UINT64 origin;
    UINT64 originRaw;
    ULONG cpu = 0;
    ULONG ignoredCpu = 0;
    BOOLEAN installed;
    BOOLEAN retired = TRUE;
    NTSTATUS hideStatus = STATUS_SUCCESS;

    if (!NT_SUCCESS(
            KeWaitForMutexObject(&g_EptLock, Executive, KernelMode, FALSE, NULL))) {
        return;
    }

    installed = g_Ept.Installed;
    origin = g_Ept.Origin;
    originRaw = g_Ept.OriginRaw;
    g_Ept.Busy = 1;
    g_Ept.Installed = FALSE;
    InterlockedExchange(&g_Ept.Closing, 1);
    KeReleaseMutex(&g_EptLock, FALSE);

    if (!installed && origin == 0) {
        if (NT_SUCCESS(
                KeWaitForMutexObject(&g_EptLock, Executive, KernelMode, FALSE, NULL))) {
            g_Ept.Busy = 0;
            g_Ept.Stale = FALSE;
            KeReleaseMutex(&g_EptLock, FALSE);
        }

        InterlockedExchange(&g_Ept.Closing, 0);
        return;
    }

    if (RemoveFromHypervisor &&
        InterlockedCompareExchange(&g_NoumenHypervisor.State, 0, 0) == NoumenStateRunning) {
        if (g_Ept.Target != 0) {
            (VOID)NoumenEptKernelUnhook(
                g_Ept.Target,
                NOUMEN_HYPERCALL_PROCESSOR_ANY,
                &reply,
                &cpu);
        }

        NoumenEptApplyOnAllProcessors();
        retired = NoumenEptWaitRetired(500);
        (VOID)NoumenEptWaitQuiescent(5000);

        if (origin != 0) {
            hideStatus = NoumenEptKernelUnhook(
                origin,
                NOUMEN_HYPERCALL_PROCESSOR_ANY,
                &ignoredReply,
                &ignoredCpu);

            NoumenEptApplyOnAllProcessors();
            retired = NoumenEptWaitRetired(500) && retired;
        }
    }

    if (!retired ||
        (RemoveFromHypervisor &&
         InterlockedCompareExchange(&g_NoumenHypervisor.State, 0, 0) == NoumenStateRunning &&
         ((!NT_SUCCESS(hideStatus)) || (ignoredReply != 0 && ignoredReply != 7)))) {
        //
        // 还有核没有恢复 leaf，或 origin 的 Hide hook 没摘掉：保留 origin
        // （Closing 已经是 1，stale proxy 不会碰它），等 STOP 或下一次 remove。
        //
        if (NT_SUCCESS(
                KeWaitForMutexObject(&g_EptLock, Executive, KernelMode, FALSE, NULL))) {
            g_Ept.Stale = TRUE;
            g_Ept.Busy = 0;
            KeReleaseMutex(&g_EptLock, FALSE);
        }

        return;
    }

    if (InterlockedCompareExchange(&g_Ept.InFlight, 0, 0) != 0) {
        //
        // 还有线程在 origin 里：留页、留 Closing=1，绝不释放。
        //
        if (NT_SUCCESS(
                KeWaitForMutexObject(&g_EptLock, Executive, KernelMode, FALSE, NULL))) {
            g_Ept.Stale = TRUE;
            g_Ept.Busy = 0;
            KeReleaseMutex(&g_EptLock, FALSE);
        }

        return;
    }

    if (NT_SUCCESS(
            KeWaitForMutexObject(&g_EptLock, Executive, KernelMode, FALSE, NULL))) {
        if (g_Ept.Origin == origin) {
            g_Ept.Origin = 0;
            g_Ept.OriginRaw = 0;
        }

        g_Ept.Target = 0;
        g_Ept.Proxy = 0;
        g_Ept.OriginHidden = FALSE;
        g_Ept.Stale = FALSE;
        g_Ept.Busy = 0;
        KeReleaseMutex(&g_EptLock, FALSE);
    }

    if (origin != 0) {
        ExFreePoolWithTag(
            (PVOID)(ULONG_PTR)(originRaw != 0 ? originRaw : origin),
            NOUMEN_EPT_PROXY_POOL_TAG);
    }

    InterlockedExchange(&g_Ept.Closing, 0);
}

//
// OS 引导信息（ntoskrnl 基址 / System EPROCESS）。
//
// 这是**驱动**的职责，不是 hypervisor 的：hypervisor 只提供通用内存原语，
// `KernelArgs` 里没有任何 OS 字段。uapp 通过 `GetOsInfo` 拿到这三个值后，
// 自己解析导出、选偏移表、遍历进程/模块。
//
// 定位方式：`RtlPcToFileHeader(ntoskrnl 内任意代码地址)`。不要用“向下扫 MZ”：
// 内核非分页区的无效地址会直接 bugcheck 0x50，`__try/__except` 抓不到。
//
static
ULONG_PTR
NoumenFindNtosBase(
    _Out_ PULONG SizeOut
    )
{
    typedef
    PVOID
    (NTAPI *RTL_PC_TO_FILE_HEADER)(
        _In_ PVOID PcValue,
        _Out_ PVOID *BaseOfImage
        );

    UNICODE_STRING pcRoutineName = RTL_CONSTANT_STRING(L"NtCreateFile");
    UNICODE_STRING headerRoutineName = RTL_CONSTANT_STRING(L"RtlPcToFileHeader");
    RTL_PC_TO_FILE_HEADER headerRoutine;
    PVOID pc;
    PVOID imageBase = NULL;
    ULONG_PTR base;
    ULONG size = 0;

    *SizeOut = 0;

    pc = MmGetSystemRoutineAddress(&pcRoutineName);
    headerRoutine = (RTL_PC_TO_FILE_HEADER)(ULONG_PTR)MmGetSystemRoutineAddress(
        &headerRoutineName);

    if (pc == NULL || headerRoutine == NULL) {
        return 0;
    }

    if (headerRoutine(pc, &imageBase) == NULL || imageBase == NULL) {
        return 0;
    }

    base = (ULONG_PTR)imageBase;

    // 基址一定有效；SEH 只兜底。
    __try {
        PUCHAR probe = (PUCHAR)base;

        if (probe[0] == 'M' && probe[1] == 'Z') {
            ULONG e_lfanew = *(ULONG *)(probe + 0x3C);

            if (e_lfanew >= 0x40 && e_lfanew <= 0x1000 &&
                *(ULONG *)(probe + e_lfanew) == 0x00004550) {
                size = *(ULONG *)(probe + e_lfanew + 0x18 + 0x38);
            }
        }
    } __except (EXCEPTION_EXECUTE_HANDLER) {
        size = 0;
    }

    if (size >= 0x100000 && size <= 0x40000000) {
        *SizeOut = size;
    }

    return base;
}

static
ULONG
NoumenOsBuildNumber(
    VOID
    )
{
    RTL_OSVERSIONINFOW info;

    RtlZeroMemory(&info, sizeof(info));
    info.dwOSVersionInfoSize = sizeof(info);

    if (NT_SUCCESS(RtlGetVersion(&info))) {
        return info.dwBuildNumber;
    }

    return 0;
}

NTSTATUS
NoumenHypervisorGetOsInfo(
    _Out_ PNOUMEN_OS_INFO Info
    )
{
    ULONG size = 0;
    ULONG_PTR base;

    if (Info == NULL) {
        return STATUS_INVALID_PARAMETER;
    }

    RtlZeroMemory(Info, sizeof(*Info));

    Info->Version = NOUMEN_OS_INFO_VERSION;
    Info->Build = NoumenOsBuildNumber();

    base = NoumenFindNtosBase(&size);

    if (base != 0) {
        Info->NtosBase = (UINT64)base;
        Info->NtosSize = size;
        Info->Flags |= NOUMEN_OS_INFO_FLAG_NTOS;
    }

    if (PsInitialSystemProcess != NULL) {
        Info->SystemEprocess = (UINT64)(ULONG_PTR)PsInitialSystemProcess;
        Info->Flags |= NOUMEN_OS_INFO_FLAG_EPROCESS;
    }

    return STATUS_SUCCESS;
}

NTSTATUS
NoumenHypervisorResolveNtCreateFile(
    _Out_ PUINT64 Address
    )
{
    UNICODE_STRING name = RTL_CONSTANT_STRING(L"NtCreateFile");
    PVOID routine;

    if (Address == NULL) {
        return STATUS_INVALID_PARAMETER;
    }

    routine = MmGetSystemRoutineAddress(&name);

    if (routine == NULL) {
        return STATUS_PROCEDURE_NOT_FOUND;
    }

    *Address = (UINT64)(ULONG_PTR)routine;

    return STATUS_SUCCESS;
}

NTSTATUS
NoumenHypervisorReadKernelImage(
    _In_ ULONG Offset,
    _In_ ULONG Length,
    _Out_writes_bytes_(Length) PVOID Output
    )
{
    NTSTATUS waitStatus;
    PUCHAR base;
    ULONG size;
    KAFFINITY previousAffinity = 0;
    BOOLEAN pinned = FALSE;

    if (Output == NULL || Length == 0) {
        return STATUS_INVALID_PARAMETER;
    }

    waitStatus = KeWaitForMutexObject(&g_HypervisorLock, Executive, KernelMode, FALSE, NULL);

    if (!NT_SUCCESS(waitStatus)) {
        return waitStatus;
    }

    base = (PUCHAR)g_NoumenHypervisor.Image.ImageBase;
    size = g_NoumenHypervisor.Image.ImageSize;

    if (base == NULL ||
        Offset > size ||
        Length > size - Offset) {
        KeReleaseMutex(&g_HypervisorLock, FALSE);
        return STATUS_INVALID_ADDRESS;
    }

    //
    // 在 StartProcessor 上读：HideSelf 已经在该核的 EPT 里生效，且 INVEPT
    // 清掉了旧翻译。其它核可能还没 sync，读到真实页会让测试结论错误。
    //
    if (g_NoumenHypervisor.StartProcessor < 64 &&
        KeGetCurrentProcessorNumber() != g_NoumenHypervisor.StartProcessor) {
        previousAffinity = KeSetSystemAffinityThreadEx(
            (KAFFINITY)(1ULL << g_NoumenHypervisor.StartProcessor));
        pinned = TRUE;
    }

    //
    // 这是 guest 侧读取：走 guest 页表 + EPT。HideSelf 之后这里读到的是
    // dummy 全 0；root 侧的 READ_SELF hypercall 才读真实页。
    //
    RtlCopyMemory(Output, base + Offset, Length);

    if (pinned) {
        KeRevertToUserAffinityThreadEx(previousAffinity);
    }

    KeReleaseMutex(&g_HypervisorLock, FALSE);

    return STATUS_SUCCESS;
}

NTSTATUS
NoumenHypervisorStart(
    _In_ BOOLEAN SingleCore,
    _In_ BOOLEAN RandomizeCore
    )
{
    NOUMEN_START_CONTEXT startContext;
    HANDLE threadHandle;
    ULONG cpuCount;
    ULONG processorIndex;
    NTSTATUS status;
    NTSTATUS waitStatus;

    waitStatus = KeWaitForMutexObject(&g_HypervisorLock, Executive, KernelMode, FALSE, NULL);

    if (!NT_SUCCESS(waitStatus)) {
        return waitStatus;
    }

    if (g_NoumenHypervisor.State == NoumenStateRunning) {
        KeReleaseMutex(&g_HypervisorLock, FALSE);
        return STATUS_SUCCESS;
    }

    if (g_NoumenHypervisor.EntryInvoked) {
        KeReleaseMutex(&g_HypervisorLock, FALSE);
        return STATUS_UNSUCCESSFUL;
    }

    cpuCount = NoumenActiveProcessorCount();

    if (g_NoumenHypervisor.KernelArgsInitialized &&
        g_NoumenHypervisor.KernelArgs.CpuCount != 0 &&
        g_NoumenHypervisor.KernelArgs.CpuCount < cpuCount) {
        cpuCount = g_NoumenHypervisor.KernelArgs.CpuCount;
    }

    processorIndex = NoumenSelectProcessor(RandomizeCore, cpuCount);

    g_NoumenHypervisor.StartProcessor = 0;

    KeInitializeEvent(&startContext.Completion, NotificationEvent, FALSE);
    startContext.Status = STATUS_UNSUCCESSFUL;
    startContext.SingleCore = SingleCore;
    startContext.ProcessorIndex = processorIndex;
    startContext.CpuCount = cpuCount;
    threadHandle = NULL;

    status = PsCreateSystemThread(
        &threadHandle,
        THREAD_ALL_ACCESS,
        NULL,
        NULL,
        NULL,
        NoumenStartThread,
        &startContext);

    if (!NT_SUCCESS(status)) {
        KeReleaseMutex(&g_HypervisorLock, FALSE);
        return status;
    }

    KeWaitForSingleObject(&startContext.Completion, Executive, KernelMode, FALSE, NULL);
    ZwClose(threadHandle);

    status = startContext.Status;

    if (NT_SUCCESS(status)) {
#if NOUMEN_ENABLE_LOGGER
        (VOID)NoumenLogDumpToFile();
        (VOID)NoumenLogStart();
#endif
    } else {
        (VOID)NoumenLogDumpFailure("virtualization failed", status);
    }

    KeReleaseMutex(&g_HypervisorLock, FALSE);

    return status;
}

NTSTATUS
NoumenHypervisorGetKernelArgs(
    _Out_ PNOUMEN_KERNEL_ARGS Args
    )
{
    NTSTATUS waitStatus;

    if (Args == NULL) {
        return STATUS_INVALID_PARAMETER;
    }

    waitStatus = KeWaitForMutexObject(&g_HypervisorLock, Executive, KernelMode, FALSE, NULL);

    if (!NT_SUCCESS(waitStatus)) {
        return waitStatus;
    }

    *Args = g_NoumenHypervisor.KernelArgs;

    KeReleaseMutex(&g_HypervisorLock, FALSE);

    return STATUS_SUCCESS;
}

NTSTATUS
NoumenHypervisorSetKernelArgs(
    _In_ const NOUMEN_KERNEL_ARGS *Args
    )
{
    NTSTATUS waitStatus;

    if (Args == NULL) {
        return STATUS_INVALID_PARAMETER;
    }

    waitStatus = KeWaitForMutexObject(&g_HypervisorLock, Executive, KernelMode, FALSE, NULL);

    if (!NT_SUCCESS(waitStatus)) {
        return waitStatus;
    }

    if (g_NoumenHypervisor.EntryInvoked) {
        KeReleaseMutex(&g_HypervisorLock, FALSE);
        return STATUS_UNSUCCESSFUL;
    }

    g_NoumenHypervisor.KernelArgs = *Args;
    g_NoumenHypervisor.KernelArgsInitialized = TRUE;

    KeReleaseMutex(&g_HypervisorLock, FALSE);

    return STATUS_SUCCESS;
}

NTSTATUS
NoumenHypervisorSetWinOffsets(
    _In_ const NOUMEN_WIN_OFFSETS *Offsets
    )
{
    NTSTATUS waitStatus;

    if (Offsets == NULL) {
        return STATUS_INVALID_PARAMETER;
    }

    waitStatus = KeWaitForMutexObject(&g_HypervisorLock, Executive, KernelMode, FALSE, NULL);

    if (!NT_SUCCESS(waitStatus)) {
        return waitStatus;
    }

    if (g_NoumenHypervisor.EntryInvoked) {
        KeReleaseMutex(&g_HypervisorLock, FALSE);
        return STATUS_UNSUCCESSFUL;
    }

    if (Offsets->Build == 0) {
        g_NoumenHypervisor.WinOffsetsProvided = FALSE;
        RtlZeroMemory(&g_NoumenHypervisor.WinOffsets, sizeof(g_NoumenHypervisor.WinOffsets));
    } else {
        g_NoumenHypervisor.WinOffsets = *Offsets;
        g_NoumenHypervisor.WinOffsetsProvided = TRUE;
    }

    KeReleaseMutex(&g_HypervisorLock, FALSE);

    return STATUS_SUCCESS;
}

NTSTATUS
NoumenHypervisorSetImagePath(
    _In_reads_bytes_(Length) const WCHAR *Path,
    _In_ ULONG Length
    )
{
    WCHAR buffer[NOUMEN_IMAGE_PATH_CHARS];
    UNICODE_STRING ntPath;
    NTSTATUS status;
    NTSTATUS waitStatus;

    status = NoumenBuildNtPath(Path, Length, &ntPath, buffer, NOUMEN_IMAGE_PATH_CHARS);

    if (!NT_SUCCESS(status)) {
        return status;
    }

    waitStatus = KeWaitForMutexObject(&g_HypervisorLock, Executive, KernelMode, FALSE, NULL);

    if (!NT_SUCCESS(waitStatus)) {
        return waitStatus;
    }

    if (g_NoumenHypervisor.EntryInvoked) {
        KeReleaseMutex(&g_HypervisorLock, FALSE);
        return STATUS_UNSUCCESSFUL;
    }

    NoumenElfFree(&g_NoumenHypervisor.Image);
    g_NoumenHypervisor.ImageFromMemory = FALSE;
    g_NoumenHypervisor.KernelArgsInitialized = FALSE;
    g_NoumenHypervisor.WinOffsetsProvided = FALSE;
    RtlZeroMemory(&g_NoumenHypervisor.WinOffsets, sizeof(g_NoumenHypervisor.WinOffsets));
    RtlZeroMemory(&g_NoumenHypervisor.KernelArgs, sizeof(g_NoumenHypervisor.KernelArgs));

    RtlCopyMemory(g_NoumenImagePathBuffer, ntPath.Buffer, ntPath.Length + sizeof(WCHAR));
    g_NoumenImagePath.Length = ntPath.Length;
    g_NoumenImagePath.MaximumLength = ntPath.MaximumLength;

    KeReleaseMutex(&g_HypervisorLock, FALSE);

    return STATUS_SUCCESS;
}

NTSTATUS
NoumenHypervisorLoadImageFromMemory(
    _In_reads_bytes_(Size) const VOID *Data,
    _In_ ULONG Size
    )
{
    NTSTATUS status;
    NTSTATUS waitStatus;

    if (Data == NULL || Size == 0) {
        return STATUS_INVALID_PARAMETER;
    }

    waitStatus = KeWaitForMutexObject(&g_HypervisorLock, Executive, KernelMode, FALSE, NULL);

    if (!NT_SUCCESS(waitStatus)) {
        return waitStatus;
    }

    if (g_NoumenHypervisor.EntryInvoked) {
        KeReleaseMutex(&g_HypervisorLock, FALSE);
        return STATUS_UNSUCCESSFUL;
    }

    NoumenElfFree(&g_NoumenHypervisor.Image);
    g_NoumenHypervisor.ImageFromMemory = FALSE;
    g_NoumenHypervisor.KernelArgsInitialized = FALSE;
    g_NoumenHypervisor.WinOffsetsProvided = FALSE;
    RtlZeroMemory(&g_NoumenHypervisor.WinOffsets, sizeof(g_NoumenHypervisor.WinOffsets));
    RtlZeroMemory(&g_NoumenHypervisor.KernelArgs, sizeof(g_NoumenHypervisor.KernelArgs));

    status = NoumenElfLoadFromMemory(Data, Size, 0, &g_NoumenHypervisor.Image);

    if (NT_SUCCESS(status)) {
        g_NoumenHypervisor.ImageFromMemory = TRUE;
    }

    KeReleaseMutex(&g_HypervisorLock, FALSE);

    return status;
}

VOID
NoumenHypervisorCleanup(
    VOID
    )
{
    NTSTATUS waitStatus;

    //
    // 先注销进程退出回调，再触碰注册表，然后释放所有锁住的目标页。正常
    // Stop 已经移除过 hook；这里是“没有 Stop 就卸载”的兜底。
    //
    (VOID)PsSetCreateProcessNotifyRoutineEx(NoumenEptProcessExitNotify, TRUE);

    NoumenEptHooksReleaseAll(FALSE);

    //
    // NtCreateFile hook：先摘掉（hypervisor 还在运行时发 0x31），再决定能不能
    // 释放 trampoline / 日志缓冲。hypervisor 没停就卸载驱动时，那页仍可能被
    // guest 执行，和镜像一样留在内存里比释放安全。
    //
    NoumenNtCreateFileRelease(TRUE);
    NoumenEptProxyRelease(
        InterlockedCompareExchange(&g_NoumenHypervisor.State, 0, 0) == NoumenStateRunning);

    if (InterlockedCompareExchange(&g_NoumenHypervisor.State, 0, 0) != NoumenStateRunning) {
        NoumenNtCreateFileFreeResources();
    }

    waitStatus = KeWaitForMutexObject(&g_HypervisorLock, Executive, KernelMode, FALSE, NULL);

    if (!NT_SUCCESS(waitStatus)) {
        NoumenLogStop();
        return;
    }

    NoumenLogStop();

    if (!g_NoumenHypervisor.EntryInvoked) {
        NoumenElfFree(&g_NoumenHypervisor.Image);
        NoumenLogFree();
        NoumenFreeKernelPfnList();
        }

    InterlockedExchange(&g_NoumenHypervisor.State, NoumenStateUnloaded);

    KeReleaseMutex(&g_HypervisorLock, FALSE);
}
