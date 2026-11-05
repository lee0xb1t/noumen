#include "driver.h"

#define NOUMEN_LOG_DUMP_INTERVAL (-10000000LL)
#define NOUMEN_LOG_SEQUENCE_RETRIES 16
#define NOUMEN_LOG_PATH_CHARS (NOUMEN_MAX_PATH + 8)
#define NOUMEN_LOG_NO_PING_CPU ((ULONG)-1)

static PNOUMEN_LOG_BUFFER g_LogBuffer;
static FAST_MUTEX g_LogLock;
static FAST_MUTEX g_LogPathLock;
static HANDLE g_LogThread;
static KEVENT g_LogThreadExit;
static volatile LONG g_LogThreadStop;
static volatile LONG g_LogPingEnabled = 1;
static const CHAR g_LogSignature[16] = "noumen.hv.log";
static NOUMEN_LOG_RECORD g_LogFileRecords[NOUMEN_LOG_CAPACITY];
static WCHAR g_LogPathBuffer[NOUMEN_LOG_PATH_CHARS];
static UNICODE_STRING g_LogFilePath;
static const WCHAR g_DefaultLogPath[] = L"\\??\\C:\\noumen.log";

static
VOID
NoumenLogCopyPath(
    _Out_ PUNICODE_STRING Path,
    _Out_writes_(NOUMEN_LOG_PATH_CHARS) PWCHAR Buffer
    )
{
    ULONG length;

    ExAcquireFastMutex(&g_LogPathLock);

    length = g_LogFilePath.Length;
    Path->Length = (USHORT)length;
    Path->MaximumLength = (USHORT)(length + sizeof(WCHAR));
    Path->Buffer = Buffer;

    RtlCopyMemory(Buffer, g_LogFilePath.Buffer, length);
    Buffer[length / sizeof(WCHAR)] = L'\0';

    ExReleaseFastMutex(&g_LogPathLock);
}

static
NTSTATUS
NoumenLogWriteFailure(
    _In_z_ PCSTR Reason,
    _In_ NTSTATUS Status
    )
{
    HANDLE file = NULL;
    OBJECT_ATTRIBUTES objectAttributes;
    IO_STATUS_BLOCK ioStatus;
    NTSTATUS openStatus;
    NTSTATUS writeStatus;
    WCHAR pathBuffer[NOUMEN_LOG_PATH_CHARS];
    UNICODE_STRING path;
    char line[256];
    size_t length;

    NoumenLogCopyPath(&path, pathBuffer);

    InitializeObjectAttributes(
        &objectAttributes,
        &path,
        OBJ_CASE_INSENSITIVE | OBJ_KERNEL_HANDLE,
        NULL,
        NULL);

    openStatus = ZwCreateFile(
        &file,
        GENERIC_WRITE | SYNCHRONIZE,
        &objectAttributes,
        &ioStatus,
        NULL,
        FILE_ATTRIBUTE_NORMAL,
        0,
        FILE_OVERWRITE_IF,
        FILE_SYNCHRONOUS_IO_NONALERT,
        NULL,
        0);

    if (!NT_SUCCESS(openStatus)) {
        return openStatus;
    }

    writeStatus = RtlStringCbPrintfA(
        line,
        sizeof(line),
        "Noumen log dump failed: %s (0x%08X)\r\n",
        Reason,
        Status);

    if (NT_SUCCESS(writeStatus)) {
        RtlStringCbLengthA(line, sizeof(line), &length);
        writeStatus = ZwWriteFile(
            file,
            NULL,
            NULL,
            NULL,
            &ioStatus,
            line,
            (ULONG)length,
            NULL,
            NULL);
    }

    ZwClose(file);

    return writeStatus;
}

static
VOID
NoumenLogThread(
    _In_ PVOID Context
    )
{
    LARGE_INTEGER interval;

    UNREFERENCED_PARAMETER(Context);

    while (InterlockedCompareExchange(&g_LogThreadStop, 0, 0) == 0) {
        (VOID)NoumenLogDumpToFile();

        interval.QuadPart = NOUMEN_LOG_DUMP_INTERVAL;
        KeDelayExecutionThread(KernelMode, FALSE, &interval);
    }

    // Tell NoumenLogStop that we are exiting. g_LogThread is a HANDLE, not an
    // object pointer, so it must never be passed to KeWaitForSingleObject
    // (doing so bugchecks with 0xA).
    KeSetEvent(&g_LogThreadExit, IO_NO_INCREMENT, FALSE);

    PsTerminateSystemThread(STATUS_SUCCESS);
}

NTSTATUS
NoumenLogInitialize(
    VOID
    )
{
    if (g_LogBuffer != NULL) {
        return STATUS_SUCCESS;
    }

    RtlZeroMemory(&g_LogPathLock, sizeof(g_LogPathLock));
    ExInitializeFastMutex(&g_LogPathLock);

    RtlZeroMemory(g_LogPathBuffer, sizeof(g_LogPathBuffer));
    RtlCopyMemory(g_LogPathBuffer, g_DefaultLogPath, sizeof(g_DefaultLogPath));

    g_LogFilePath.Buffer = g_LogPathBuffer;
    g_LogFilePath.Length = (USHORT)(sizeof(g_DefaultLogPath) - sizeof(WCHAR));
    g_LogFilePath.MaximumLength = (USHORT)sizeof(g_DefaultLogPath);

    RtlZeroMemory(&g_LogLock, sizeof(g_LogLock));
    ExInitializeFastMutex(&g_LogLock);

    g_LogBuffer = ExAllocatePoolWithTag(
        NonPagedPoolNx,
        sizeof(NOUMEN_LOG_BUFFER),
        NOUMEN_POOL_TAG);

    if (g_LogBuffer == NULL) {
        return STATUS_INSUFFICIENT_RESOURCES;
    }

    RtlZeroMemory(g_LogBuffer, sizeof(NOUMEN_LOG_BUFFER));

    return STATUS_SUCCESS;
}

VOID
NoumenLogFree(
    VOID
    )
{
    NoumenLogStop();

    if (g_LogBuffer != NULL) {
        ExFreePoolWithTag(g_LogBuffer, NOUMEN_POOL_TAG);
        g_LogBuffer = NULL;
    }
}

PVOID
NoumenLogBuffer(
    VOID
    )
{
    return g_LogBuffer;
}

ULONG
NoumenLogBufferSize(
    VOID
    )
{
    return sizeof(NOUMEN_LOG_BUFFER);
}

BOOLEAN
NoumenLogIsReady(
    VOID
    )
{
    if (g_LogBuffer == NULL) {
        return FALSE;
    }

    return RtlCompareMemory(
        (const VOID *)g_LogBuffer->Signature,
        (const VOID *)g_LogSignature,
        sizeof(g_LogSignature)) == sizeof(g_LogSignature);
}

NTSTATUS
NoumenLogSetPath(
    _In_reads_bytes_(Length) const WCHAR *Path,
    _In_ ULONG Length
    )
{
    WCHAR buffer[NOUMEN_LOG_PATH_CHARS];
    UNICODE_STRING ntPath;
    NTSTATUS status;

    if (g_LogBuffer == NULL) {
        return STATUS_DEVICE_NOT_READY;
    }

    status = NoumenBuildNtPath(Path, Length, &ntPath, buffer, NOUMEN_LOG_PATH_CHARS);

    if (!NT_SUCCESS(status)) {
        return status;
    }

    ExAcquireFastMutex(&g_LogPathLock);

    RtlCopyMemory(g_LogPathBuffer, ntPath.Buffer, ntPath.Length + sizeof(WCHAR));
    g_LogFilePath.Length = ntPath.Length;
    g_LogFilePath.MaximumLength = ntPath.MaximumLength;

    ExReleaseFastMutex(&g_LogPathLock);

    return STATUS_SUCCESS;
}

NTSTATUS
NoumenLogSnapshot(
    _Out_writes_bytes_opt_(Capacity * NOUMEN_LOG_RECORD_SIZE) NOUMEN_LOG_RECORD *Records,
    _In_ ULONG Capacity,
    _Out_ PULONG Count,
    _Out_ UINT64 *Total,
    _Out_ UINT64 *Dropped
    )
{
    ULONG attempt;
    ULONG start;
    ULONG count;
    ULONG copyCount;
    ULONG i;
    UINT64 sequence;
    NTSTATUS status;

    if (Count == NULL || Total == NULL || Dropped == NULL) {
        return STATUS_INVALID_PARAMETER;
    }

    *Count = 0;
    *Total = 0;
    *Dropped = 0;

    if (g_LogBuffer == NULL) {
        return STATUS_DEVICE_NOT_READY;
    }

    if (Capacity != 0 && Records == NULL) {
        return STATUS_INVALID_PARAMETER;
    }

    status = STATUS_RETRY;
    ExAcquireFastMutex(&g_LogLock);

    if (!NoumenLogIsReady()) {
        ExReleaseFastMutex(&g_LogLock);
        return STATUS_INVALID_PARAMETER;
    }

    for (attempt = 0; attempt < NOUMEN_LOG_SEQUENCE_RETRIES; attempt++) {
        sequence = g_LogBuffer->Sequence;

        if ((sequence & 1) != 0) {
            YieldProcessor();
            continue;
        }

        KeMemoryBarrier();

        start = g_LogBuffer->MessageStart;
        count = g_LogBuffer->MessageCount;
        *Total = g_LogBuffer->TotalMessageCount;
        *Dropped = g_LogBuffer->DroppedMessageCount;

        if (count > NOUMEN_LOG_CAPACITY) {
            count = NOUMEN_LOG_CAPACITY;
        }

        start %= NOUMEN_LOG_CAPACITY;
        copyCount = count;

        if (copyCount > Capacity) {
            copyCount = Capacity;
        }

        for (i = 0; i < copyCount; i++) {
            RtlCopyMemory(
                &Records[i],
                &g_LogBuffer->Messages[(start + i) % NOUMEN_LOG_CAPACITY],
                sizeof(NOUMEN_LOG_RECORD));
        }

        KeMemoryBarrier();

        if (g_LogBuffer->Sequence == sequence) {
            *Count = copyCount;
            status = STATUS_SUCCESS;
            break;
        }

        YieldProcessor();
    }

    ExReleaseFastMutex(&g_LogLock);

    return status;
}

/// Processor that can run the guest, or [`NOUMEN_LOG_NO_PING_CPU`] when the
/// caller must not issue a VMCALL at all.
///
/// `VMCALL` only produces a VM exit in VMX non-root operation. Single-core
/// mode virtualizes one processor (`StartProcessor`), so a dump thread that
/// happens to run on any other processor executes `VMCALL` outside VMX
/// operation: it raises `#UD` (the processor never ran `VMXON`) or, on a
/// processor that is in VMX root operation, fails with `VMfail` and leaves
/// `RAX` untouched. Both outcomes make the ping look "failed" although the
/// log ring is fine.
static
ULONG
NoumenLogPingProcessor(
    VOID
    )
{
    ULONG cpu;
    ULONG cpuCount;

    cpuCount = g_NoumenHypervisor.CpuCount;

    if (cpuCount == 0) {
        return NOUMEN_LOG_NO_PING_CPU;
    }

    // Single-core mode: only the selected processor ever executes the guest,
    // no matter how many processors are active.
    if (g_NoumenHypervisor.SingleCore) {
        return g_NoumenHypervisor.StartProcessor;
    }

    // Multi-core mode: every effective processor runs `_init`.
    cpu = KeGetCurrentProcessorNumber();

    return cpu < cpuCount ? cpu : NOUMEN_LOG_NO_PING_CPU;
}

/// Non-fatal liveness probe.
///
/// The caller is pinned to `Processor` for the duration of the VMCALL so that
/// a migration between the CPU check and the instruction cannot turn the probe
/// into a fault on a processor without a guest.
static
BOOLEAN
NoumenLogPing(
    _In_ ULONG Processor
    )
{
    KAFFINITY previousAffinity;
    UINT64 pong;

    previousAffinity = KeSetSystemAffinityThreadEx((KAFFINITY)(1ULL << Processor));

    pong = NoumenHypercallPing(g_NoumenHypervisor.HypercallKey, 0x1234);

    KeRevertToUserAffinityThreadEx(previousAffinity);

    return pong == ((UINT64)NOUMEN_HYPERCALL_PONG_MAGIC ^ 0x1234);
}

NTSTATUS
NoumenLogDumpToFile(
    VOID
    )
{
    HANDLE file = NULL;
    OBJECT_ATTRIBUTES objectAttributes;
    IO_STATUS_BLOCK ioStatus;
    NTSTATUS status;
    ULONG pingProcessor;
    PCSTR pingState;
    ULONG cpu;
    UINT64 total;
    UINT64 dropped;
    ULONG count;
    ULONG i;
    WCHAR pathBuffer[NOUMEN_LOG_PATH_CHARS];
    UNICODE_STRING path;
    char line[256];
    char text[NOUMEN_LOG_TEXT_LENGTH];
    size_t length;

    if (g_NoumenHypervisor.State != NoumenStateRunning) {
        return STATUS_DEVICE_NOT_READY;
    }

    if (!NoumenLogIsReady()) {
        return NoumenLogWriteFailure("log buffer not initialized", STATUS_INVALID_PARAMETER);
    }

    // The ring lives in this driver's nonpaged pool and is read directly, so
    // the dump never depends on the hypervisor answering a hypercall. The ping
    // is only recorded in the header: on a processor that does not run the
    // guest it cannot work, and letting it abort the dump hid the whole log
    // behind "ping failed (0xC0000001)".
    cpu = KeGetCurrentProcessorNumber();
    pingProcessor = NoumenLogPingProcessor();

    if (pingProcessor == NOUMEN_LOG_NO_PING_CPU) {
        pingState = "skipped";
    } else if (InterlockedCompareExchange(&g_LogPingEnabled, 1, 1) == 0) {
        pingState = "disabled";
    } else if (NoumenLogPing(pingProcessor)) {
        pingState = "ok";
    } else {
        pingState = "failed";
    }

    status = NoumenLogSnapshot(
        g_LogFileRecords,
        NOUMEN_LOG_CAPACITY,
        &count,
        &total,
        &dropped);

    if (!NT_SUCCESS(status)) {
        return NoumenLogWriteFailure("snapshot failed (writer busy)", status);
    }

    NoumenLogCopyPath(&path, pathBuffer);

    InitializeObjectAttributes(
        &objectAttributes,
        &path,
        OBJ_CASE_INSENSITIVE | OBJ_KERNEL_HANDLE,
        NULL,
        NULL);

    status = ZwCreateFile(
        &file,
        GENERIC_WRITE | SYNCHRONIZE,
        &objectAttributes,
        &ioStatus,
        NULL,
        FILE_ATTRIBUTE_NORMAL,
        0,
        FILE_OVERWRITE_IF,
        FILE_SYNCHRONOUS_IO_NONALERT,
        NULL,
        0);

    if (!NT_SUCCESS(status)) {
        return status;
    }

    status = RtlStringCbPrintfA(
        line,
        sizeof(line),
        "Noumen log dump: %u record(s), total=%I64u, dropped=%I64u, cpu=%u, ping=%s\r\n",
        count,
        total,
        dropped,
        cpu,
        pingState);

    if (NT_SUCCESS(status)) {
        RtlStringCbLengthA(line, sizeof(line), &length);
        status = ZwWriteFile(
            file,
            NULL,
            NULL,
            NULL,
            &ioStatus,
            line,
            (ULONG)length,
            NULL,
            NULL);
    }

    for (i = 0; NT_SUCCESS(status) && i < count; i++) {
        RtlZeroMemory(text, sizeof(text));
        RtlCopyMemory(text, g_LogFileRecords[i].Data, NOUMEN_LOG_TEXT_LENGTH);
        text[NOUMEN_LOG_TEXT_LENGTH - 1] = '\0';

        status = RtlStringCbPrintfA(
            line,
            sizeof(line),
            "[%I64u] tsc=%I64u aux=%u %s\r\n",
            g_LogFileRecords[i].Id,
            g_LogFileRecords[i].Tsc,
            g_LogFileRecords[i].Aux,
            text);

        if (!NT_SUCCESS(status)) {
            break;
        }

        RtlStringCbLengthA(line, sizeof(line), &length);
        status = ZwWriteFile(
            file,
            NULL,
            NULL,
            NULL,
            &ioStatus,
            line,
            (ULONG)length,
            NULL,
            NULL);
    }

    ZwClose(file);

    return status;
}

NTSTATUS
NoumenLogDumpFailure(
    _In_z_ PCSTR Reason,
    _In_ NTSTATUS Status
    )
{
    return NoumenLogWriteFailure(Reason, Status);
}

NTSTATUS
NoumenLogStart(
    VOID
    )
{
    NTSTATUS status;

    if (g_LogThread != NULL) {
        return STATUS_SUCCESS;
    }

    InterlockedExchange(&g_LogThreadStop, 0);

    // Reset the exit event: it is set by the thread on its way out.
    KeInitializeEvent(&g_LogThreadExit, NotificationEvent, FALSE);

    status = PsCreateSystemThread(
        &g_LogThread,
        THREAD_ALL_ACCESS,
        NULL,
        NULL,
        NULL,
        NoumenLogThread,
        NULL);

    if (!NT_SUCCESS(status)) {
        g_LogThread = NULL;
    }

    return status;
}

VOID
NoumenLogEnablePing(
    _In_ BOOLEAN Enable
    )
{
    InterlockedExchange(&g_LogPingEnabled, Enable ? 1 : 0);
}

VOID
NoumenLogStop(
    VOID
    )
{
    InterlockedExchange(&g_LogThreadStop, 1);

    if (g_LogThread == NULL) {
        return;
    }

    // Wait for the thread to signal that it is gone (2s cap). Never wait on
    // g_LogThread: it is a HANDLE returned by PsCreateSystemThread, and using
    // it as an object pointer bugchecks with 0xA.
    {
        LARGE_INTEGER timeout;

        timeout.QuadPart = -2 * 1000 * 1000 * 10;
        (VOID)KeWaitForSingleObject(
            &g_LogThreadExit,
            Executive,
            KernelMode,
            FALSE,
            &timeout);
    }

    ZwClose(g_LogThread);
    g_LogThread = NULL;
}
