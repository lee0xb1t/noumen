#include "driver.h"

C_ASSERT(sizeof(NOUMEN_INFO) == 72);
C_ASSERT(sizeof(NOUMEN_OS_INFO) == 40);
C_ASSERT(FIELD_OFFSET(NOUMEN_OS_INFO, NtosBase) == 0x10);
C_ASSERT(FIELD_OFFSET(NOUMEN_OS_INFO, SystemEprocess) == 0x20);
C_ASSERT(sizeof(NOUMEN_KERNEL_ARGS) == 168);
C_ASSERT(sizeof(NOUMEN_WIN_OFFSETS) == 56);
C_ASSERT(FIELD_OFFSET(NOUMEN_KERNEL_ARGS, WinOffsets) == 0x70);
C_ASSERT(FIELD_OFFSET(NOUMEN_KERNEL_ARGS, PhysMapBase) == 0x10);
C_ASSERT(FIELD_OFFSET(NOUMEN_KERNEL_ARGS, SystemCr3) == 0x18);
C_ASSERT(FIELD_OFFSET(NOUMEN_KERNEL_ARGS, KernelPfnBase) == 0x60);
C_ASSERT(sizeof(NOUMEN_START_HYPERVISOR) == 8);
C_ASSERT(sizeof(NOUMEN_HYPERCALL_PING_REQUEST) == 16);
C_ASSERT(sizeof(NOUMEN_HYPERCALL_PING_RESPONSE) == 16);
C_ASSERT(sizeof(NOUMEN_EPT_HOOK_REQUEST) == 40);
C_ASSERT(sizeof(NOUMEN_EPT_HOOK_RESPONSE) == 16);
C_ASSERT(sizeof(NOUMEN_READ_KERNEL_IMAGE) == 8);
C_ASSERT(sizeof(NOUMEN_SET_IMAGE_PATH) == 528);
C_ASSERT(FIELD_OFFSET(NOUMEN_SET_IMAGE_PATH, Path) == 8);
C_ASSERT(FIELD_OFFSET(NOUMEN_SET_LOG_PATH, Path) == 8);
C_ASSERT(sizeof(NOUMEN_NT_CREATE_FILE_REQUEST) == 16);
C_ASSERT(sizeof(NOUMEN_NT_CREATE_FILE_LOG) == 312);
C_ASSERT(FIELD_OFFSET(NOUMEN_NT_CREATE_FILE_LOG, ObjectName) == 48);
C_ASSERT(FIELD_OFFSET(NOUMEN_NT_CREATE_FILE_LOG, Flags) == 304);
C_ASSERT(sizeof(NOUMEN_NT_CREATE_FILE_RESPONSE) == 56);

static
NTSTATUS
NoumenHandlePing(
    _In_reads_bytes_(InputLength) const VOID *Input,
    _In_ ULONG InputLength,
    _Out_writes_bytes_(OutputLength) VOID *Output,
    _In_ ULONG OutputLength,
    _Out_ PULONG Written
    );

static
NTSTATUS
NoumenHandleGetInfo(
    _In_reads_bytes_(InputLength) const VOID *Input,
    _In_ ULONG InputLength,
    _Out_writes_bytes_(OutputLength) VOID *Output,
    _In_ ULONG OutputLength,
    _Out_ PULONG Written
    );

static
NTSTATUS
NoumenHandleSetImagePath(
    _In_reads_bytes_(InputLength) const VOID *Input,
    _In_ ULONG InputLength,
    _Out_writes_bytes_(OutputLength) VOID *Output,
    _In_ ULONG OutputLength,
    _Out_ PULONG Written
    );

static
NTSTATUS
NoumenHandleSetLogPath(
    _In_reads_bytes_(InputLength) const VOID *Input,
    _In_ ULONG InputLength,
    _Out_writes_bytes_(OutputLength) VOID *Output,
    _In_ ULONG OutputLength,
    _Out_ PULONG Written
    );

static
NTSTATUS
NoumenHandleStartHypervisor(
    _In_reads_bytes_(InputLength) const VOID *Input,
    _In_ ULONG InputLength,
    _Out_writes_bytes_(OutputLength) VOID *Output,
    _In_ ULONG OutputLength,
    _Out_ PULONG Written
    );

static
NTSTATUS
NoumenHandleGetKernelArgs(
    _In_reads_bytes_(InputLength) const VOID *Input,
    _In_ ULONG InputLength,
    _Out_writes_bytes_(OutputLength) VOID *Output,
    _In_ ULONG OutputLength,
    _Out_ PULONG Written
    );

static
NTSTATUS
NoumenHandleSetKernelArgs(
    _In_reads_bytes_(InputLength) const VOID *Input,
    _In_ ULONG InputLength,
    _Out_writes_bytes_(OutputLength) VOID *Output,
    _In_ ULONG OutputLength,
    _Out_ PULONG Written
    );

static
NTSTATUS
NoumenHandleSetWinOffsets(
    _In_reads_bytes_(InputLength) const VOID *Input,
    _In_ ULONG InputLength,
    _Out_writes_bytes_(OutputLength) VOID *Output,
    _In_ ULONG OutputLength,
    _Out_ PULONG Written
    );

static
NTSTATUS
NoumenHandleStopHypervisor(
    _In_reads_bytes_(InputLength) const VOID *Input,
    _In_ ULONG InputLength,
    _Out_writes_bytes_(OutputLength) VOID *Output,
    _In_ ULONG OutputLength,
    _Out_ PULONG Written
    );

static
NTSTATUS
NoumenHandleHypercallPing(
    _In_reads_bytes_(InputLength) const VOID *Input,
    _In_ ULONG InputLength,
    _Out_writes_bytes_(OutputLength) VOID *Output,
    _In_ ULONG OutputLength,
    _Out_ PULONG Written
    );

static
NTSTATUS
NoumenHandleEptHook(
    _In_reads_bytes_(InputLength) const VOID *Input,
    _In_ ULONG InputLength,
    _Out_writes_bytes_(OutputLength) VOID *Output,
    _In_ ULONG OutputLength,
    _Out_ PULONG Written
    );

static
NTSTATUS
NoumenHandleResolveNtCreateFile(
    _In_reads_bytes_(InputLength) const VOID *Input,
    _In_ ULONG InputLength,
    _Out_writes_bytes_(OutputLength) VOID *Output,
    _In_ ULONG OutputLength,
    _Out_ PULONG Written
    );

static
NTSTATUS
NoumenHandleReadKernelImage(
    _In_reads_bytes_(InputLength) const VOID *Input,
    _In_ ULONG InputLength,
    _Out_writes_bytes_(OutputLength) VOID *Output,
    _In_ ULONG OutputLength,
    _Out_ PULONG Written
    );

static
NTSTATUS
NoumenHandleGetOsInfo(
    _In_reads_bytes_(InputLength) const VOID *Input,
    _In_ ULONG InputLength,
    _Out_writes_bytes_(OutputLength) VOID *Output,
    _In_ ULONG OutputLength,
    _Out_ PULONG Written
    );

static
NTSTATUS
NoumenHandleHookNtCreateFile(
    _In_reads_bytes_(InputLength) const VOID *Input,
    _In_ ULONG InputLength,
    _Out_writes_bytes_(OutputLength) VOID *Output,
    _In_ ULONG OutputLength,
    _Out_ PULONG Written
    );

static NTSTATUS
NoumenHandleSvmHook(
    _In_reads_bytes_(InputLength) const VOID *Input,
    _In_ ULONG InputLength,
    _Out_writes_bytes_(OutputLength) VOID *Output,
    _In_ ULONG OutputLength,
    _Out_ PULONG Written
    )
{
    const NOUMEN_SVM_HOOK_REQUEST *request;
    NOUMEN_EPT_HOOK_RESPONSE *response;
    UINT64 reply = 0;
    ULONG processor = 0;
    NTSTATUS status;

    if (InputLength < sizeof(NOUMEN_SVM_HOOK_REQUEST)) {
        return STATUS_INFO_LENGTH_MISMATCH;
    }

    if (OutputLength < sizeof(NOUMEN_EPT_HOOK_RESPONSE)) {
        return STATUS_BUFFER_TOO_SMALL;
    }

    request = (const NOUMEN_SVM_HOOK_REQUEST *)Input;
    response = (NOUMEN_EPT_HOOK_RESPONSE *)Output;
    *Written = 0;

    status = NoumenHypervisorSvmHook(
        request->VirtualAddress,
        request->Handler,
        request->Trampoline,
        request->Method,
        request->Kind,
        request->Remove != 0,
        request->Processor,
        request->ProcessId,
        &reply,
        &processor);

    if (!NT_SUCCESS(status)) {
        return status;
    }

    response->Reply = reply;
    response->Processor = processor;
    response->Reserved = 0;
    *Written = sizeof(*response);

    return STATUS_SUCCESS;
}

static const NOUMEN_COMMAND_HANDLER g_CommandHandlers[NoumenCommandMax] = {
    [NoumenCommandPing] = NoumenHandlePing,
    [NoumenCommandGetInfo] = NoumenHandleGetInfo,
    [NoumenCommandGetKernelArgs] = NoumenHandleGetKernelArgs,
    [NoumenCommandSetKernelArgs] = NoumenHandleSetKernelArgs,
    [NoumenCommandSetImagePath] = NoumenHandleSetImagePath,
    [NoumenCommandSetLogPath] = NoumenHandleSetLogPath,
    [NoumenCommandStartHypervisor] = NoumenHandleStartHypervisor,
    [NoumenCommandStopHypervisor] = NoumenHandleStopHypervisor,
    [NoumenCommandHypercallPing] = NoumenHandleHypercallPing,
    [NoumenCommandEptHook] = NoumenHandleEptHook,
    [NoumenCommandResolveNtCreateFile] = NoumenHandleResolveNtCreateFile,
    [NoumenCommandReadKernelImage] = NoumenHandleReadKernelImage,
    [NoumenCommandGetOsInfo] = NoumenHandleGetOsInfo,
    [NoumenCommandSvmHook] = NoumenHandleSvmHook,
    [NoumenCommandHookNtCreateFile] = NoumenHandleHookNtCreateFile,
    [NoumenCommandSetWinOffsets] = NoumenHandleSetWinOffsets
};

NTSTATUS
NoumenCommandDispatch(
    _In_ ULONG Command,
    _In_reads_bytes_(InputLength) const VOID *Input,
    _In_ ULONG InputLength,
    _Out_writes_bytes_(OutputLength) VOID *Output,
    _In_ ULONG OutputLength,
    _Out_ PULONG Written
    )
{
    if (Written == NULL) {
        return STATUS_INVALID_PARAMETER;
    }

    *Written = 0;

    if (Command >= NoumenCommandMax || g_CommandHandlers[Command] == NULL) {
        return STATUS_NOT_IMPLEMENTED;
    }

    return g_CommandHandlers[Command](
        Input,
        InputLength,
        Output,
        OutputLength,
        Written);
}

static
NTSTATUS
NoumenHandlePing(
    _In_reads_bytes_(InputLength) const VOID *Input,
    _In_ ULONG InputLength,
    _Out_writes_bytes_(OutputLength) VOID *Output,
    _In_ ULONG OutputLength,
    _Out_ PULONG Written
    )
{
    const NOUMEN_PING *request;
    PNOUMEN_PING_RESPONSE response;

    *Written = 0;

    if (InputLength < sizeof(NOUMEN_PING)) {
        return STATUS_INVALID_PARAMETER;
    }

    if (OutputLength < sizeof(NOUMEN_PING_RESPONSE)) {
        return STATUS_BUFFER_TOO_SMALL;
    }

    request = (const NOUMEN_PING *)Input;
    response = (PNOUMEN_PING_RESPONSE)Output;

    response->Value = request->Value;
    response->Magic = NOUMEN_PING_MAGIC;

    *Written = sizeof(NOUMEN_PING_RESPONSE);

    return STATUS_SUCCESS;
}

static
NTSTATUS
NoumenHandleGetInfo(
    _In_reads_bytes_(InputLength) const VOID *Input,
    _In_ ULONG InputLength,
    _Out_writes_bytes_(OutputLength) VOID *Output,
    _In_ ULONG OutputLength,
    _Out_ PULONG Written
    )
{
    PNOUMEN_INFO info;

    UNREFERENCED_PARAMETER(Input);
    UNREFERENCED_PARAMETER(InputLength);

    *Written = 0;

    if (OutputLength < sizeof(NOUMEN_INFO)) {
        return STATUS_BUFFER_TOO_SMALL;
    }

    info = (PNOUMEN_INFO)Output;
    RtlZeroMemory(info, sizeof(NOUMEN_INFO));

    info->ProtocolVersion = NOUMEN_PROTOCOL_VERSION;
    info->State = (ULONG)g_NoumenHypervisor.State;
    info->LastStatus = g_NoumenHypervisor.LastStatus;
    info->CpuCount = g_NoumenHypervisor.CpuCount;
    info->StartProcessor = g_NoumenHypervisor.StartProcessor;
    info->SingleCore = g_NoumenHypervisor.SingleCore ? 1 : 0;
    info->KernelBase = (UINT64)(ULONG_PTR)g_NoumenHypervisor.Image.ImageBase;
    info->KernelSize = g_NoumenHypervisor.Image.ImageSize;
    info->EntryPoint = (UINT64)(ULONG_PTR)g_NoumenHypervisor.Image.EntryPoint;
    info->SymbolBase = (UINT64)(ULONG_PTR)g_NoumenHypervisor.Image.SymbolTable;
    info->SymbolCount = g_NoumenHypervisor.Image.SymbolCount;
    info->LogSize = NoumenLogBufferSize();
    info->HypercallKey = g_NoumenHypervisor.HypercallKey;

    *Written = sizeof(NOUMEN_INFO);

    return STATUS_SUCCESS;
}

static
NTSTATUS
NoumenHandleSetImagePath(
    _In_reads_bytes_(InputLength) const VOID *Input,
    _In_ ULONG InputLength,
    _Out_writes_bytes_(OutputLength) VOID *Output,
    _In_ ULONG OutputLength,
    _Out_ PULONG Written
    )
{
    const NOUMEN_SET_IMAGE_PATH *request;
    ULONG headerSize;

    UNREFERENCED_PARAMETER(Output);
    UNREFERENCED_PARAMETER(OutputLength);

    *Written = 0;

    headerSize = FIELD_OFFSET(NOUMEN_SET_IMAGE_PATH, Path);

    if (InputLength < headerSize) {
        return STATUS_INVALID_PARAMETER;
    }

    request = (const NOUMEN_SET_IMAGE_PATH *)Input;

    if (request->Length == 0 || request->Length > (InputLength - headerSize)) {
        return STATUS_INVALID_PARAMETER;
    }

    return NoumenHypervisorSetImagePath(request->Path, request->Length);
}

static
NTSTATUS
NoumenHandleSetLogPath(
    _In_reads_bytes_(InputLength) const VOID *Input,
    _In_ ULONG InputLength,
    _Out_writes_bytes_(OutputLength) VOID *Output,
    _In_ ULONG OutputLength,
    _Out_ PULONG Written
    )
{
    const NOUMEN_SET_LOG_PATH *request;
    ULONG headerSize;

    UNREFERENCED_PARAMETER(Output);
    UNREFERENCED_PARAMETER(OutputLength);

    *Written = 0;

    headerSize = FIELD_OFFSET(NOUMEN_SET_LOG_PATH, Path);

    if (InputLength < headerSize) {
        return STATUS_INVALID_PARAMETER;
    }

    request = (const NOUMEN_SET_LOG_PATH *)Input;

    if (request->Length == 0 || request->Length > (InputLength - headerSize)) {
        return STATUS_INVALID_PARAMETER;
    }

    return NoumenLogSetPath(request->Path, request->Length);
}

static
NTSTATUS
NoumenHandleStartHypervisor(
    _In_reads_bytes_(InputLength) const VOID *Input,
    _In_ ULONG InputLength,
    _Out_writes_bytes_(OutputLength) VOID *Output,
    _In_ ULONG OutputLength,
    _Out_ PULONG Written
    )
{
    const NOUMEN_START_HYPERVISOR *request;

    UNREFERENCED_PARAMETER(Output);
    UNREFERENCED_PARAMETER(OutputLength);

    *Written = 0;

    if (InputLength < sizeof(NOUMEN_START_HYPERVISOR)) {
        return STATUS_INVALID_PARAMETER;
    }

    request = (const NOUMEN_START_HYPERVISOR *)Input;

    return NoumenHypervisorStart(
        (BOOLEAN)(request->SingleCore != 0),
        (BOOLEAN)(request->RandomizeCore != 0));
}

static
NTSTATUS
NoumenHandleGetKernelArgs(
    _In_reads_bytes_(InputLength) const VOID *Input,
    _In_ ULONG InputLength,
    _Out_writes_bytes_(OutputLength) VOID *Output,
    _In_ ULONG OutputLength,
    _Out_ PULONG Written
    )
{
    NTSTATUS status;

    UNREFERENCED_PARAMETER(Input);
    UNREFERENCED_PARAMETER(InputLength);

    *Written = 0;

    if (OutputLength < sizeof(NOUMEN_KERNEL_ARGS)) {
        return STATUS_BUFFER_TOO_SMALL;
    }

    status = NoumenHypervisorGetKernelArgs((PNOUMEN_KERNEL_ARGS)Output);

    if (NT_SUCCESS(status)) {
        *Written = sizeof(NOUMEN_KERNEL_ARGS);
    }

    return status;
}

static
NTSTATUS
NoumenHandleSetKernelArgs(
    _In_reads_bytes_(InputLength) const VOID *Input,
    _In_ ULONG InputLength,
    _Out_writes_bytes_(OutputLength) VOID *Output,
    _In_ ULONG OutputLength,
    _Out_ PULONG Written
    )
{
    UNREFERENCED_PARAMETER(Output);
    UNREFERENCED_PARAMETER(OutputLength);

    *Written = 0;

    if (InputLength < sizeof(NOUMEN_KERNEL_ARGS)) {
        return STATUS_INVALID_PARAMETER;
    }

    return NoumenHypervisorSetKernelArgs((const NOUMEN_KERNEL_ARGS *)Input);
}

static
NTSTATUS
NoumenHandleSetWinOffsets(
    _In_reads_bytes_(InputLength) const VOID *Input,
    _In_ ULONG InputLength,
    _Out_writes_bytes_(OutputLength) VOID *Output,
    _In_ ULONG OutputLength,
    _Out_ PULONG Written
    )
{
    UNREFERENCED_PARAMETER(Output);
    UNREFERENCED_PARAMETER(OutputLength);

    *Written = 0;

    if (InputLength < sizeof(NOUMEN_WIN_OFFSETS)) {
        return STATUS_INVALID_PARAMETER;
    }

    return NoumenHypervisorSetWinOffsets((const NOUMEN_WIN_OFFSETS *)Input);
}

static
NTSTATUS
NoumenHandleStopHypervisor(
    _In_reads_bytes_(InputLength) const VOID *Input,
    _In_ ULONG InputLength,
    _Out_writes_bytes_(OutputLength) VOID *Output,
    _In_ ULONG OutputLength,
    _Out_ PULONG Written
    )
{
    UNREFERENCED_PARAMETER(Input);
    UNREFERENCED_PARAMETER(InputLength);
    UNREFERENCED_PARAMETER(Output);
    UNREFERENCED_PARAMETER(OutputLength);

    *Written = 0;

    return NoumenHypervisorStop();
}

static
NTSTATUS
NoumenHandleHypercallPing(
    _In_reads_bytes_(InputLength) const VOID *Input,
    _In_ ULONG InputLength,
    _Out_writes_bytes_(OutputLength) VOID *Output,
    _In_ ULONG OutputLength,
    _Out_ PULONG Written
    )
{
    const NOUMEN_HYPERCALL_PING_REQUEST *request;
    PNOUMEN_HYPERCALL_PING_RESPONSE response;
    UINT64 reply;
    ULONG processor;
    NTSTATUS status;

    *Written = 0;

    if (InputLength < sizeof(NOUMEN_HYPERCALL_PING_REQUEST)) {
        return STATUS_INVALID_PARAMETER;
    }

    if (OutputLength < sizeof(NOUMEN_HYPERCALL_PING_RESPONSE)) {
        return STATUS_BUFFER_TOO_SMALL;
    }

    request = (const NOUMEN_HYPERCALL_PING_REQUEST *)Input;

    status = NoumenHypervisorHypercallPing(
        request->Nonce,
        request->Processor,
        &reply,
        &processor);

    if (!NT_SUCCESS(status)) {
        return status;
    }

    response = (PNOUMEN_HYPERCALL_PING_RESPONSE)Output;
    response->Reply = reply;
    response->Processor = processor;
    response->Reserved = 0;

    *Written = sizeof(NOUMEN_HYPERCALL_PING_RESPONSE);

    return STATUS_SUCCESS;
}

static
NTSTATUS
NoumenHandleEptHook(
    _In_reads_bytes_(InputLength) const VOID *Input,
    _In_ ULONG InputLength,
    _Out_writes_bytes_(OutputLength) VOID *Output,
    _In_ ULONG OutputLength,
    _Out_ PULONG Written
    )
{
    const NOUMEN_EPT_HOOK_REQUEST *request;
    PNOUMEN_EPT_HOOK_RESPONSE response;
    UINT64 reply;
    ULONG processor;
    NTSTATUS status;

    *Written = 0;

    if (InputLength < sizeof(NOUMEN_EPT_HOOK_REQUEST)) {
        // Distinct from the semantic checks below: this means uapp and driver
        // disagree about the request layout.
        return STATUS_INFO_LENGTH_MISMATCH;
    }

    if (OutputLength < sizeof(NOUMEN_EPT_HOOK_RESPONSE)) {
        return STATUS_BUFFER_TOO_SMALL;
    }

    request = (const NOUMEN_EPT_HOOK_REQUEST *)Input;

    if (request->Kind > NOUMEN_HOOK_KIND_USER_BREAKPOINT || request->Remove > 1) {
        return STATUS_INVALID_PARAMETER;
    }

    status = NoumenHypervisorEptHook(
        request->VirtualAddress,
        request->Kind,
        request->Remove != 0,
        request->Processor,
        request->ProcessId,
        request->Handler,
        request->Trampoline,
        &reply,
        &processor);

    if (!NT_SUCCESS(status)) {
        return status;
    }

    response = (PNOUMEN_EPT_HOOK_RESPONSE)Output;
    response->Reply = reply;
    response->Processor = processor;
    response->Reserved = 0;

    *Written = sizeof(NOUMEN_EPT_HOOK_RESPONSE);

    return STATUS_SUCCESS;
}

static
NTSTATUS
NoumenHandleResolveNtCreateFile(
    _In_reads_bytes_(InputLength) const VOID *Input,
    _In_ ULONG InputLength,
    _Out_writes_bytes_(OutputLength) VOID *Output,
    _In_ ULONG OutputLength,
    _Out_ PULONG Written
    )
{
    NTSTATUS status;
    UINT64 address;

    UNREFERENCED_PARAMETER(Input);
    UNREFERENCED_PARAMETER(InputLength);

    if (OutputLength < sizeof(UINT64)) {
        return STATUS_BUFFER_TOO_SMALL;
    }

    status = NoumenHypervisorResolveNtCreateFile(&address);

    if (!NT_SUCCESS(status)) {
        return status;
    }

    *(UINT64 *)Output = address;
    *Written = sizeof(UINT64);

    return STATUS_SUCCESS;
}

static
NTSTATUS
NoumenHandleReadKernelImage(
    _In_reads_bytes_(InputLength) const VOID *Input,
    _In_ ULONG InputLength,
    _Out_writes_bytes_(OutputLength) VOID *Output,
    _In_ ULONG OutputLength,
    _Out_ PULONG Written
    )
{
    const NOUMEN_READ_KERNEL_IMAGE *request;
    NTSTATUS status;

    if (InputLength < sizeof(NOUMEN_READ_KERNEL_IMAGE)) {
        return STATUS_INVALID_PARAMETER;
    }

    request = (const NOUMEN_READ_KERNEL_IMAGE *)Input;

    if (request->Length == 0 || OutputLength < request->Length) {
        return STATUS_BUFFER_TOO_SMALL;
    }

    status = NoumenHypervisorReadKernelImage(
        request->Offset,
        request->Length,
        Output);

    if (!NT_SUCCESS(status)) {
        return status;
    }

    *Written = request->Length;

    return STATUS_SUCCESS;
}

static
NTSTATUS
NoumenHandleGetOsInfo(
    _In_reads_bytes_(InputLength) const VOID *Input,
    _In_ ULONG InputLength,
    _Out_writes_bytes_(OutputLength) VOID *Output,
    _In_ ULONG OutputLength,
    _Out_ PULONG Written
    )
{
    NTSTATUS status;

    UNREFERENCED_PARAMETER(Input);
    UNREFERENCED_PARAMETER(InputLength);

    *Written = 0;

    if (OutputLength < sizeof(NOUMEN_OS_INFO)) {
        return STATUS_BUFFER_TOO_SMALL;
    }

    status = NoumenHypervisorGetOsInfo((PNOUMEN_OS_INFO)Output);

    if (!NT_SUCCESS(status)) {
        return status;
    }

    *Written = sizeof(NOUMEN_OS_INFO);

    return STATUS_SUCCESS;
}

//
// NtCreateFile hook：安装、移除或读取命中日志。
//
// 输出布局是“一个 response + N 条 record”，`OutputLength` 决定 N 的上限，
// `MaxRecords` 可以再收紧。安装/移除永远只写 response，调用方不会把返回值
// 误当成记录。
//
static
NTSTATUS
NoumenHandleHookNtCreateFile(
    _In_reads_bytes_(InputLength) const VOID *Input,
    _In_ ULONG InputLength,
    _Out_writes_bytes_(OutputLength) VOID *Output,
    _In_ ULONG OutputLength,
    _Out_ PULONG Written
    )
{
    const NOUMEN_NT_CREATE_FILE_REQUEST *request;
    PNOUMEN_NT_CREATE_FILE_RESPONSE response;
    ULONG capacity;
    ULONG maxRecords;
    ULONG count = 0;
    NTSTATUS status;

    *Written = 0;

    if (InputLength < sizeof(NOUMEN_NT_CREATE_FILE_REQUEST)) {
        // uapp 和驱动对请求布局的理解不一致。
        return STATUS_INFO_LENGTH_MISMATCH;
    }

    if (OutputLength < sizeof(NOUMEN_NT_CREATE_FILE_RESPONSE)) {
        return STATUS_BUFFER_TOO_SMALL;
    }

    request = (const NOUMEN_NT_CREATE_FILE_REQUEST *)Input;

    if (request->Action > NOUMEN_NTCF_ACTION_READ_LOG) {
        return STATUS_INVALID_PARAMETER;
    }

    response = (PNOUMEN_NT_CREATE_FILE_RESPONSE)Output;

    capacity = 0;

    if (request->Action == NOUMEN_NTCF_ACTION_READ_LOG) {
        capacity = (OutputLength - sizeof(*response)) /
            sizeof(NOUMEN_NT_CREATE_FILE_LOG);
    }

    maxRecords = capacity;

    if (request->MaxRecords != 0 && request->MaxRecords < maxRecords) {
        maxRecords = request->MaxRecords;
    }

    status = NoumenHypervisorHookNtCreateFile(
        request->Action,
        request->Processor,
        maxRecords,
        response,
        (PNOUMEN_NT_CREATE_FILE_LOG)(response + 1),
        &count);

    if (!NT_SUCCESS(status)) {
        return status;
    }

    *Written = sizeof(*response) + count * sizeof(NOUMEN_NT_CREATE_FILE_LOG);

    return STATUS_SUCCESS;
}
