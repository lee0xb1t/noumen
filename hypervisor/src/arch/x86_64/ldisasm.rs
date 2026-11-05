//! 极简 x86-64 指令长度解码器。
//!
//! EPT detour 会用一条绝对跳转覆盖目标开头 14 字节。被覆盖的指令必须在
//! **另一个地址**的 trampoline 里原样重新执行，只有当它们都不依赖当前地址时
//! 才是安全的：
//!
//! * RIP 相对操作数仍然指向 hook 页；
//! * 相对跳转仍然指向原始代码。
//!
//! 这两种情况都会被标记出来，让安装者拒绝 hook，而不是悄悄破坏 guest。
//! 无法解码的指令同样拒绝。
//!
//! 解码器只需要对“长度”准确，不需要完整语义。它不是完整反汇编器：
//! VEX/EVEX 编码返回 `None`（其操作数大小规则容易写错，而且开头 14 字节
//! 含 AVX/AVX-512 的函数本来就不适合作为 detour 目标）。
//!
//! 本模块不依赖 crate，可以单独编译和测试。

/// x86 指令的架构最大长度。
pub const MAX_INSTRUCTION_LEN: usize = 15;

/// 一条指令的解码结果。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct DecodedInstruction {
    /// 指令长度（字节），1..=15。
    pub len: u8,
    /// 至少有一个操作数是 RIP 相对（`mod=00, r/m=101`）。
    pub rip_relative: bool,
    /// 该指令是相对跳转（rel8/rel32 的 `call`/`jmp`/`jcc`）。
    pub relative_control_flow: bool,
}

impl DecodedInstruction {
    /// 该指令复制到别处后语义不变。
    pub const fn is_relocatable(&self) -> bool {
        !self.rip_relative && !self.relative_control_flow
    }
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Immediate {
    None,
    U8,
    U16,
    /// 操作数大小立即数：有 `0x66` 时 2 字节，否则 4 字节。
    Z,
    /// `B8..=BF mov r, imm`：REX.W 时 8 字节，否则 2/4 字节。
    Mov,
    /// `A0..=A3 moffs`：默认 8 字节地址，32 位地址大小时 4 字节。
    Moffs,
    /// `C8 enter`：imm16 后跟 imm8。
    Enter,
    Rel8,
    Rel32,
    /// `F6 /0`、`F6 /1 test`：imm8；该组其它成员没有立即数。
    Test8,
    /// `F7 /0`、`F7 /1 test`：操作数大小立即数。
    TestZ,
}

/// 解码 `bytes` 开头的一条指令。
///
/// 对每条会被接受的指令，`bytes` 至少要有 [`MAX_INSTRUCTION_LEN`] 个可读字节；
/// 切片更短时解码器返回 `None`，而不是越界读取。
pub fn decode(bytes: &[u8]) -> Option<DecodedInstruction> {
    let mut pos = 0usize;
    let mut operand_size_16 = false;
    let mut address_size_32 = false;
    let mut rex_w = false;

    // 前缀。REX 只有紧挨着 opcode 时才生效，因此它后面的 legacy 前缀会
    // 取消它。
    loop {
        let byte = *bytes.get(pos)?;

        match byte {
            0xF0 | 0xF2 | 0xF3 | 0x2E | 0x36 | 0x3E | 0x26 | 0x64 | 0x65 => {
                rex_w = false;
                pos += 1;
            }
            0x66 => {
                operand_size_16 = true;
                rex_w = false;
                pos += 1;
            }
            0x67 => {
                address_size_32 = true;
                rex_w = false;
                pos += 1;
            }
            0x40..=0x4F => {
                rex_w = byte & 0x08 != 0;
                pos += 1;
            }
            _ => break,
        }

        if pos >= MAX_INSTRUCTION_LEN {
            return None;
        }
    }

    let opcode = *bytes.get(pos)?;
    pos += 1;

    let (map, opcode) = if opcode == 0x0F {
        let second = *bytes.get(pos)?;
        pos += 1;

        match second {
            0x38 => {
                let third = *bytes.get(pos)?;
                pos += 1;
                (2, third)
            }
            0x3A => {
                let third = *bytes.get(pos)?;
                pos += 1;
                (3, third)
            }
            _ => (1, second),
        }
    } else if matches!(opcode, 0xC4 | 0xC5 | 0x62) {
        // VEX / EVEX。
        return None;
    } else {
        (0, opcode)
    };

    let (has_modrm, immediate) = match map {
        0 => one_byte(opcode),
        1 => two_byte(opcode),
        // 0F 38：所有指令都带 ModRM，且都没有立即数。
        2 => (true, Immediate::None),
        // 0F 3A：所有指令都带 ModRM 和 imm8。
        3 => (true, Immediate::U8),
        _ => return None,
    };

    let mut rip_relative = false;
    let mut reg = 0u8;

    if has_modrm {
        let modrm = *bytes.get(pos)?;
        pos += 1;

        reg = (modrm >> 3) & 7;
        let mode = modrm >> 6;
        let rm = modrm & 7;

        if mode != 3 {
            if rm == 4 {
                // SIB 字节。
                let sib = *bytes.get(pos)?;
                pos += 1;

                // mod=00 且 SIB.base=101：disp32，绝对寻址（不是 RIP 相对）。
                if mode == 0 && (sib & 7) == 5 {
                    pos += 4;
                }
            } else if mode == 0 && rm == 5 {
                // 64 位寻址下 mod=00、r/m=101 是 RIP 相对。
                rip_relative = !address_size_32;
                pos += 4;
            }

            match mode {
                1 => pos += 1,
                2 => pos += 4,
                _ => {}
            }
        }
    }

    let immediate_len = match immediate {
        Immediate::None => 0,
        Immediate::U8 => 1,
        Immediate::U16 => 2,
        Immediate::Z => {
            if operand_size_16 {
                2
            } else {
                4
            }
        }
        Immediate::Mov => {
            if rex_w {
                8
            } else if operand_size_16 {
                2
            } else {
                4
            }
        }
        Immediate::Moffs => {
            if address_size_32 {
                4
            } else {
                8
            }
        }
        Immediate::Enter => 3,
        Immediate::Rel8 => 1,
        Immediate::Rel32 => 4,
        Immediate::Test8 => {
            if reg <= 1 {
                1
            } else {
                0
            }
        }
        Immediate::TestZ => {
            if reg <= 1 {
                if operand_size_16 {
                    2
                } else {
                    4
                }
            } else {
                0
            }
        }
    };

    pos += immediate_len;

    if pos > MAX_INSTRUCTION_LEN || pos > bytes.len() {
        return None;
    }

    Some(DecodedInstruction {
        len: pos as u8,
        rip_relative,
        relative_control_flow: matches!(immediate, Immediate::Rel8 | Immediate::Rel32),
    })
}

/// 单字节 opcode map 中某个 opcode 的操作数形态。
fn one_byte(op: u8) -> (bool, Immediate) {
    match op {
        // ALU 组：低三位选择具体形式。
        0x00..=0x05 | 0x08..=0x0D | 0x10..=0x15 | 0x18..=0x1D | 0x20..=0x25
        | 0x28..=0x2D | 0x30..=0x35 | 0x38..=0x3D => match op & 7 {
            0..=3 => (true, Immediate::None),
            4 => (false, Immediate::U8),
            _ => (false, Immediate::Z),
        },

        // 64 位模式下非法；长度仍然是确定的。
        0x06 | 0x07 | 0x0E | 0x16 | 0x17 | 0x1E | 0x1F | 0x27 | 0x2F | 0x37 | 0x3F => {
            (false, Immediate::None)
        }

        0x50..=0x5F => (false, Immediate::None), // push/pop
        0x60 | 0x61 => (false, Immediate::None), // pusha/popa
        0x63 => (true, Immediate::None),         // movsxd
        0x68 => (false, Immediate::Z),           // push imm
        0x69 => (true, Immediate::Z),            // imul r, r/m, imm
        0x6A => (false, Immediate::U8),          // push imm8
        0x6B => (true, Immediate::U8),           // imul r, r/m, imm8
        0x6C..=0x6F => (false, Immediate::None), // ins/outs
        0x70..=0x7F => (false, Immediate::Rel8), // jcc rel8
        0x80 | 0x82 | 0x83 => (true, Immediate::U8),
        0x81 => (true, Immediate::Z),
        0x84..=0x8F => (true, Immediate::None),
        0x90..=0x99 => (false, Immediate::None),
        0x9A => (false, Immediate::None), // far call，非法
        0x9B..=0x9F => (false, Immediate::None),
        0xA0..=0xA3 => (false, Immediate::Moffs),
        0xA4..=0xA7 => (false, Immediate::None),
        0xA8 => (false, Immediate::U8),
        0xA9 => (false, Immediate::Z),
        0xAA..=0xAF => (false, Immediate::None),
        0xB0..=0xB7 => (false, Immediate::U8),
        0xB8..=0xBF => (false, Immediate::Mov),
        0xC0 | 0xC1 => (true, Immediate::U8),
        0xC2 => (false, Immediate::U16), // retn imm16
        0xC3 => (false, Immediate::None),
        0xC6 => (true, Immediate::U8),
        0xC7 => (true, Immediate::Z),
        0xC8 => (false, Immediate::Enter),
        0xC9 => (false, Immediate::None),
        0xCA => (false, Immediate::U16),
        0xCB | 0xCC | 0xCE | 0xCF => (false, Immediate::None),
        0xCD => (false, Immediate::U8), // int imm8
        0xD0..=0xD3 => (true, Immediate::None),
        0xD4 | 0xD5 => (false, Immediate::U8),
        0xD6 | 0xD7 => (false, Immediate::None),
        0xD8..=0xDF => (true, Immediate::None), // x87
        0xE0..=0xE3 => (false, Immediate::Rel8),
        0xE4..=0xE7 => (false, Immediate::U8), // in/out imm8
        0xE8 | 0xE9 => (false, Immediate::Rel32),
        0xEA => (false, Immediate::None), // far jmp，非法
        0xEB => (false, Immediate::Rel8),
        0xEC..=0xEF => (false, Immediate::None),
        0xF1 | 0xF4 | 0xF5 | 0xF8..=0xFD => (false, Immediate::None),
        0xF6 => (true, Immediate::Test8),
        0xF7 => (true, Immediate::TestZ),
        0xFE | 0xFF => (true, Immediate::None),

        // 0x0F 是 escape，其余值是 legacy 前缀；两者都不会走到这里。
        _ => (false, Immediate::None),
    }
}

/// 两字节（`0F xx`）map 中某个 opcode 的操作数形态。
fn two_byte(op: u8) -> (bool, Immediate) {
    match op {
        // 无 ModRM、无立即数。
        0x04..=0x09 | 0x0A | 0x0B | 0x0C | 0x0E | 0x30..=0x37 | 0x77 | 0xA0
        | 0xA1 | 0xA2 | 0xA8 | 0xA9 | 0xAA | 0xC8..=0xCF => (false, Immediate::None),

        // jcc rel32。
        0x80..=0x8F => (false, Immediate::Rel32),

        // ModRM + imm8。
        0x0F | 0x70..=0x73 | 0xA4 | 0xAC | 0xBA | 0xC2 | 0xC4..=0xC6 => {
            (true, Immediate::U8)
        }

        // 其余都是 ModRM + 无立即数。
        _ => (true, Immediate::None),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn len(bytes: &[u8]) -> usize {
        decode(bytes).expect("instruction should decode").len as usize
    }

    fn decoded(bytes: &[u8]) -> DecodedInstruction {
        decode(bytes).expect("instruction should decode")
    }

    #[test]
    fn common_prologue_instructions() {
        // push rbp
        assert_eq!(len(&[0x55]), 1);
        // mov [rsp+8], rbx
        assert_eq!(len(&[0x48, 0x89, 0x5C, 0x24, 0x08]), 5);
        // mov [rsp+0x10], rsi
        assert_eq!(len(&[0x48, 0x89, 0x74, 0x24, 0x10]), 5);
        // sub rsp, 0x28
        assert_eq!(len(&[0x48, 0x83, 0xEC, 0x28]), 4);
        // sub rsp, 0x1000
        assert_eq!(len(&[0x48, 0x81, 0xEC, 0x00, 0x10, 0x00, 0x00]), 7);
        // and qword [rsp+0x40], 0
        assert_eq!(len(&[0x48, 0x83, 0x64, 0x24, 0x40, 0x00]), 6);
        // mov qword [rsp+0x10], 0
        assert_eq!(len(&[0xC7, 0x44, 0x24, 0x10, 0x00, 0x00, 0x00, 0x00]), 8);
        // lea rax, [rsp+0x20]
        assert_eq!(len(&[0x48, 0x8D, 0x44, 0x24, 0x20]), 5);
        // test ecx, 1
        assert_eq!(len(&[0xF7, 0xC1, 0x01, 0x00, 0x00, 0x00]), 6);
        // test cl, 1
        assert_eq!(len(&[0xF6, 0xC1, 0x01]), 3);
        // imul rax, rcx
        assert_eq!(len(&[0x48, 0x0F, 0xAF, 0xC1]), 4);
        // push r15
        assert_eq!(len(&[0x41, 0x57]), 2);
        // jmp r8
        assert_eq!(len(&[0x41, 0xFF, 0xE0]), 3);
        // ret
        assert_eq!(len(&[0xC3]), 1);
        // endbr64
        assert_eq!(len(&[0xF3, 0x0F, 0x1E, 0xFA]), 4);
        // nop word [rax+rax]
        assert_eq!(len(&[0x66, 0x0F, 0x1F, 0x44, 0x00, 0x00]), 6);
        // nop dword [rax+rax]
        assert_eq!(
            len(&[0x0F, 0x1F, 0x84, 0x00, 0x00, 0x00, 0x00, 0x00]),
            8
        );
        // mov rax, imm64
        assert_eq!(
            len(&[0x48, 0xB8, 1, 2, 3, 4, 5, 6, 7, 8]),
            10
        );
        // mov ax, imm16
        assert_eq!(len(&[0x66, 0xB8, 0x34, 0x12]), 4);
        // mov eax, imm32
        assert_eq!(len(&[0xB8, 0x34, 0x12, 0x00, 0x00]), 5);
    }

    #[test]
    fn system_and_rare_instructions() {
        // syscall
        assert_eq!(len(&[0x0F, 0x05]), 2);
        // vmcall (ModRM)
        assert_eq!(len(&[0x0F, 0x01, 0xC1]), 3);
        // int3
        assert_eq!(len(&[0xCC]), 1);
        // int 0x2e
        assert_eq!(len(&[0xCD, 0x2E]), 2);
        // enter 0, 0
        assert_eq!(len(&[0xC8, 0x00, 0x00, 0x00]), 4);
        // moffs，64 位地址
        assert_eq!(len(&[0x48, 0xA1, 1, 2, 3, 4, 5, 6, 7, 8]), 10);
        // 32 位地址大小的 moffs
        assert_eq!(len(&[0x67, 0xA1, 1, 2, 3, 4]), 6);
        // palignr xmm0, xmm1, 8（0F 3A map，imm8）
        assert_eq!(len(&[0x0F, 0x3A, 0x0F, 0xC1, 0x08]), 5);
        // pshufd xmm0, xmm1, 0（0F map + imm8）
        assert_eq!(len(&[0x0F, 0x70, 0xC1, 0x00]), 4);
    }

    #[test]
    fn rip_relative_is_detected() {
        // mov eax, [rip+0x44332211]
        let insn = decoded(&[0x8B, 0x05, 0x11, 0x22, 0x33, 0x44]);
        assert_eq!(insn.len, 6);
        assert!(insn.rip_relative);
        assert!(!insn.is_relocatable());

        // lea rax, [rip+0x44332211]
        let insn = decoded(&[0x48, 0x8D, 0x05, 0x11, 0x22, 0x33, 0x44]);
        assert_eq!(insn.len, 7);
        assert!(insn.rip_relative);

        // jmp [rip+0]
        let insn = decoded(&[0xFF, 0x25, 0x00, 0x00, 0x00, 0x00]);
        assert_eq!(insn.len, 6);
        assert!(insn.rip_relative);

        // mov rcx, [rax*8+0] 使用 SIB 字节；这是绝对 disp32，
        // 不是 RIP 相对。
        let insn = decoded(&[0x48, 0x8B, 0x0C, 0xC5, 0x00, 0x00, 0x00, 0x00]);
        assert_eq!(insn.len, 8);
        assert!(!insn.rip_relative);

        // 32 位地址大小时，mod=00、r/m=5 是绝对 disp32。
        let insn = decoded(&[0x67, 0x8B, 0x05, 0x00, 0x00, 0x00, 0x00]);
        assert_eq!(insn.len, 7);
        assert!(!insn.rip_relative);
    }

    #[test]
    fn relative_branches_are_detected() {
        for bytes in [
            &[0xE8u8, 0, 0, 0, 0][..],       // call rel32
            &[0xE9, 0, 0, 0, 0][..],         // jmp rel32
            &[0xEB, 0xFE][..],               // jmp rel8
            &[0x74, 0x05][..],               // je rel8
            &[0x0F, 0x84, 0, 0, 0, 0][..],   // je rel32
            &[0xE0, 0x02][..],               // loopne rel8
        ] {
            let insn = decoded(bytes);
            assert!(insn.relative_control_flow, "{bytes:02x?}");
            assert!(!insn.is_relocatable(), "{bytes:02x?}");
        }

        // `jmp r8` 是间接跳转，因此可以搬移。
        let insn = decoded(&[0x41, 0xFF, 0xE0]);
        assert!(!insn.relative_control_flow);
        assert!(insn.is_relocatable());
    }

    #[test]
    fn unsupported_encodings_are_rejected() {
        assert!(decode(&[0xC4, 0xE2, 0x79, 0x18, 0xC0]).is_none()); // VEX3
        assert!(decode(&[0xC5, 0xF8, 0x58, 0xC0]).is_none()); // VEX2
        assert!(decode(&[0x62, 0xF1, 0x7C, 0x48, 0x58, 0xC0]).is_none()); // EVEX

        // 15 个前缀永远不构成一条指令。
        assert!(decode(&[0x66; 15]).is_none());

        // 截断的输入被拒绝，而不是越界读取。
        assert!(decode(&[0x48, 0x8B, 0x05, 0x00]).is_none());
    }

    #[test]
    fn decoding_a_prologue_reaches_the_patch_size() {
        // 典型的 MSVC x64 prologue：mov [rsp+8], rbx; mov [rsp+0x10], rsi;
        // push rdi; sub rsp, 0x20。
        let bytes = [
            0x48, 0x89, 0x5C, 0x24, 0x08, // 5
            0x48, 0x89, 0x74, 0x24, 0x10, // 5
            0x57, // 1
            0x48, 0x83, 0xEC, 0x20, // 4
        ];

        let mut total = 0usize;
        let mut count = 0usize;

        while total < 14 {
            let insn = decode(&bytes[total..]).expect("prologue decodes");
            assert!(insn.is_relocatable());
            total += insn.len as usize;
            count += 1;
        }

        assert_eq!(count, 4);
        assert_eq!(total, 15);
    }
}
