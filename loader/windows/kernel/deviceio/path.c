#include "path.h"

NTSTATUS
NoumenBuildNtPath(
    _In_reads_bytes_(Length) const WCHAR *Path,
    _In_ ULONG Length,
    _Out_ PUNICODE_STRING NtPath,
    _Out_writes_(BufferChars) PWCHAR Buffer,
    _In_ ULONG BufferChars
    )
{
    ULONG chars;
    ULONG totalChars;
    ULONG prefixChars;
    ULONG i;
    BOOLEAN ntPrefix;
    BOOLEAN devicePrefix;

    if (Path == NULL || NtPath == NULL || Buffer == NULL) {
        return STATUS_INVALID_PARAMETER;
    }

    if (Length == 0 || (Length % sizeof(WCHAR)) != 0) {
        return STATUS_INVALID_PARAMETER;
    }

    chars = Length / sizeof(WCHAR);

    if (chars > NOUMEN_MAX_PATH) {
        return STATUS_INVALID_PARAMETER;
    }

    for (i = 0; i < chars; i++) {
        if (Path[i] == L'\0') {
            return STATUS_INVALID_PARAMETER;
        }
    }

    ntPrefix = (BOOLEAN)(chars >= 4 &&
        Path[0] == L'\\' && Path[1] == L'?' && Path[2] == L'?' && Path[3] == L'\\');

    devicePrefix = (BOOLEAN)(chars >= 8 &&
        Path[0] == L'\\' && Path[1] == L'D' && Path[2] == L'e' && Path[3] == L'v' &&
        Path[4] == L'i' && Path[5] == L'c' && Path[6] == L'e' && Path[7] == L'\\');

    if (ntPrefix || devicePrefix) {
        prefixChars = 0;
    } else {
        if (chars < 3 ||
            !((Path[0] >= L'A' && Path[0] <= L'Z') ||
              (Path[0] >= L'a' && Path[0] <= L'z')) ||
            Path[1] != L':' ||
            (Path[2] != L'\\' && Path[2] != L'/')) {
            return STATUS_INVALID_PARAMETER;
        }

        prefixChars = 4;
    }

    totalChars = prefixChars + chars;

    if (totalChars >= BufferChars) {
        return STATUS_BUFFER_OVERFLOW;
    }

    if (prefixChars != 0) {
        Buffer[0] = L'\\';
        Buffer[1] = L'?';
        Buffer[2] = L'?';
        Buffer[3] = L'\\';
    }

    for (i = 0; i < chars; i++) {
        Buffer[prefixChars + i] = (Path[i] == L'/') ? L'\\' : Path[i];
    }

    Buffer[totalChars] = L'\0';

    NtPath->Buffer = Buffer;
    NtPath->Length = (USHORT)(totalChars * sizeof(WCHAR));
    NtPath->MaximumLength = (USHORT)((totalChars + 1) * sizeof(WCHAR));

    return STATUS_SUCCESS;
}
