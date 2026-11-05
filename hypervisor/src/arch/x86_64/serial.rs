//! COM1 (0x3F8) polling serial console.
//!
//! The hardware implementation lives behind the `serial-console` feature. With
//! the feature disabled every function becomes a no-op with the same
//! signature, so callers do not need any `cfg`s; `serial_init` reports success
//! because boot must not fail just because the console was compiled out.

#[cfg(feature = "serial-console")]
mod imp {
    use core::fmt::Write;
    use x86::io::{inb, outb};

    const PORT: u16 = 0x3F8; // COM1

    pub fn serial_init() -> bool {
        unsafe {
            // Disable all interrupts (IER = 0)
            outb(PORT + 1, 0x00);

            // Enable DLAB to set baud rate divisor
            outb(PORT + 3, 0x80);

            // Set divisor to 1 (115200 baud)
            outb(PORT + 0, 0x01);
            outb(PORT + 1, 0x00);

            // Clear DLAB, set 8N1 (8 data bits, no parity, 1 stop bit)
            outb(PORT + 3, 0x03);

            // Enable FIFO, clear buffers, set interrupt trigger level
            outb(PORT + 2, 0xC7);

            // Set DTR and RTS, keep IRQs disabled (MCR = 0x0B)
            outb(PORT + 4, 0x0B);

            // Enter loopback mode to test the serial chip
            outb(PORT + 4, 0x1E);
            outb(PORT + 0, 0xAE); // Send test byte

            // Wait RX ready
            let mut timeout = 100000;
            while timeout > 0 {
                if inb(PORT + 5) & 0x01 == 0x01 {
                    break;
                }
                timeout -= 1;
            }

            // Check if the echoed byte matches
            if inb(PORT + 0) != 0xAE {
                return false; // Serial chip is faulty
            }

            // Exit loopback mode, keep IRQs disabled (MCR = 0x0B)
            outb(PORT + 4, 0x0B);

            true
        }
    }

    pub fn serial_reset() {
        unsafe {
            // Disable all interrupts (IER = 0)
            outb(PORT + 1, 0x00);

            // Enable DLAB to set baud rate divisor
            outb(PORT + 3, 0x80);

            // Set divisor to 1 (115200 baud)
            outb(PORT + 0, 0x01);
            outb(PORT + 1, 0x00);

            // Clear DLAB, set 8N1 (8 data bits, no parity, 1 stop bit)
            outb(PORT + 3, 0x03);

            // Enable FIFO, clear buffers, set interrupt trigger level
            outb(PORT + 2, 0xC7);

            // Set DTR and RTS, keep IRQs disabled (MCR = 0x0B)
            outb(PORT + 4, 0x0B);

            inb(PORT + 5); // LSR
            inb(PORT + 6); // MSR
            while inb(PORT + 5) & 0x01 == 0x01 {
                inb(PORT + 0); // drain RX
            }
        }
    }

    /// Write a single byte to the serial port (blocking).
    /// This function busy-waits until the transmit FIFO is empty.
    pub fn serial_write_byte(byte: u8) {
        unsafe {
            // Wait for Transmit Holding Register Empty (THRE) – LSR bit 5
            while (inb(PORT + 5) & 0x20) == 0 {
                // Optionally use core::hint::spin_loop() here for better CPU yield
            }
            outb(PORT + 0, byte);
        }
    }

    /// Write a null-terminated string to the serial port.
    /// Each byte is sent using `serial_write_byte`.
    pub fn serial_write_str(s: &str) {
        for &byte in s.as_bytes() {
            serial_write_byte(byte);
        }
    }

    pub struct SerialWriter;

    impl Write for SerialWriter {
        fn write_str(&mut self, s: &str) -> core::fmt::Result {
            serial_write_str(s);
            Ok(())
        }
    }
}

#[cfg(not(feature = "serial-console"))]
mod imp {
    // Keep the API surface identical to the serial-console build.
    #![allow(dead_code)]

    /// No console is compiled in; report success so the boot path continues.
    pub fn serial_init() -> bool {
        true
    }

    pub fn serial_reset() {}

    pub fn serial_write_byte(_byte: u8) {}

    pub fn serial_write_str(_s: &str) {}

    pub struct SerialWriter;

    impl core::fmt::Write for SerialWriter {
        fn write_str(&mut self, _s: &str) -> core::fmt::Result {
            Ok(())
        }
    }
}

pub use imp::*;
