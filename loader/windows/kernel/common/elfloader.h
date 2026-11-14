#pragma once

#include <ntddk.h>
#include "elf.h"

typedef struct _NOUMEN_ELF_IMAGE
{
    PVOID ImageBase;
    // ImageBase 的页对齐原始 pool 指针；ImageBase 可能被向上对齐。
    PVOID ImageRaw;
    ULONG ImageSize;
    PVOID EntryPoint;
    PVOID InitFunction;
    PVOID SymbolTable;
    ULONG SymbolTableSize;
    USHORT SymbolCount;
    PVOID StringTable;
    ULONG StringTableSize;
    // 镜像内尾部追加的“镜像所有页 PFN”数组；位于 ImageBase..ImageBase+ImageSize。
    PVOID PfnTable;
    ULONG PfnCount;

    // 可选的物理区间表（PHYSICAL_MEMORY_RANGE 的 16 字节/条副本）。
    // PhysMapSize == 0 表示没有预留：驱动会把 PhysMapBase 填 0。
    ULONG PhysMapOffset;
    ULONG PhysMapSize;
    ULONG PhysMapCount;

} NOUMEN_ELF_IMAGE, *PNOUMEN_ELF_IMAGE;

NTSTATUS
NoumenElfLoadFromFile(
    _In_ PCUNICODE_STRING Path,
            _In_ ULONG PhysMapSize,
    _Out_ PNOUMEN_ELF_IMAGE Image
    );

NTSTATUS
NoumenElfLoadFromMemory(
    _In_reads_bytes_(Size) const VOID *Data,
    _In_ ULONG Size,
            _In_ ULONG PhysMapSize,
    _Out_ PNOUMEN_ELF_IMAGE Image
    );

VOID
NoumenElfFree(
    _Inout_ PNOUMEN_ELF_IMAGE Image
    );
