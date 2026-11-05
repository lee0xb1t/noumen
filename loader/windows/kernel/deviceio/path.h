#pragma once

#include "public.h"

NTSTATUS
NoumenBuildNtPath(
    _In_reads_bytes_(Length) const WCHAR *Path,
    _In_ ULONG Length,
    _Out_ PUNICODE_STRING NtPath,
    _Out_writes_(BufferChars) PWCHAR Buffer,
    _In_ ULONG BufferChars
    );
