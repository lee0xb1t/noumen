#include "driver.h"
#include <stdarg.h>

#if NOUMEN_ENABLE_SERIAL

#define NOUMEN_COM1_BASE          0x3F8
#define NOUMEN_UART_SPIN_LIMIT    100000

static
UCHAR
NoumenSerialRead(
    _In_ UINT32 Offset
    )
{
    return __inbyte((USHORT)(NOUMEN_COM1_BASE + Offset));
}

static
VOID
NoumenSerialWriteRegister(
    _In_ UINT32 Offset,
    _In_ UCHAR Value
    )
{
    __outbyte((USHORT)(NOUMEN_COM1_BASE + Offset), Value);
}

VOID
NoumenSerialReset(
    VOID
    )
{
    ULONG spin;

    NoumenSerialWriteRegister(1, 0x00);
    NoumenSerialWriteRegister(3, 0x80);
    NoumenSerialWriteRegister(0, 0x01);
    NoumenSerialWriteRegister(1, 0x00);
    NoumenSerialWriteRegister(3, 0x03);
    NoumenSerialWriteRegister(2, 0xC7);
    NoumenSerialWriteRegister(4, 0x0B);

    (VOID)NoumenSerialRead(5);
    (VOID)NoumenSerialRead(6);

    spin = 0;
    while ((NoumenSerialRead(5) & 0x01) != 0 && spin < NOUMEN_UART_SPIN_LIMIT) {
        (VOID)NoumenSerialRead(0);
        spin++;
    }
}

VOID
NoumenSerialWriteByte(
    _In_ UCHAR Byte
    )
{
    ULONG spin;

    spin = NOUMEN_UART_SPIN_LIMIT;
    while ((NoumenSerialRead(5) & 0x20) == 0 && spin != 0) {
        spin--;
    }

    if (spin != 0) {
        NoumenSerialWriteRegister(0, Byte);
    }
}

VOID
NoumenSerialWrite(
    _In_z_ PCSTR Text
    )
{
    if (Text == NULL) {
        return;
    }

    while (*Text != '\0') {
        NoumenSerialWriteByte((UCHAR)*Text);
        Text++;
    }
}

VOID
NoumenSerialPrint(
    _In_z_ PCSTR Format,
    ...
    )
{
    char buffer[256];
    va_list arguments;

    if (Format == NULL) {
        return;
    }

    va_start(arguments, Format);

    if (NT_SUCCESS(RtlStringCbVPrintfA(buffer, sizeof(buffer), Format, arguments))) {
        NoumenSerialWrite(buffer);
    }

    va_end(arguments);
}

BOOLEAN
NoumenSerialLoopbackTest(
    VOID
    )
{
    ULONG spin;
    UCHAR data;

    NoumenSerialWriteRegister(4, 0x1E);
    NoumenSerialWriteRegister(0, 0xAE);

    spin = NOUMEN_UART_SPIN_LIMIT;
    while ((NoumenSerialRead(5) & 0x01) == 0 && spin != 0) {
        spin--;
    }

    if (spin == 0) {
        NoumenSerialWriteRegister(4, 0x0B);
        return FALSE;
    }

    data = NoumenSerialRead(0);
    NoumenSerialWriteRegister(4, 0x0B);

    return data == 0xAE;
}

VOID
NoumenSerialInitialize(
    VOID
    )
{
    NoumenSerialReset();
}

#else

VOID
NoumenSerialInitialize(
    VOID
    )
{
}

VOID
NoumenSerialReset(
    VOID
    )
{
}

VOID
NoumenSerialWriteByte(
    _In_ UCHAR Byte
    )
{
    UNREFERENCED_PARAMETER(Byte);
}

VOID
NoumenSerialWrite(
    _In_z_ PCSTR Text
    )
{
    UNREFERENCED_PARAMETER(Text);
}

VOID
NoumenSerialPrint(
    _In_z_ PCSTR Format,
    ...
    )
{
    UNREFERENCED_PARAMETER(Format);
}

BOOLEAN
NoumenSerialLoopbackTest(
    VOID
    )
{
    return FALSE;
}

#endif
