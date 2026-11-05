#pragma once

#include <ntddk.h>

NTSTATUS
NoumenCommInitialize(
    _In_ PDRIVER_OBJECT DriverObject
    );

VOID
NoumenCommCleanup(
    VOID
    );
