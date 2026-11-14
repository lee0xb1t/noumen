#pragma once

#include <ntddk.h>
#include "public.h"

#ifndef NOUMEN_ENABLE_LOGGER
#define NOUMEN_ENABLE_LOGGER 1
#endif

#define NOUMEN_LOG_CAPACITY         512
#define NOUMEN_LOG_TEXT_LENGTH      128
#define NOUMEN_LOG_RECORD_SIZE      152

#pragma pack(push, 8)

typedef struct _NOUMEN_LOG_RECORD
{
    UINT64 Id;
    UINT64 Tsc;
    UINT32 Aux;
    CHAR Data[NOUMEN_LOG_TEXT_LENGTH];
    UINT32 Reserved;
} NOUMEN_LOG_RECORD, *PNOUMEN_LOG_RECORD;

typedef struct _NOUMEN_LOG_BUFFER
{
    CHAR Signature[16];
    volatile UINT64 Sequence;
    volatile UINT32 MessageStart;
    volatile UINT32 MessageCount;
    volatile UINT64 TotalMessageCount;
    volatile UINT64 DroppedMessageCount;
    NOUMEN_LOG_RECORD Messages[NOUMEN_LOG_CAPACITY];
} NOUMEN_LOG_BUFFER, *PNOUMEN_LOG_BUFFER;

#pragma pack(pop)

#define NOUMEN_LOG_BUFFER_SIZE 77872

C_ASSERT(sizeof(NOUMEN_LOG_RECORD) == NOUMEN_LOG_RECORD_SIZE);
C_ASSERT(sizeof(NOUMEN_LOG_BUFFER) == NOUMEN_LOG_BUFFER_SIZE);

NTSTATUS
NoumenLogInitialize(
    VOID
    );

VOID
NoumenLogFree(
    VOID
    );

PVOID
NoumenLogBuffer(
    VOID
    );

ULONG
NoumenLogBufferSize(
    VOID
    );

BOOLEAN
NoumenLogIsReady(
    VOID
    );

NTSTATUS
NoumenLogSnapshot(
    _Out_writes_bytes_opt_(Capacity * NOUMEN_LOG_RECORD_SIZE) NOUMEN_LOG_RECORD *Records,
    _In_ ULONG Capacity,
    _Out_ PULONG Count,
    _Out_ UINT64 *Total,
    _Out_ UINT64 *Dropped
    );

NTSTATUS
NoumenLogSetPath(
    _In_reads_bytes_(Length) const WCHAR *Path,
    _In_ ULONG Length
    );

NTSTATUS
NoumenLogDumpToFile(
    VOID
    );

NTSTATUS
NoumenLogDumpFailure(
    _In_z_ PCSTR Reason,
    _In_ NTSTATUS Status
    );

NTSTATUS
NoumenLogStart(
    VOID
    );

VOID
NoumenLogStop(
    VOID
    );

VOID
NoumenLogEnablePing(
    _In_ BOOLEAN Enable
    );
