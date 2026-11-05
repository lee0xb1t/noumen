use crate::arch::x86_64::ldisasm;

pub const PAGE_SIZE: usize = 0x1000;

#[cfg(test)]
pub fn build_direct_span(
    primary: &mut [u8; PAGE_SIZE],
    secondary: Option<&mut [u8; PAGE_SIZE]>,
    offset: usize,
    handler_va: u64,
) -> Result<(), DetourError> {
    let mut patch = [0u8; ABSOLUTE_JUMP_LEN];

    // The fake page is execute-only (R=0). `jmp [rip+0]` would read its own
    // operand as data and fault, so use a register-indirect jump instead.
    write_register_jump(&mut patch, handler_va);
    write_span(primary, secondary, offset, &patch)
}

#[cfg(test)]
pub fn build_direct(
    fake_page: &mut [u8; PAGE_SIZE],
    offset: usize,
    handler_va: u64,
) -> Result<(), DetourError> {
    build_direct_span(fake_page, None, offset, handler_va)
}

pub const ABSOLUTE_JUMP_LEN: usize = 14;

pub const TRAMPOLINE_OFFSET: usize = 0x200;

pub const MAX_STOLEN_LEN: usize = 32;

const _: () = assert!(
    TRAMPOLINE_OFFSET + REPORT_LEN + MAX_STOLEN_LEN + ABSOLUTE_JUMP_LEN <= PAGE_SIZE
);

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum DetourError {
    PatchOutOfRange,
    TooLong,
    UnsupportedInstruction,
    HypercallsDisabled,
    InvalidTarget,
}

pub fn needs_second_page(bytes: &[u8]) -> bool {
    if bytes.len() < ABSOLUTE_JUMP_LEN {
        return true;
    }

    let mut stolen = 0usize;

    while stolen < ABSOLUTE_JUMP_LEN {
        if stolen >= MAX_STOLEN_LEN || stolen >= bytes.len() {
            return true;
        }

        let Some(instruction) = ldisasm::decode(&bytes[stolen..]) else {
            return true;
        };

        if !instruction.is_relocatable() {
            return false;
        }

        stolen += instruction.len as usize;
    }

    false
}

#[cfg(test)]
pub fn build(
    fake_page: &mut [u8; PAGE_SIZE],
    offset: usize,
    target_va: u64,
    trampoline_va: u64,
    key: u64,
    slot: u64,
    trampoline_page: &mut [u8; PAGE_SIZE],
) -> Result<usize, DetourError> {
    build_span(
        fake_page,
        None,
        offset,
        target_va,
        trampoline_va,
        key,
        slot,
        trampoline_page,
    )
}

pub fn build_span(
    primary: &mut [u8; PAGE_SIZE],
    secondary: Option<&mut [u8; PAGE_SIZE]>,
    offset: usize,
    target_va: u64,
    trampoline_va: u64,
    key: u64,
    slot: u64,
    trampoline_page: &mut [u8; PAGE_SIZE],
) -> Result<usize, DetourError> {
    if key == 0 {
        return Err(DetourError::HypercallsDisabled);
    }

    let (stolen, buffer) = decode_stolen(primary, secondary.as_deref(), offset)?;

    trampoline_page.fill(0);
    write_report_code(trampoline_page, key, slot);

    let stolen_at = TRAMPOLINE_OFFSET + REPORT_LEN;

    trampoline_page[stolen_at..stolen_at + stolen].copy_from_slice(&buffer[..stolen]);
    write_absolute_jump(
        &mut trampoline_page[stolen_at + stolen..stolen_at + stolen + ABSOLUTE_JUMP_LEN],
        target_va + stolen as u64,
    );

    let mut patch = [0u8; ABSOLUTE_JUMP_LEN];
    write_register_jump(&mut patch, trampoline_va);
    write_span(primary, secondary, offset, &patch)?;

    Ok(stolen)
}

/// Build the **origin** trampoline in a caller-owned page and patch the fake
/// page to jump straight to `proxy_va`.
///
/// The origin is what the driver's proxy `call`s to run the original function:
///
/// ```text
/// origin+0        : the instructions overwritten by the fake-page jump
/// origin+stolen   : jmp [rip+0] -> target_va + stolen
/// ```
///
/// Because the proxy reaches it with a normal `call`, the original function's
/// final `ret` returns to the proxy, which can record the return value and pass
/// it through.
pub fn build_origin_span(
    primary: &mut [u8; PAGE_SIZE],
    secondary: Option<&mut [u8; PAGE_SIZE]>,
    offset: usize,
    target_va: u64,
    proxy_va: u64,
    origin: &mut [u8; PAGE_SIZE],
) -> Result<usize, DetourError> {
    if proxy_va == 0 {
        return Err(DetourError::InvalidTarget);
    }

    let (stolen, buffer) = decode_stolen(primary, secondary.as_deref(), offset)?;

    origin.fill(0);
    origin[..stolen].copy_from_slice(&buffer[..stolen]);
    // The origin page is hidden (data reads see zero, execution sees the real
    // page). `jmp [rip+0]` would read its operand as data and fault, so use the
    // register form here as well.
    write_register_jump(
        &mut origin[stolen..stolen + ABSOLUTE_JUMP_LEN],
        target_va + stolen as u64,
    );

    let mut patch = [0u8; ABSOLUTE_JUMP_LEN];
    write_register_jump(&mut patch, proxy_va);
    write_span(primary, secondary, offset, &patch)?;

    Ok(stolen)
}

/// Decode whole instructions until at least one absolute jump is covered.
///
/// The bytes are read across the page boundary when `secondary` is present.
fn decode_stolen(
    primary: &[u8; PAGE_SIZE],
    secondary: Option<&[u8; PAGE_SIZE]>,
    offset: usize,
) -> Result<(usize, [u8; MAX_STOLEN_LEN]), DetourError> {
    let secondary_len = if secondary.is_some() { PAGE_SIZE } else { 0 };

    if offset + ABSOLUTE_JUMP_LEN > PAGE_SIZE + secondary_len {
        return Err(DetourError::PatchOutOfRange);
    }

    let available = (PAGE_SIZE - offset + secondary_len).min(MAX_STOLEN_LEN);

    let mut buffer = [0u8; MAX_STOLEN_LEN];
    read_span(primary, secondary, offset, available, &mut buffer)?;

    let mut stolen = 0usize;

    while stolen < ABSOLUTE_JUMP_LEN {
        if stolen >= available {
            return Err(DetourError::TooLong);
        }

        let instruction = ldisasm::decode(&buffer[stolen..available])
            .ok_or(DetourError::UnsupportedInstruction)?;

        if !instruction.is_relocatable() {
            return Err(DetourError::UnsupportedInstruction);
        }

        let opcode = buffer[stolen];
        let ud2 = opcode == 0x0F
            && buffer
                .get(stolen + 1)
                .is_some_and(|byte| *byte == 0x0B);

        if matches!(opcode, 0xCC | 0xCD | 0xF1) || ud2 {
            return Err(DetourError::UnsupportedInstruction);
        }

        stolen += instruction.len as usize;
    }

    if stolen > MAX_STOLEN_LEN {
        return Err(DetourError::TooLong);
    }

    Ok((stolen, buffer))
}

fn read_span(
    primary: &[u8; PAGE_SIZE],
    secondary: Option<&[u8; PAGE_SIZE]>,
    offset: usize,
    length: usize,
    out: &mut [u8; MAX_STOLEN_LEN],
) -> Result<(), DetourError> {
    let capacity = PAGE_SIZE + if secondary.is_some() { PAGE_SIZE } else { 0 };

    if offset + length > capacity {
        return Err(DetourError::TooLong);
    }

    let first = (PAGE_SIZE - offset).min(length);
    out[..first].copy_from_slice(&primary[offset..offset + first]);

    if first < length {
        let secondary = secondary.ok_or(DetourError::TooLong)?;
        let rest = length - first;
        out[first..length].copy_from_slice(&secondary[..rest]);
    }

    Ok(())
}

fn write_span(
    primary: &mut [u8; PAGE_SIZE],
    secondary: Option<&mut [u8; PAGE_SIZE]>,
    offset: usize,
    bytes: &[u8],
) -> Result<(), DetourError> {
    let capacity = PAGE_SIZE + if secondary.is_some() { PAGE_SIZE } else { 0 };

    if offset + bytes.len() > capacity {
        return Err(DetourError::PatchOutOfRange);
    }

    let first = (PAGE_SIZE - offset).min(bytes.len());
    primary[offset..offset + first].copy_from_slice(&bytes[..first]);

    if first < bytes.len() {
        let secondary = secondary.ok_or(DetourError::PatchOutOfRange)?;
        let rest = bytes.len() - first;
        secondary[..rest].copy_from_slice(&bytes[first..]);
    }

    Ok(())
}

// trampoline layout: report VMCALL (41B) + stolen bytes + 14B jump back.
pub const REPORT_LEN: usize = 41;

fn write_report_code(page: &mut [u8; PAGE_SIZE], key: u64, slot: u64) {
    let mut code = [0u8; REPORT_LEN];
    let mut at = 0usize;

    for byte in [0x50u8, 0x51, 0x52, 0x41, 0x50] {
        code[at] = byte;
        at += 1;
    }

    code[at..at + 3].copy_from_slice(&[0x49, 0x89, 0xC8]);
    at += 3;

    code[at] = 0x48;
    code[at + 1] = 0xBA;
    code[at + 2..at + 10].copy_from_slice(&slot.to_le_bytes());
    at += 10;

    code[at] = 0xB9;
    code[at + 1..at + 5].copy_from_slice(&(0x100u32).to_le_bytes());
    at += 5;

    code[at] = 0x48;
    code[at + 1] = 0xB8;
    code[at + 2..at + 10].copy_from_slice(&key.to_le_bytes());
    at += 10;

    code[at..at + 3].copy_from_slice(&[0x0F, 0x01, 0xC1]);
    at += 3;

    for byte in [0x41u8, 0x58, 0x5A, 0x59, 0x58] {
        code[at] = byte;
        at += 1;
    }

    debug_assert_eq!(at, REPORT_LEN);

    page[TRAMPOLINE_OFFSET..TRAMPOLINE_OFFSET + REPORT_LEN].copy_from_slice(&code);
}

fn write_absolute_jump(destination: &mut [u8], target: u64) {
    debug_assert!(destination.len() >= ABSOLUTE_JUMP_LEN);

    destination[0..6].copy_from_slice(&[0xFF, 0x25, 0x00, 0x00, 0x00, 0x00]);
    destination[6..14].copy_from_slice(&target.to_le_bytes());
}

/// `mov r11, imm64; jmp r11; nop` — an absolute jump with **no memory
/// operand**.
///
/// The fake page and the hidden origin page are mapped execute-only
/// (`R=W=0, X=1`). `FF 25` (`jmp [rip+0]`) reads its 8-byte target from the
/// same page as data, which raises an EPT violation and makes the page switch
/// to the real/data view while the CPU is still decoding the patch — the guest
/// then continues at the wrong offset (the NtCreateFile+7 crash). R11 is
/// volatile at a function-call boundary, so clobbering it here is allowed.
fn write_register_jump(destination: &mut [u8], target: u64) {
    debug_assert!(destination.len() >= ABSOLUTE_JUMP_LEN);

    destination[0] = 0x49;
    destination[1] = 0xBB;
    destination[2..10].copy_from_slice(&target.to_le_bytes());
    destination[10] = 0x41;
    destination[11] = 0xFF;
    destination[12] = 0xE3;
    destination[13] = 0x90;
}

#[cfg(test)]
mod tests {
    use super::*;

    const KEY: u64 = 0x10F2C;
    const SLOT: u64 = 3;
    const TRAMPOLINE_VA: u64 = 0xFFFF_F800_1234_5200;
    const TARGET_VA: u64 = 0xFFFF_F800_4321_0040;

    fn prologue_page() -> [u8; PAGE_SIZE] {
        let mut page = [0x90u8; PAGE_SIZE];

        let prologue = [
            0x48, 0x89, 0x5C, 0x24, 0x08, // 5
            0x48, 0x89, 0x74, 0x24, 0x10, // 5
            0x57, // 1
            0x48, 0x83, 0xEC, 0x20, // 4
        ];

        page[0x40..0x40 + prologue.len()].copy_from_slice(&prologue);
        page
    }

    fn assert_register_jump(bytes: &[u8], target: u64) {
        assert_eq!(&bytes[0..2], &[0x49, 0xBB], "mov r11, imm64");
        assert_eq!(&bytes[2..10], &target.to_le_bytes());
        assert_eq!(&bytes[10..14], &[0x41, 0xFF, 0xE3, 0x90], "jmp r11; nop");
    }

    #[test]
    fn direct_jump_targets_the_user_handler_without_a_stub() {
        let mut page = prologue_page();
        let handler_va = 0xFFFF_F800_DEAD_BEEF;

        build_direct(&mut page, 0x40, handler_va).expect("direct patch fits");

        assert_register_jump(&page[0x40..0x40 + ABSOLUTE_JUMP_LEN], handler_va);
        assert_eq!(page[0x4E], 0x20);
        assert_eq!(page[0x4F], 0x90);
    }

    #[test]
    fn direct_jump_can_cross_a_page_boundary() {
        let mut primary = [0x90u8; PAGE_SIZE];
        let mut secondary = [0x90u8; PAGE_SIZE];
        let handler_va = 0xFFFF_F800_1122_3344;
        let offset = PAGE_SIZE - 8;

        build_direct_span(
            &mut primary,
            Some(&mut secondary),
            offset,
            handler_va,
        )
        .expect("14-byte patch spans two pages");

        let mut bytes = [0u8; ABSOLUTE_JUMP_LEN];
        bytes[..8].copy_from_slice(&primary[offset..]);
        bytes[8..].copy_from_slice(&secondary[..6]);

        assert_register_jump(&bytes, handler_va);
    }

    #[test]
    fn builds_a_detour_for_a_typical_prologue() {
        let mut page = prologue_page();
        let mut stub = [0u8; PAGE_SIZE];

        let stolen = build(
            &mut page, 0x40, TARGET_VA, TRAMPOLINE_VA, KEY, SLOT, &mut stub,
        )
        .expect("prologue is relocatable");

        assert_eq!(stolen, 15);

        // Fake page: register jump to the internal trampoline (no data read).
        assert_register_jump(&page[0x40..0x40 + ABSOLUTE_JUMP_LEN], TRAMPOLINE_VA);
        assert_eq!(page[0x4E], 0x20);
        assert_eq!(page[0x4F], 0x90);
        assert_eq!(page[0x3F], 0x90);

        let report = TRAMPOLINE_OFFSET;
        assert_eq!(&stub[report..report + 5], &[0x50, 0x51, 0x52, 0x41, 0x50]);
        assert_eq!(&stub[report + 5..report + 8], &[0x49, 0x89, 0xC8]);
        assert_eq!(stub[report + 8], 0x48);
        assert_eq!(stub[report + 9], 0xBA);
        assert_eq!(&stub[report + 10..report + 18], &SLOT.to_le_bytes());
        assert_eq!(stub[report + 18], 0xB9);
        assert_eq!(&stub[report + 19..report + 23], &0x100u32.to_le_bytes());
        assert_eq!(stub[report + 23], 0x48);
        assert_eq!(stub[report + 24], 0xB8);
        assert_eq!(&stub[report + 25..report + 33], &KEY.to_le_bytes());
        assert_eq!(&stub[report + 33..report + 36], &[0x0F, 0x01, 0xC1]);
        assert_eq!(&stub[report + 36..report + 41], &[0x41, 0x58, 0x5A, 0x59, 0x58]);

        assert_eq!(stub[0], 0);

        let stolen_at = report + REPORT_LEN;
        assert_eq!(
            &stub[stolen_at..stolen_at + stolen],
            &prologue_page()[0x40..0x40 + stolen]
        );
        assert_eq!(
            &stub[stolen_at + stolen..stolen_at + stolen + 6],
            &[0xFF, 0x25, 0x00, 0x00, 0x00, 0x00]
        );
        assert_eq!(
            &stub[stolen_at + stolen + 6..stolen_at + stolen + 14],
            &(TARGET_VA + stolen as u64).to_le_bytes()
        );
    }

    #[test]
    fn trampoline_flow_is_target_to_report_to_original() {
        let mut page = prologue_page();
        let mut trampoline = [0u8; PAGE_SIZE];

        let stolen = build(
            &mut page,
            0x40,
            TARGET_VA,
            TRAMPOLINE_VA,
            KEY,
            SLOT,
            &mut trampoline,
        )
        .expect("prologue is relocatable");

        assert_register_jump(&page[0x40..0x40 + ABSOLUTE_JUMP_LEN], TRAMPOLINE_VA);

        let report = TRAMPOLINE_OFFSET;
        assert_eq!(
            &trampoline[report + 33..report + 36],
            &[0x0F, 0x01, 0xC1]
        );

        let stolen_at = report + REPORT_LEN;
        assert_eq!(
            &trampoline[stolen_at..stolen_at + stolen],
            &prologue_page()[0x40..0x40 + stolen]
        );

        let back_at = stolen_at + stolen;
        assert_eq!(
            &trampoline[back_at..back_at + 6],
            &[0xFF, 0x25, 0x00, 0x00, 0x00, 0x00]
        );
        assert_eq!(
            u64::from_le_bytes(
                trampoline[back_at + 6..back_at + 14]
                    .try_into()
                    .unwrap()
            ),
            TARGET_VA + stolen as u64
        );
    }

    #[test]
    fn origin_runs_stolen_bytes_then_jumps_back_to_the_target() {
        let mut page = prologue_page();
        let mut origin = [0xCCu8; PAGE_SIZE];
        let proxy_va = 0xFFFF_F800_DEAD_BEEF;

        let stolen = build_origin_span(
            &mut page,
            None,
            0x40,
            TARGET_VA,
            proxy_va,
            &mut origin,
        )
        .expect("prologue is relocatable");

        assert_eq!(stolen, 15);

        // Fake page: register jump straight to the driver proxy (no data read).
        assert_register_jump(&page[0x40..0x40 + ABSOLUTE_JUMP_LEN], proxy_va);
        assert_eq!(page[0x4E], 0x20);

        // Origin: stolen bytes first, then a register jump back to
        // target+stolen (the origin page is data-hidden, so `FF 25` would
        // fault on its own operand read).
        assert_eq!(
            &origin[..stolen],
            &prologue_page()[0x40..0x40 + stolen]
        );
        assert_register_jump(
            &origin[stolen..stolen + ABSOLUTE_JUMP_LEN],
            TARGET_VA + stolen as u64,
        );
        assert_eq!(origin[stolen + 14], 0);
    }

    #[test]
    fn ntcreatefile_prologue_becomes_a_14_byte_origin() {
        // Windows 10 19041 `nt!NtCreateFile` prologue:
        //   sub rsp, 88h            (7)
        //   xor eax, eax            (2)
        //   mov [rsp+78h], rax      (5)
        let prologue = [
            0x48, 0x81, 0xEC, 0x88, 0x00, 0x00, 0x00, // sub rsp,88h
            0x33, 0xC0, // xor eax,eax
            0x48, 0x89, 0x44, 0x24, 0x78, // mov [rsp+78h],rax
        ];
        let mut page = [0x90u8; PAGE_SIZE];
        page[..prologue.len()].copy_from_slice(&prologue);

        let mut origin = [0u8; PAGE_SIZE];
        let proxy_va = 0xFFFF_F800_1122_3344;
        let stolen = build_origin_span(&mut page, None, 0, TARGET_VA, proxy_va, &mut origin)
            .expect("the real NtCreateFile prologue is relocatable");

        assert_eq!(stolen, 14);
        assert_eq!(&origin[..14], &prologue);
        assert_register_jump(&origin[14..14 + ABSOLUTE_JUMP_LEN], TARGET_VA + 14);
        assert_register_jump(&page[..ABSOLUTE_JUMP_LEN], proxy_va);
        assert_eq!(page[14], 0x90);
    }

    #[test]
    fn origin_rejects_a_zero_proxy() {
        let mut page = prologue_page();
        let mut origin = [0u8; PAGE_SIZE];

        assert_eq!(
            build_origin_span(&mut page, None, 0x40, TARGET_VA, 0, &mut origin),
            Err(DetourError::InvalidTarget)
        );
    }

    #[test]
    fn trampoline_handles_a_prologue_longer_than_the_patch() {
        let mut page = [0x90u8; PAGE_SIZE];
        let prologue = [
            0x48, 0x89, 0x5C, 0x24, 0x08, // mov [rsp+8], rbx        (5)
            0x48, 0x89, 0x74, 0x24, 0x10, // mov [rsp+0x10], rsi     (5)
            0x48, 0x8B, 0x03,             // mov rax, [rbx]          (3)
            0x48, 0x81, 0xC4, 0x08, 0x00, 0x00, 0x00, // add rsp, 8 (7)
        ];
        page[0x40..0x40 + prologue.len()].copy_from_slice(&prologue);

        let mut stub = [0u8; PAGE_SIZE];
        let stolen = build(
            &mut page, 0x40, TARGET_VA, TRAMPOLINE_VA, KEY, SLOT, &mut stub,
        )
        .expect("prologue is relocatable");

        assert_eq!(stolen, 20);
        let stolen_at = TRAMPOLINE_OFFSET + REPORT_LEN;
        assert_eq!(
            &stub[stolen_at..stolen_at + stolen],
            &prologue
        );

        let back_at = stolen_at + stolen;
        assert_eq!(
            u64::from_le_bytes(stub[back_at + 6..back_at + 14].try_into().unwrap()),
            TARGET_VA + 20
        );
    }

    #[test]
    fn absolute_jump_encoding_is_position_free() {
        let mut bytes = [0u8; 14];
        write_absolute_jump(&mut bytes, 0xFFFF_F800_1234_5678);

        assert_eq!(&bytes[0..6], &[0xFF, 0x25, 0x00, 0x00, 0x00, 0x00]);
        assert_eq!(&bytes[6..], &0xFFFF_F800_1234_5678u64.to_le_bytes());
    }

    #[test]
    fn rejects_rip_relative_prologue() {
        let mut page = [0x90u8; PAGE_SIZE];
        page[0x40..0x47].copy_from_slice(&[0x48, 0x8D, 0x05, 0x11, 0x22, 0x33, 0x44]);

        let mut stub = [0u8; PAGE_SIZE];
        assert_eq!(
            build(&mut page, 0x40, TARGET_VA, TRAMPOLINE_VA, KEY, SLOT, &mut stub),
            Err(DetourError::UnsupportedInstruction)
        );
    }

    #[test]
    fn rejects_relative_branch() {
        let mut page = [0x90u8; PAGE_SIZE];
        page[0x40..0x45].copy_from_slice(&[0xE9, 0x00, 0x00, 0x00, 0x00]);

        let mut stub = [0u8; PAGE_SIZE];
        assert_eq!(
            build(&mut page, 0x40, TARGET_VA, TRAMPOLINE_VA, KEY, SLOT, &mut stub),
            Err(DetourError::UnsupportedInstruction)
        );
    }

    #[test]
    fn rejects_breakpoint_and_ud2_in_the_stolen_bytes() {
        let mut page = [0x90u8; PAGE_SIZE];
        page[0x40] = 0xCC;

        let mut stub = [0u8; PAGE_SIZE];
        assert_eq!(
            build(&mut page, 0x40, TARGET_VA, TRAMPOLINE_VA, KEY, SLOT, &mut stub),
            Err(DetourError::UnsupportedInstruction)
        );

        page[0x40] = 0x0F;
        page[0x41] = 0x0B;
        assert_eq!(
            build(&mut page, 0x40, TARGET_VA, TRAMPOLINE_VA, KEY, SLOT, &mut stub),
            Err(DetourError::UnsupportedInstruction)
        );
    }

    #[test]
    fn rejects_patch_that_does_not_fit_in_the_page() {
        let mut page = [0x90u8; PAGE_SIZE];
        let mut stub = [0u8; PAGE_SIZE];

        assert_eq!(
            build(&mut page, PAGE_SIZE - 13, TARGET_VA, TRAMPOLINE_VA, KEY, SLOT, &mut stub),
            Err(DetourError::PatchOutOfRange)
        );
    }

    #[test]
    fn rejects_detour_without_hypercalls() {
        let mut page = prologue_page();
        let mut stub = [0u8; PAGE_SIZE];

        assert_eq!(
            build(&mut page, 0x40, TARGET_VA, TRAMPOLINE_VA, 0, SLOT, &mut stub),
            Err(DetourError::HypercallsDisabled)
        );
    }

    #[test]
    fn last_instruction_may_end_at_the_page_boundary() {
        let mut page = [0x90u8; PAGE_SIZE];
        let mut stub = [0u8; PAGE_SIZE];

        let stolen = build(
            &mut page,
            PAGE_SIZE - 14,
            TARGET_VA,
            TRAMPOLINE_VA,
            KEY,
            SLOT,
            &mut stub,
        )
        .expect("NOPs are relocatable");

        let stolen_at = TRAMPOLINE_OFFSET + REPORT_LEN;
        assert_eq!(stolen, 14);
        assert_register_jump(
            &page[PAGE_SIZE - ABSOLUTE_JUMP_LEN..PAGE_SIZE],
            TRAMPOLINE_VA,
        );
        assert_eq!(
            &stub[stolen_at + stolen + 6..stolen_at + stolen + 14],
            &(TARGET_VA + 14).to_le_bytes()
        );
    }

    #[test]
    fn needs_second_page_detects_boundary_cases() {
        assert!(!needs_second_page(&[0x90u8; 14]));

        assert!(needs_second_page(&[0x90u8; 13]));

        let mut bytes = [0x90u8; 32];
        bytes[..10].copy_from_slice(&[0x48, 0xB8, 1, 2, 3, 4, 5, 6, 7, 8]);
        assert!(!needs_second_page(&bytes[..15]));

        bytes[10..20].copy_from_slice(&[0x48, 0xB8, 1, 2, 3, 4, 5, 6, 7, 8]);
        assert!(needs_second_page(&bytes[..15]));
    }

    #[test]
    fn cross_page_patch_and_stolen_bytes() {
        let mut page_a = [0x90u8; PAGE_SIZE];
        let mut page_b = [0x90u8; PAGE_SIZE];
        let mut stub = [0u8; PAGE_SIZE];

        let offset = PAGE_SIZE - 8;
        let stolen = build_span(
            &mut page_a,
            Some(&mut page_b),
            offset,
            TARGET_VA,
            TRAMPOLINE_VA,
            KEY,
            SLOT,
            &mut stub,
        )
        .expect("NOPs are relocatable");

        assert_eq!(stolen, 14);

        let mut expected = [0u8; ABSOLUTE_JUMP_LEN];
        expected[0] = 0x49;
        expected[1] = 0xBB;
        expected[2..10].copy_from_slice(&TRAMPOLINE_VA.to_le_bytes());
        expected[10..14].copy_from_slice(&[0x41, 0xFF, 0xE3, 0x90]);
        assert_eq!(&page_a[offset..PAGE_SIZE], &expected[..8]);
        assert_eq!(&page_b[..6], &expected[8..]);

        let stolen_at = TRAMPOLINE_OFFSET + REPORT_LEN;
        assert_eq!(
            &stub[stolen_at..stolen_at + stolen],
            &[0x90u8; 14]
        );
        assert_eq!(
            &stub[stolen_at + stolen + 6..stolen_at + stolen + 14],
            &(TARGET_VA + 14).to_le_bytes()
        );
    }

    #[test]
    fn cross_page_rejects_without_secondary() {
        let mut page = [0x90u8; PAGE_SIZE];
        let mut stub = [0u8; PAGE_SIZE];

        assert_eq!(
            build(
                &mut page,
                PAGE_SIZE - 8,
                TARGET_VA,
                TRAMPOLINE_VA,
                KEY,
                SLOT,
                &mut stub
            ),
            Err(DetourError::PatchOutOfRange)
        );
    }

    #[test]
    fn cross_page_instructions_are_decoded_across_the_boundary() {
        let mut page_a = [0x90u8; PAGE_SIZE];
        let mut page_b = [0x90u8; PAGE_SIZE];
        let mut stub = [0u8; PAGE_SIZE];

        let prologue = [
            0x48, 0x89, 0x5C, 0x24, 0x08, // mov [rsp+8], rbx
            0x48, 0x89, 0x74, 0x24, 0x10, // mov [rsp+0x10], rsi
            0x57, // push rdi
            0x48, 0x83, 0xEC, 0x20, // sub rsp, 0x20
        ];

        let offset = PAGE_SIZE - 5;
        page_a[offset..].copy_from_slice(&prologue[..5]);
        page_b[..prologue.len() - 5].copy_from_slice(&prologue[5..]);

        let stolen = build_span(
            &mut page_a,
            Some(&mut page_b),
            offset,
            TARGET_VA,
            TRAMPOLINE_VA,
            KEY,
            SLOT,
            &mut stub,
        )
        .expect("prologue is relocatable across the boundary");

        let stolen_at = TRAMPOLINE_OFFSET + REPORT_LEN;
        assert_eq!(stolen, 15);
        assert_eq!(
            &stub[stolen_at..stolen_at + stolen],
            &prologue
        );
    }
}
