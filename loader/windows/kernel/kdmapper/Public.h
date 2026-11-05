#pragma once

#ifdef _KERNEL_MODE
#include <ntddk.h>
#else
#include <windows.h>
#include <winioctl.h>
#endif

#define NOUMEN_PROTOCOL_VERSION 4
#define NOUMEN_POOL_TAG         'NDIS'

#ifndef NOUMEN_PANIC_DETAIL
#define NOUMEN_PANIC_DETAIL     1
#endif

#define NOUMEN_DEVICE_NAME          L"\\Device\\Noumen"
#define NOUMEN_SYMBOLIC_LINK_NAME   L"\\DosDevices\\Noumen"
#define NOUMEN_USER_DEVICE_PATH     L"\\\\.\\Noumen"

#define NOUMEN_DEVICE_TYPE          FILE_DEVICE_UNKNOWN
#define NOUMEN_IOCTL_EXECUTE        CTL_CODE(NOUMEN_DEVICE_TYPE, 0x800, METHOD_BUFFERED, FILE_ANY_ACCESS)

#define NOUMEN_MAX_IOCTL_OUTPUT     (1024UL * 1024UL)
#define NOUMEN_MAX_PATH             260

typedef enum _NOUMEN_STATE
{
    NoumenStateUnloaded = 0,
    NoumenStateLoading = 1,
    NoumenStateRunning = 2,
    NoumenStateFailed = 3
} NOUMEN_STATE;

#pragma pack(push, 8)

typedef struct _NOUMEN_REQUEST_HEADER
{
    ULONG ProtocolVersion;
    ULONG Command;
    ULONG InputLength;
    ULONG OutputLength;
} NOUMEN_REQUEST_HEADER, *PNOUMEN_REQUEST_HEADER;

typedef struct _NOUMEN_RESPONSE_HEADER
{
    ULONG ProtocolVersion;
    ULONG Command;
    NTSTATUS Status;
    ULONG OutputLength;
} NOUMEN_RESPONSE_HEADER, *PNOUMEN_RESPONSE_HEADER;

#pragma pack(pop)
