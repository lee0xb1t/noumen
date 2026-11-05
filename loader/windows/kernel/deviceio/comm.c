#include "driver.h"

static PDEVICE_OBJECT g_CommDeviceObject;
static BOOLEAN g_CommSymbolicLink;

static
NTSTATUS
NTAPI
NoumenEvtCreateClose(
    _In_ PDEVICE_OBJECT DeviceObject,
    _Inout_ PIRP Irp
    )
{
    UNREFERENCED_PARAMETER(DeviceObject);

    Irp->IoStatus.Status = STATUS_SUCCESS;
    Irp->IoStatus.Information = 0;

    IoCompleteRequest(Irp, IO_NO_INCREMENT);

    return STATUS_SUCCESS;
}

static
NTSTATUS
NTAPI
NoumenEvtDeviceControl(
    _In_ PDEVICE_OBJECT DeviceObject,
    _Inout_ PIRP Irp
    )
{
    PIO_STACK_LOCATION stack;
    PNOUMEN_REQUEST_HEADER request;
    PNOUMEN_RESPONSE_HEADER response;
    PVOID buffer;
    const UCHAR *inputPayload;
    UCHAR *outputPayload;
    ULONG inputLength;
    ULONG outputLength;
    ULONG outputCapacity;
    ULONG ioControlCode;
    ULONG command;
    ULONG written;
    ULONG information;
    ULONG responseSize;
    NTSTATUS handlerStatus;
    NTSTATUS status;

    UNREFERENCED_PARAMETER(DeviceObject);

    stack = IoGetCurrentIrpStackLocation(Irp);
    buffer = Irp->AssociatedIrp.SystemBuffer;
    inputLength = stack->Parameters.DeviceIoControl.InputBufferLength;
    outputLength = stack->Parameters.DeviceIoControl.OutputBufferLength;
    ioControlCode = stack->Parameters.DeviceIoControl.IoControlCode;

    response = NULL;
    information = 0;
    written = 0;
    status = STATUS_INVALID_DEVICE_REQUEST;

    if (ioControlCode != NOUMEN_IOCTL_EXECUTE) {
        goto Complete;
    }

    if (buffer == NULL || inputLength < sizeof(NOUMEN_REQUEST_HEADER)) {
        status = STATUS_INVALID_PARAMETER;
        goto Complete;
    }

    request = (PNOUMEN_REQUEST_HEADER)buffer;

    if (request->ProtocolVersion != NOUMEN_PROTOCOL_VERSION) {
        status = STATUS_REVISION_MISMATCH;
        goto Complete;
    }

    if (request->InputLength > inputLength - sizeof(NOUMEN_REQUEST_HEADER)) {
        status = STATUS_INVALID_PARAMETER;
        goto Complete;
    }

    if (outputLength < sizeof(NOUMEN_RESPONSE_HEADER)) {
        status = STATUS_BUFFER_TOO_SMALL;
        goto Complete;
    }

    outputCapacity = outputLength - sizeof(NOUMEN_RESPONSE_HEADER);

    if (request->OutputLength != 0 && request->OutputLength < outputCapacity) {
        outputCapacity = request->OutputLength;
    }

    if (outputCapacity > NOUMEN_MAX_IOCTL_OUTPUT) {
        outputCapacity = NOUMEN_MAX_IOCTL_OUTPUT;
    }

    responseSize = sizeof(NOUMEN_RESPONSE_HEADER) + outputCapacity;
    response = ExAllocatePoolWithTag(NonPagedPoolNx, responseSize, NOUMEN_POOL_TAG);

    if (response == NULL) {
        status = STATUS_INSUFFICIENT_RESOURCES;
        goto Complete;
    }

    RtlZeroMemory(response, responseSize);

    response->ProtocolVersion = NOUMEN_PROTOCOL_VERSION;
    response->Command = request->Command;

    command = request->Command;
    inputPayload = (const UCHAR *)buffer + sizeof(NOUMEN_REQUEST_HEADER);
    outputPayload = (UCHAR *)response + sizeof(NOUMEN_RESPONSE_HEADER);

    handlerStatus = NoumenCommandDispatch(
        command,
        inputPayload,
        request->InputLength,
        outputPayload,
        outputCapacity,
        &written);

    if (written > outputCapacity) {
        written = outputCapacity;
    }

    response->Status = handlerStatus;
    response->OutputLength = written;

    RtlCopyMemory(buffer, response, sizeof(NOUMEN_RESPONSE_HEADER) + written);
    information = sizeof(NOUMEN_RESPONSE_HEADER) + written;

    status = STATUS_SUCCESS;

Complete:
    if (response != NULL) {
        ExFreePoolWithTag(response, NOUMEN_POOL_TAG);
    }

    Irp->IoStatus.Status = status;
    Irp->IoStatus.Information = information;

    IoCompleteRequest(Irp, IO_NO_INCREMENT);

    return status;
}

NTSTATUS
NoumenCommInitialize(
    _In_ PDRIVER_OBJECT DriverObject
    )
{
    NTSTATUS status;
    PDEVICE_OBJECT deviceObject;
    UNICODE_STRING deviceName;
    UNICODE_STRING symbolicLink;

    if (DriverObject == NULL) {
        return STATUS_INVALID_PARAMETER;
    }

    RtlInitUnicodeString(&deviceName, NOUMEN_DEVICE_NAME);

    deviceObject = NULL;
    status = IoCreateDevice(
        DriverObject,
        0,
        &deviceName,
        FILE_DEVICE_UNKNOWN,
        FILE_DEVICE_SECURE_OPEN,
        FALSE,
        &deviceObject);

    if (!NT_SUCCESS(status)) {
        return status;
    }

    deviceObject->Flags |= DO_BUFFERED_IO;

    DriverObject->MajorFunction[IRP_MJ_CREATE] = NoumenEvtCreateClose;
    DriverObject->MajorFunction[IRP_MJ_CLOSE] = NoumenEvtCreateClose;
    DriverObject->MajorFunction[IRP_MJ_CLEANUP] = NoumenEvtCreateClose;
    DriverObject->MajorFunction[IRP_MJ_DEVICE_CONTROL] = NoumenEvtDeviceControl;

    RtlInitUnicodeString(&symbolicLink, NOUMEN_SYMBOLIC_LINK_NAME);

    status = IoCreateSymbolicLink(&symbolicLink, &deviceName);

    if (!NT_SUCCESS(status)) {
        IoDeleteDevice(deviceObject);
        return status;
    }

    g_CommDeviceObject = deviceObject;
    g_CommSymbolicLink = TRUE;

    deviceObject->Flags &= ~DO_DEVICE_INITIALIZING;

    return STATUS_SUCCESS;
}

VOID
NoumenCommCleanup(
    VOID
    )
{
    UNICODE_STRING symbolicLink;

    if (g_CommSymbolicLink) {
        RtlInitUnicodeString(&symbolicLink, NOUMEN_SYMBOLIC_LINK_NAME);
        IoDeleteSymbolicLink(&symbolicLink);
        g_CommSymbolicLink = FALSE;
    }

    if (g_CommDeviceObject != NULL) {
        IoDeleteDevice(g_CommDeviceObject);
        g_CommDeviceObject = NULL;
    }
}
