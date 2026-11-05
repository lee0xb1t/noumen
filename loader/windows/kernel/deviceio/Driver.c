#include "driver.h"

NTSTATUS
DriverEntry(
    _In_ PDRIVER_OBJECT DriverObject,
    _In_ PUNICODE_STRING RegistryPath
    )
{
    NTSTATUS status;

    UNREFERENCED_PARAMETER(RegistryPath);

    NoumenHypervisorInitialize();

    status = NoumenLogInitialize();

    if (!NT_SUCCESS(status)) {
        return status;
    }

    NoumenSerialInitialize();

    status = NoumenCommInitialize(DriverObject);

    if (!NT_SUCCESS(status)) {
        NoumenLogFree();
        return status;
    }

    DriverObject->DriverUnload = NoumenEvtDriverUnload;

    return STATUS_SUCCESS;
}

VOID
NoumenEvtDriverUnload(
    _In_ PDRIVER_OBJECT DriverObject
    )
{
    UNREFERENCED_PARAMETER(DriverObject);

    NoumenCommCleanup();
    NoumenHypervisorCleanup();
}
