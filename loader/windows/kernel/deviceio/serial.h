#pragma once

#include <ntddk.h>

#ifndef NOUMEN_ENABLE_SERIAL
#define NOUMEN_ENABLE_SERIAL 0
#endif

VOID
NoumenSerialInitialize(
    VOID
    );

VOID
NoumenSerialReset(
    VOID
    );

VOID
NoumenSerialWriteByte(
    _In_ UCHAR Byte
    );

VOID
NoumenSerialWrite(
    _In_z_ PCSTR Text
    );

VOID
NoumenSerialPrint(
    _In_z_ PCSTR Format,
    ...
    );

BOOLEAN
NoumenSerialLoopbackTest(
    VOID
    );
