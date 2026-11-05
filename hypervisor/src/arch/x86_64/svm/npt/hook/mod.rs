//! NPT hooks for the SVM backend: byte-patch and NPT function hooks.

use core::sync::atomic::{AtomicBool, AtomicU32, AtomicU64, Ordering};

use self::{
    method as debug,
    wait::{WaitError, WaitTable},
};

use crate::{
    arch::x86_64::{
        addr::PhysAddr,
        guest_context::GuestContext,
        guest_memory,
        hook_events::{self, HookEvent},
        ldisasm,
        memory::{self, Page, PAGE_SIZE},
        paging::{self, DIRECT_MAP_SIZE},
        safe_routine::{self, HostExceptionInfo},
        svm::{npt::NptView, state::SvmVcpu},
        sync,
        tsc::tsc_hz,
        vcpu::MAX_CPUS,
    },
    klog,
};

pub mod method;
pub mod wait;

// Host tests compile `wait` as a top-level module; re-exporting `vmcb` here
// keeps `super::vmcb` valid in both layouts.
pub use super::vmcb;

pub const MAX_HOOKS: usize = 32;
pub const PAGE_MASK: u64 = 0xfff;
/// `jmp qword ptr [rip+0]` + 8-byte target.
pub const DETOUR_JUMP_LEN: usize = 14;
const RFLAGS_IF: u64 = 1 << 9;
const RFLAGS_TF: u64 = 1 << 8;

/// 与 VMX 后端一致的编号，调用方 直接映射成文本。
#[repr(u32)]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum HookError {
    InvalidAddress = 1,
    NotMapped = 2,
    Unsupported = 3,
    NoFreeSlot = 4,
    NoMemory = 5,
    AlreadyExists = 6,
    NotFound = 7,
}

impl HookError {
    pub const fn as_u32(self) -> u32 {
        self as u32
    }
}

#[derive(Clone, Copy, Debug)]
pub struct HookEntry {
    pub active: bool,
    pub va: u64,
    /// Physical address of the patched byte.
    pub gpa: u64,
    pub original: u8,
    pub kind: u32,
    /// Private copy used by the execution view.
    pub clean_pfn: u64,
    /// Private copy used by the read/write view (NPT function hooks).
    pub data_pfn: u64,
    /// Generation that installed this view.
    pub generation: u32,
    /// Generation that retired it; `0` means "never installed".
    pub retired_generation: u32,
    /// Disable the hook after the first hit.
    pub once: bool,
    /// `hook::method::METHOD_*` used to produce the trap.
    pub method: u32,
    /// NPT function hook trampoline guest VA.
    pub trampoline: u64,
    /// Original bytes replaced by a `DetourDirect` jump (14 bytes).
    pub patch: [u8; 32],
    pub patch_len: u8,
}

impl HookEntry {
    const EMPTY: Self = Self {
        active: false,
        va: 0,
        gpa: 0,
        original: 0,
        kind: 0,
        clean_pfn: 0,
        data_pfn: 0,
        generation: 0,
        retired_generation: 0,
        once: false,
        method: debug::METHOD_BYTE_PATCH,
        trampoline: 0,
        patch: [0; 32],
        patch_len: 0,
    };
}

struct HookSlot {
    entry: HookEntry,
    /// Execution copy: patched with `0xCC` or a jump.
    clean: Page<[u8; PAGE_SIZE]>,
    /// Read/write copy for NPT function hooks (never patched).
    pristine: Page<[u8; PAGE_SIZE]>,
}

impl HookSlot {
    const fn empty() -> Self {
        Self {
            entry: HookEntry::EMPTY,
            clean: Page::empty(),
            pristine: Page::empty(),
        }
    }
}

static HOOKS: sync::Mutex<[HookSlot; MAX_HOOKS]> =
    sync::Mutex::new([const { HookSlot::empty() }; MAX_HOOKS]);

/// User-breakpoint wait entries, one per hook slot.
static WAITS: sync::Mutex<WaitTable<{ wait::MAX_HOOK_WAITS }>> =
    sync::Mutex::new(WaitTable::new());

/// Configurable wait timeout; `INFINITE_TIMEOUT` parks until an ACK.
static WAIT_TIMEOUT_MS: AtomicU64 = AtomicU64::new(wait::DEFAULT_TIMEOUT_MS);

/// Monotonic view generation, bumped by install/remove.
static GENERATION: AtomicU32 = AtomicU32::new(1);

/// Last generation each vCPU applied to its own NPT.
static APPLIED: [AtomicU32; MAX_CPUS] = [const { AtomicU32::new(0) }; MAX_CPUS];

/// vCPUs that have run `setup`; only these can hold a stale NPT view.
static ONLINE: [AtomicBool; MAX_CPUS] = [const { AtomicBool::new(false) }; MAX_CPUS];

static HITS: AtomicU32 = AtomicU32::new(0);

/// Limit the user-breakpoint diagnostic logs (thread lookup, wait state).
static USER_BP_LOGS: AtomicU32 = AtomicU32::new(0);

/// Temporary breakpoint used by `WAIT_ACTION_SINGLE_STEP`: after one instruction
/// the thread stops *before* the next one.
#[derive(Clone, Copy, Debug)]
pub struct StopAfter {
    pub va: u64,
    pub gpa: u64,
    pub original: u8,
}

/// Global single-step stop: the thread may migrate before reaching the next
/// instruction, so the temporary byte cannot live in per-vCPU state.
static STOP_AFTER: sync::Mutex<Option<StopAfter>> = sync::Mutex::new(None);

/// Per-vCPU single-step state.
#[derive(Clone, Copy, Debug)]
pub struct HookPending {
    pub va: u64,
    pub gpa: u64,
    pub clean_pfn: u64,
    pub slot: usize,
    pub once: bool,
    /// The wait ACK asked to stop before the next instruction.
    pub single_step: bool,
    /// RFLAGS before the single step (including the guest's own TF/IF).
    pub rflags: u64,
}

pub fn mark_online(cpu: u32) {
    if (cpu as usize) < MAX_CPUS {
        ONLINE[cpu as usize].store(true, Ordering::Release);
    }
}

/// A free slot whose previous page has been retired on every online vCPU.
fn slot_reusable(slot: &HookSlot) -> bool {
    if slot.entry.active {
        return false;
    }

    let retired = slot.entry.retired_generation;

    if retired == 0 {
        return true;
    }

    let mut minimum = u32::MAX;

    for cpu in 0..MAX_CPUS {
        if ONLINE[cpu].load(Ordering::Acquire) {
            minimum = minimum.min(APPLIED[cpu].load(Ordering::Acquire));
        }
    }

    retired <= minimum
}

/// Bring this vCPU's NPT up to the latest hook generation.
#[inline]
pub fn sync(svm: &mut SvmVcpu) {
    let cpu = svm.cpu_id as usize;

    if cpu >= MAX_CPUS {
        return;
    }

    let generation = GENERATION.load(Ordering::Acquire);
    let applied = APPLIED[cpu].load(Ordering::Acquire);

    if applied == generation {
        return;
    }

    let table = HOOKS.lock();

    for slot in table.iter() {
        let entry = slot.entry;

        let page = entry.gpa & !PAGE_MASK;

        if entry.active && entry.kind != 3 && entry.generation > applied {
            let pfn = if entry.method == debug::METHOD_NPT_HOOK {
                entry.data_pfn
            } else {
                entry.clean_pfn
            };
            let _ = svm.set_npt_view(page, NptView::ShadowNx, pfn);
        } else if !entry.active && entry.retired_generation > applied {
            let _ = svm.set_npt_view(page, NptView::OriginalExec, page >> 12);
        }
    }

    drop(table);

    APPLIED[cpu].store(generation, Ordering::Release);
}

pub fn install(svm: &mut SvmVcpu, va: u64, kind: u32, handler: u64) -> u32 {
    install_method(svm, va, kind, handler, debug::METHOD_BYTE_PATCH)
}

/// Install with an explicit NPT debug method.
///
/// 目前只有 byte-patch 接到页机制；其它方法返回 `Unsupported`，编号保留。
pub fn install_method(
    svm: &mut SvmVcpu,
    va: u64,
    kind: u32,
    handler: u64,
    method: u32,
) -> u32 {
    if method != debug::METHOD_BYTE_PATCH {
        klog!(
            "SVM debug: method {} is not supported (va={:#x} kind={})",
            method,
            va,
            kind
        );

        return HookError::Unsupported.as_u32();
    }

    install_byte_patch(svm, va, kind, handler)
}

/// Install the single NPT function hook: the execution copy carries `0xCC`, the
/// read/write copy stays pristine, and the real page is never modified.
pub fn install_npt(svm: &mut SvmVcpu, va: u64, handler: u64, trampoline: u64, encoded: u64) -> u32 {
    if !guest_memory::is_canonical(va) || handler == 0 || trampoline == 0 {
        return HookError::InvalidAddress.as_u32();
    }

    let kind = (encoded & 0xff) as u32;
    let page_hint = (encoded >> 8) << 12;

    let gpa = if page_hint != 0 {
        page_hint | (va & PAGE_MASK)
    } else {
        match guest_memory::translate_guest(svm.vmcb.save.cr3, va, false, false, false) {
            Ok(gpa) => gpa,
            Err(_) => return HookError::NotMapped.as_u32(),
        }
    };

    let page = gpa & !PAGE_MASK;
    let offset = (gpa & PAGE_MASK) as u16;

    if page + PAGE_SIZE as u64 > DIRECT_MAP_SIZE {
        return HookError::NotMapped.as_u32();
    }

    let mut table = HOOKS.lock();

    if table.iter().any(|slot| slot.entry.active && slot.entry.va == va) {
        return HookError::AlreadyExists.as_u32();
    }

    let Some(index) = table.iter().position(slot_reusable) else {
        return HookError::NoFreeSlot.as_u32();
    };

    let generation = GENERATION.fetch_add(1, Ordering::AcqRel) + 1;

    if table[index].clean.virt().as_u64() == 0 {
        match memory::alloc::<[u8; PAGE_SIZE]>() {
            Ok(page) => table[index].clean = page,
            Err(_) => return HookError::NoMemory.as_u32(),
        }
    }

    let clean_pfn = table[index].clean.pfn();

    if !copy_from_gpa(page, table[index].clean.as_mut_ptr() as *mut u8, PAGE_SIZE) {
        return HookError::NotMapped.as_u32();
    }

    let original = table[index].clean[offset as usize];

    // Build the trampoline: stolen original instructions followed by an
    // absolute jmp back to va + stolen_len.
    //
    // The bytes must come from the **original** page, i.e. before the INT3 is
    // written into the shadow copy below. Taking them after the patch copies
    // the `0xCC` as the first "instruction" and then decodes the rest from the
    // middle of the first instruction: the trampoline ends up executing
    // `int3` + garbage and the guest bugchecks (0x3B / STATUS_BREAKPOINT).
    let mut stolen = 0usize;
    let mut stolen_bytes = [0u8; 32];

    while stolen < DETOUR_JUMP_LEN {
        let Some(insn) = ldisasm::decode(&table[index].clean[offset as usize + stolen..]) else {
            return HookError::Unsupported.as_u32();
        };
        let len = insn.len as usize;
        if len == 0 || stolen + len > stolen_bytes.len() {
            return HookError::Unsupported.as_u32();
        }
        // 搬进 trampoline 的指令不能依赖原地址：RIP 相对操作数和相对跳转在
        // 新地址上会指向错误的目标。拒绝安装，而不是悄悄破坏 guest。
        if !insn.is_relocatable() {
            return HookError::Unsupported.as_u32();
        }
        stolen_bytes[stolen..stolen + len]
            .copy_from_slice(&table[index].clean[offset as usize + stolen..offset as usize + stolen + len]);
        stolen += len;
    }

    // Read/write view: a pristine copy of the page (no patch). Keeping it out
    // of the guest's real frame means data reads never see the patch and the
    // guest cannot modify the code the hook runs.
    if table[index].pristine.virt().as_u64() == 0 {
        match memory::alloc::<[u8; PAGE_SIZE]>() {
            Ok(page) => table[index].pristine = page,
            Err(_) => return HookError::NoMemory.as_u32(),
        }
    }

    if !copy_from_gpa(page, table[index].pristine.as_mut_ptr() as *mut u8, PAGE_SIZE) {
        return HookError::NotMapped.as_u32();
    }

    let data_pfn = table[index].pristine.pfn();

    // Execution view gets the patch.
    table[index].clean[offset as usize] = 0xcc;

    let tramp_gpa = match guest_memory::translate_guest(
        svm.vmcb.save.cr3,
        trampoline,
        false,
        false,
        false,
    ) {
        Ok(gpa) => gpa,
        Err(_) => return HookError::NotMapped.as_u32(),
    };

    if tramp_gpa + PAGE_SIZE as u64 > DIRECT_MAP_SIZE {
        return HookError::NotMapped.as_u32();
    }

    if !write_function_trampoline(tramp_gpa, handler, va, stolen, &stolen_bytes) {
        return HookError::NotMapped.as_u32();
    }

    let shadow_gpa = (clean_pfn << 12) | offset as u64;
    poke_gpa(shadow_gpa);

    table[index].entry = HookEntry {
        active: true,
        va,
        gpa,
        original,
        kind,
        clean_pfn,
        data_pfn,
        generation,
        retired_generation: 0,
        once: false,
        method: debug::METHOD_NPT_HOOK,
        trampoline,
        patch: [0; 32],
        patch_len: 0,
    };

    drop(table);

    // Read/write view is the pristine shadow copy; the first fetch NX-faults
    // and switches to the patched execution copy.
    if svm
        .set_npt_view(page, NptView::ShadowNx, data_pfn)
        .is_err()
    {
        return HookError::NoMemory.as_u32();
    }

    if (svm.cpu_id as usize) < MAX_CPUS {
        APPLIED[svm.cpu_id as usize].store(generation, Ordering::Release);
    }

    klog!(
        "SVM npt-hook: installed va={:#x} data={:#x} exec={:#x} handler={:#x} tramp={:#x}",
        va,
        data_pfn << 12,
        clean_pfn << 12,
        handler,
        trampoline
    );

    0
}

/// 写函数 hook 的 trampoline：保存 15 个 GPR → call handler → 执行 stolen
/// 指令 → 跳回 `va + stolen`。
fn write_function_trampoline(
    gpa: u64,
    handler: u64,
    va: u64,
    stolen: usize,
    stolen_bytes: &[u8],
) -> bool {
    let mut code = [0u8; 192];
    let mut pos = 0usize;

    for byte in [0x50u8, 0x51, 0x52, 0x53, 0x55, 0x56, 0x57] {
        code[pos] = byte;
        pos += 1;
    }
    for reg in 0..8u8 {
        code[pos] = 0x41;
        code[pos + 1] = 0x50 + reg;
        pos += 2;
    }
    code[pos] = 0x48;
    code[pos + 1] = 0xb8;
    code[pos + 2..pos + 10].copy_from_slice(&handler.to_le_bytes());
    pos += 10;
    code[pos] = 0xff;
    code[pos + 1] = 0xd0;
    pos += 2;
    for reg in (0..8u8).rev() {
        code[pos] = 0x41;
        code[pos + 1] = 0x58 + reg;
        pos += 2;
    }
    for byte in [0x5fu8, 0x5e, 0x5d, 0x5b, 0x5a, 0x59, 0x58] {
        code[pos] = byte;
        pos += 1;
    }

    code[pos..pos + stolen].copy_from_slice(&stolen_bytes[..stolen]);
    pos += stolen;
    code[pos] = 0xff;
    code[pos + 1] = 0x25;
    code[pos + 2..pos + 6].copy_from_slice(&0u32.to_le_bytes());
    code[pos + 6..pos + 14].copy_from_slice(&(va + stolen as u64).to_le_bytes());
    pos += DETOUR_JUMP_LEN;

    if !write_gpa(gpa, &code[..pos]) {
        return false;
    }

    poke_range(gpa, pos);
    true
}

fn install_byte_patch(svm: &mut SvmVcpu, va: u64, kind: u32, handler: u64) -> u32 {    if !guest_memory::is_canonical(va) {
        return HookError::InvalidAddress.as_u32();
    }

    // Software breakpoints, Hide and DetourDirect; the trampoline detour
    // (kind 1) needs a guest-visible trampoline page and is not implemented on
    // AMD.
    if kind == 4 {
        klog!("SVM: user breakpoint (kind 4) removed; use the NPT function hook");

        return HookError::Unsupported.as_u32();
    }

    if kind == 1 {
        klog!("SVM: trampoline detour (kind 1) is not supported on AMD");

        return HookError::Unsupported.as_u32();
    }

    if !matches!(kind, 0 | 2 | 3 | 4) {
        return HookError::Unsupported.as_u32();
    }

    let user = svm.vmcb.save.cs.selector & 3 == 3;
    let cr3 = svm.vmcb.save.cr3;

    // For user breakpoints the driver locks the target page and passes its
    // physical address in `handler` (only DetourDirect uses that slot for a
    // handler). Under KVA shadow the CR3 at VMCALL time may be the kernel CR3,
    // where the user page is not mapped the same way, so translating here can
    // point at the wrong frame — VMX has the same comment/fix.
    let page_hint = if handler != 0 { handler & !PAGE_MASK } else { 0 };

    let gpa = if page_hint != 0 {
        let translated = guest_memory::translate_guest(cr3, va, user, false, false).ok();

        match translated {
            Some(translated) if (translated & !PAGE_MASK) != page_hint => klog!(
                "SVM user bp: driver page={:#x} cr3 page={:#x} mismatch; using driver page va={:#x}",
                page_hint,
                translated & !PAGE_MASK,
                va
            ),
            Some(_) => klog!(
                "SVM user bp: using driver page {:#x} (va={:#x})",
                page_hint,
                va
            ),
            None => klog!(
                "SVM user bp: using driver page {:#x} (translation failed)",
                page_hint
            ),
        }

        page_hint | (va & PAGE_MASK)
    } else {
        match guest_memory::translate_guest(cr3, va, user, false, false) {
            Ok(gpa) => gpa,
            Err(_) => return HookError::NotMapped.as_u32(),
        }
    };

    if kind == 4 && ((va >> 47) & 1) != 0 {
        return HookError::Unsupported.as_u32();
    }

    let page = gpa & !PAGE_MASK;
    let offset = (gpa & PAGE_MASK) as u16;

    if page + PAGE_SIZE as u64 > DIRECT_MAP_SIZE {
        return HookError::NotMapped.as_u32();
    }

    let mut table = HOOKS.lock();

    if table.iter().any(|slot| slot.entry.active && slot.entry.va == va) {
        return HookError::AlreadyExists.as_u32();
    }

    // One NPT leaf backs one hook view. Two byte-patch hooks in the same 4 KiB
    // page would overwrite each other's clean page and `handle_npf` would pick
    // the wrong entry, so reject the second one until the page entry grows an
    // offset array (VMX-style).
    if table.iter().any(|slot| {
        slot.entry.active
            && slot.entry.method == debug::METHOD_BYTE_PATCH
            && (slot.entry.gpa & !PAGE_MASK) == page
            && slot.entry.va != va
    }) {
        return HookError::AlreadyExists.as_u32();
    }

    let Some(index) = table.iter().position(slot_reusable) else {
        return HookError::NoFreeSlot.as_u32();
    };

    let generation = GENERATION.fetch_add(1, Ordering::AcqRel) + 1;

    if kind == 3 {
        // DetourDirect: overwrite the entry with an absolute jump to a guest
        // kernel handler. No view switch is needed: the real page stays mapped
        // RWX and the jump is executed in place.
        if handler == 0 || !guest_memory::is_canonical(handler) {
            return HookError::InvalidAddress.as_u32();
        }

        if user || ((handler >> 47) & 1) == 0 {
            return HookError::Unsupported.as_u32();
        }

        if offset as usize + DETOUR_JUMP_LEN > PAGE_SIZE {
            return HookError::Unsupported.as_u32();
        }

        let mut original = [0u8; 32];

        if !copy_from_gpa(gpa, original.as_mut_ptr(), DETOUR_JUMP_LEN) {
            return HookError::NotMapped.as_u32();
        }

        let mut patch = [0u8; DETOUR_JUMP_LEN];
        patch[0..6].copy_from_slice(&[0xff, 0x25, 0, 0, 0, 0]);
        patch[6..14].copy_from_slice(&handler.to_le_bytes());

        if !write_gpa(gpa, &patch) {
            return HookError::NotMapped.as_u32();
        }

        poke_range(gpa, DETOUR_JUMP_LEN);

        table[index].entry = HookEntry {
            active: true,
            va,
            gpa,
            original: original[0],
            kind,
            clean_pfn: 0,
            data_pfn: 0,
            generation,
            retired_generation: 0,
            once: false,
            method: debug::METHOD_BYTE_PATCH,
            trampoline: 0,
            patch: original,
            patch_len: DETOUR_JUMP_LEN as u8,
        };

        drop(table);

        if (svm.cpu_id as usize) < MAX_CPUS {
            APPLIED[svm.cpu_id as usize].store(generation, Ordering::Release);
        }

        klog!("SVM hook: DetourDirect va={:#x} handler={:#x}", va, handler);

        return 0;
    }

    if table[index].clean.virt().as_u64() == 0 {
        match memory::alloc::<[u8; PAGE_SIZE]>() {
            Ok(page) => table[index].clean = page,
            Err(_) => return HookError::NoMemory.as_u32(),
        }
    }

    if kind == 2 {
        // Hide: the data view is a zero page; no byte is patched.
        table[index].clean.fill(0);
        let zero_pfn = table[index].clean.pfn();
        table[index].entry = HookEntry {
            active: true,
            va,
            gpa: page,
            original: 0,
            kind,
            clean_pfn: zero_pfn,
            data_pfn: zero_pfn,
            generation,
            retired_generation: 0,
            once: false,
            method: debug::METHOD_BYTE_PATCH,
            trampoline: 0,
            patch: [0; 32],
            patch_len: 0,
        };

        drop(table);

        if svm
            .set_npt_view(page, NptView::ShadowNx, zero_pfn)
            .is_err()
        {
            let mut table = HOOKS.lock();
            if let Some(slot) = table.iter_mut().find(|slot| slot.entry.va == va) {
                slot.entry.active = false;
            }
            return HookError::NoMemory.as_u32();
        }

        if (svm.cpu_id as usize) < MAX_CPUS {
            APPLIED[svm.cpu_id as usize].store(generation, Ordering::Release);
        }

        klog!("SVM hook: hiding page va={:#x} gpa={:#x}", va, page);
        return 0;
    }

    if !copy_from_gpa(page, table[index].clean.as_mut_ptr() as *mut u8, PAGE_SIZE) {
        return HookError::NotMapped.as_u32();
    }

    let mut original = 0u8;
    if !copy_from_gpa(gpa, &mut original as *mut u8, 1) {
        return HookError::NotMapped.as_u32();
    }

    if !write_gpa_byte(gpa, 0xcc) {
        return HookError::NotMapped.as_u32();
    }

    // Make the store visible to instruction fetch (see `poke_gpa`).
    poke_gpa(gpa);

    let clean_pfn = table[index].clean.pfn();
    table[index].entry = HookEntry {
        active: true,
        va,
        gpa,
        original,
        kind,
        clean_pfn,
        data_pfn: clean_pfn,
        generation,
        retired_generation: 0,
        once: false,
        method: debug::METHOD_BYTE_PATCH,
        trampoline: 0,
        patch: [0; 32],
        patch_len: 0,
    };

    drop(table);

    if svm
        .set_npt_view(page, NptView::ShadowNx, clean_pfn)
        .is_err()
    {
        // Roll the byte and the slot back; the view was not changed.
        write_gpa_byte(gpa, original);
        poke_gpa(gpa);
        let mut table = HOOKS.lock();
        if let Some(slot) = table.iter_mut().find(|slot| slot.entry.va == va) {
            slot.entry.active = false;
        }
        return HookError::NoMemory.as_u32();
    }

    if (svm.cpu_id as usize) < MAX_CPUS {
        APPLIED[svm.cpu_id as usize].store(generation, Ordering::Release);
    }

    // VMX pokes after every leaf switch too; real-exec keeps the same PA, so
    // the SMC store is what makes the decoder see the patched byte.
    poke_gpa(gpa);

    // Field diagnostic: prove the patch is in the real page and the clean copy
    // still holds the original byte (guest data view).
    {
        let mut real = 0u8;
        let mut copy = 0u8;
        let offset = (gpa & PAGE_MASK) as u64;

        let _ = copy_from_gpa(gpa, &mut real as *mut u8, 1);
        let _ = copy_from_gpa(
            (clean_pfn << 12) | offset,
            &mut copy as *mut u8,
            1,
        );

        klog!(
            "SVM hook: real[{}]={:#x} copy[{}]={:#x} (original={:#x})",
            offset,
            real,
            offset,
            copy,
            original
        );
    }

    klog!(
        "SVM hook: installed breakpoint va={:#x} gpa={:#x} kind={} cr3={:#x} user={}",
        va,
        gpa,
        kind,
        cr3,
        user
    );

    0
}

/// Restore every active hook before leaving virtualization.
///
/// STOP clears EFER.SVME, so from the next instruction on there is no #BP/#NPF
/// handler. Any `0xCC`/detour bytes still in guest memory would bugcheck the
/// guest (typically 0x3B), so they are restored here.
pub fn shutdown(svm: &mut SvmVcpu) {
    let mut table = HOOKS.lock();

    for slot in table.iter_mut() {
        let entry = slot.entry;

        if !entry.active {
            continue;
        }

        if entry.kind == 3 {
            if write_gpa(entry.gpa, &entry.patch[..entry.patch_len as usize]) {
                poke_range(entry.gpa, entry.patch_len as usize);
            }
        } else if entry.kind != 2 && entry.method != debug::METHOD_NPT_HOOK {
            if write_gpa_byte(entry.gpa, entry.original) {
                poke_gpa(entry.gpa);
            }
        }

        slot.entry.active = false;
    }

    drop(table);

    svm.hook_pending = None;
    svm.npt_shadow_view = None;
    svm.vmcb.save.rflags &= !RFLAGS_TF;
}

pub fn remove(svm: &mut SvmVcpu, va: u64) -> u32 {
    let mut table = HOOKS.lock();

    let Some(index) = table
        .iter()
        .position(|slot| slot.entry.active && slot.entry.va == va)
    else {
        return HookError::NotFound.as_u32();
    };

    let entry = table[index].entry;
    let generation = GENERATION.fetch_add(1, Ordering::AcqRel) + 1;

    // A pending single-step stop on this page must be undone first.
    {
        let mut guard = STOP_AFTER.lock();

        if let Some(stop) = *guard {
            if (stop.gpa & !PAGE_MASK) == (entry.gpa & !PAGE_MASK) {
                let _ = write_gpa_byte(stop.gpa, stop.original);
                poke_gpa(stop.gpa);
                *guard = None;
            }
        }
    }

    // A DetourDirect has no view; restore the 14 original bytes and retire.
    if entry.kind == 3 {
        if !write_gpa(entry.gpa, &entry.patch[..entry.patch_len as usize]) {
            return HookError::NotMapped.as_u32();
        }

        poke_range(entry.gpa, entry.patch_len as usize);

        table[index].entry.active = false;
        table[index].entry.retired_generation = generation;
        let _ = WAITS.lock().cancel_slot(index);
        drop(table);

        if (svm.cpu_id as usize) < MAX_CPUS {
            APPLIED[svm.cpu_id as usize].store(generation, Ordering::Release);
        }

        klog!("SVM hook: removed DetourDirect va={:#x}", va);

        return 0;
    }

    // Hide hooks never patched a byte.
    if entry.kind != 2 && !write_gpa_byte(entry.gpa, entry.original) {
        return HookError::NotMapped.as_u32();
    }

    if entry.kind != 2 {
        poke_gpa(entry.gpa);
    }

    table[index].entry.active = false;
    table[index].entry.retired_generation = generation;
    let _ = WAITS.lock().cancel_slot(index);
    drop(table);

    if svm.hook_pending.map(|pending| pending.va) == Some(va) {
        svm.hook_pending = None;
    }

    let page = entry.gpa & !PAGE_MASK;
    if svm
        .set_npt_view(page, NptView::OriginalExec, page >> 12)
        .is_err()
    {
        return HookError::NoMemory.as_u32();
    }

    if (svm.cpu_id as usize) < MAX_CPUS {
        APPLIED[svm.cpu_id as usize].store(generation, Ordering::Release);
    }

    klog!("SVM hook: removed breakpoint va={:#x}", va);

    0
}

/// Handle an NPF on a hooked page. Returns `true` when the fault was consumed.
pub fn handle_npf(svm: &mut SvmVcpu, gpa: u64, fetch: bool) -> bool {
    if !fetch {
        return false;
    }

    let page = gpa & !PAGE_MASK;
    let table = HOOKS.lock();

    let Some((data_pfn, kind, entry_gpa, method, clean_pfn)) = table
        .iter()
        .find(|slot| slot.entry.active && (slot.entry.gpa & !PAGE_MASK) == page)
        .map(|slot| {
            (
                slot.entry.data_pfn,
                slot.entry.kind,
                slot.entry.gpa,
                slot.entry.method,
                slot.entry.clean_pfn,
            )
        })
    else {
        return false;
    };

    drop(table);

    if kind == 2 {
        // Hide: execution is served from the real page until the next exit,
        // then the zero-page data view is put back.
        svm.hide_exec = Some(page);
    }

    let (view, frame) = if method == debug::METHOD_NPT_HOOK {
        (NptView::ShadowExec, clean_pfn)
    } else {
        (NptView::OriginalExec, page >> 12)
    };

    let ok = svm.set_npt_view(page, view, frame).is_ok();

    if ok && method == debug::METHOD_NPT_HOOK {
        // 取指视图现在指向带 0xCC 的影子页；数据读写同样走这个 leaf。
        // 记下恢复所需的读写副本，下一次 VM exit 开头切回 ShadowNx。
        svm.npt_shadow_view = Some((page, data_pfn));
    }

    if ok {
        poke_gpa(entry_gpa);
    }

    static NPF_LOGS: AtomicU32 = AtomicU32::new(0);

    if NPF_LOGS.fetch_add(1, Ordering::Relaxed) < 8 {
        klog!(
            "SVM hook: npf fetch gpa={:#x} page={:#x} rip={:#x} kind={} -> OriginalExec ({})",
            gpa,
            page,
            svm.vmcb.save.rip,
            kind,
            ok
        );
    }

    ok
}

/// Whether `page` (GPA page base) still holds an active NPT function hook.
///
/// Used before restoring the read/write view from [`SvmVcpu::npt_shadow_view`]:
/// if the hook was removed in the meantime, `sync` owns the view and this vCPU
/// must not set `NX` on a page that is no longer hooked.
pub fn is_active_npt_page(page: u64) -> bool {
    let table = HOOKS.lock();

    table.iter().any(|slot| {
        slot.entry.active
            && slot.entry.method == debug::METHOD_NPT_HOOK
            && (slot.entry.gpa & !PAGE_MASK) == page
    })
}

/// Zero-page PFN of the Hide hook covering `page`, for restoring the data view.
pub fn hide_zero_pfn(page: u64) -> Option<u64> {
    let table = HOOKS.lock();

    table
        .iter()
        .find(|slot| slot.entry.active && slot.entry.kind == 2 && slot.entry.gpa == page)
        .map(|slot| slot.entry.clean_pfn)
}

/// Handle `#BP`. Returns `false` when the breakpoint is not ours.
pub fn handle_bp(svm: &mut SvmVcpu, ctx: &mut GuestContext) -> bool {
    if svm.hook_pending.is_some() {
        return false;
    }

    let rip = svm.vmcb.save.rip;

    // A single-step stop placed at the instruction after the last one matches
    // before the normal hook table. It is global: the thread may have migrated.
    let stop = {
        let mut guard = STOP_AFTER.lock();

        match *guard {
            Some(stop) if stop.va == rip || stop.va == rip.wrapping_sub(1) => {
                *guard = None;
                Some(stop)
            }
            _ => None,
        }
    };

    if let Some(stop) = stop {
        let _ = write_gpa_byte(stop.gpa, stop.original);
        poke_gpa(stop.gpa);
        svm.vmcb.save.rip = stop.va;

        let page = stop.gpa & !PAGE_MASK;
        if let Some(clean_pfn) = page_clean_pfn(page) {
            let _ = svm.set_npt_view(page, NptView::ShadowNx, clean_pfn);
        } else {
            let _ = svm.set_npt_view(page, NptView::OriginalExec, page >> 12);
        }

        let hit = HITS.fetch_add(1, Ordering::Relaxed) + 1;
        hook_events::push_event(HookEvent {
            id: hit as u64,
            tsc: unsafe { core::arch::x86_64::_rdtsc() },
            cpu: svm.cpu_id,
            kind: 4,
            va: stop.va,
            rip,
            rax: ctx.rax,
            rcx: ctx.rcx,
            rdx: ctx.rdx,
            r8: ctx.r8,
            r9: ctx.r9,
        });

        klog!("SVM hook: single-step stop at va={:#x}", stop.va);

        return true;
    }

    let mut single_step = false;

    let table = HOOKS.lock();

    // Depending on how the intercepted trap reports RIP, it points either at
    // the INT3 or one byte past it. Match both so the hook is found on either
    // behaviour, and always rewind to the breakpoint address.
    let Some(index) = table
        .iter()
        .position(|slot| slot.entry.active && slot.entry.va == rip)
        .or_else(|| {
            table
                .iter()
                .position(|slot| slot.entry.active && slot.entry.va == rip.wrapping_sub(1))
        })
    else {
        return false;
    };

    let entry = table[index].entry;
    let bp_va = entry.va;

    if entry.method == debug::METHOD_NPT_HOOK {
        let page = entry.gpa & !PAGE_MASK;

        // 数据读写视图恢复到未打补丁的副本，再进入 trampoline。
        let _ = svm.set_npt_view(page, NptView::ShadowNx, entry.data_pfn);
        svm.vmcb.save.rip = entry.trampoline;

        let hit = HITS.fetch_add(1, Ordering::Relaxed) + 1;
        hook_events::push_event(HookEvent {
            id: hit as u64,
            tsc: unsafe { core::arch::x86_64::_rdtsc() },
            cpu: svm.cpu_id,
            kind: 1,
            va: bp_va,
            rip,
            rax: ctx.rax,
            rcx: ctx.rcx,
            rdx: ctx.rdx,
            r8: ctx.r8,
            r9: ctx.r9,
        });
        klog!("SVM npt-hook: hit va={:#x} -> trampoline={:#x}", bp_va, entry.trampoline);

        return true;
    }

    let rflags = svm.vmcb.save.rflags;

    // User breakpoints can park the target on the INT3 until the debugger
    // answers; kernel breakpoints always single-step.
    if entry.kind == 4 {
        match current_guest_thread(svm) {
            None => {
                if USER_BP_LOGS.fetch_add(1, Ordering::Relaxed) < 8 {
                    klog!(
                        "SVM user bp: no ETHREAD (gs={:#x} kernel_gs={:#x} system_cr3={:#x}); single-step",
                        svm.vmcb.save.gs.base,
                        svm.vmcb.save.kernel_gs_base,
                        crate::hv::GLOBAL_ARGS.get().system_cr3
                    );
                }
            }
            Some(thread) => {
                // SAFETY: RDTSC has no side effects and is available at CPL0.
                let now = unsafe { core::arch::x86_64::_rdtsc() };
                let ticks =
                    wait::timeout_ticks(WAIT_TIMEOUT_MS.load(Ordering::Relaxed), tsc_hz());
                let registers = gpr_snapshot(ctx);
                let context =
                    wait::context_from_vmcb(&svm.vmcb, thread, bp_va, rip, &registers);
                let mut waits = WAITS.lock();

                match waits.hit(index, svm.cpu_id, context, now, ticks) {
                    Ok(seq) => {
                        // First hit: stay on INT3 and let the debugger answer.
                        drop(waits);
                        svm.vmcb.save.rip = bp_va;
                        push_hit_event(svm, &entry, seq, bp_va, rip, ctx);
                        if USER_BP_LOGS.fetch_add(1, Ordering::Relaxed) < 8 {
                            klog!(
                                "SVM user bp: hit va={:#x} thread={:#x} seq={}",
                                bp_va,
                                thread,
                                seq
                            );
                        }
                        drop(table);
                        return true;
                    }
                    Err(WaitError::AlreadyPending) => match waits.take_due(index, now) {
                        Some(action) => {
                            single_step = action == wait::WAIT_ACTION_SINGLE_STEP;
                            if let Some(wait) = waits.complete(index) {
                                apply_wait_context(svm, ctx, &wait);
                            }
                            drop(waits);
                        }
                        None => {
                            // Keep spinning.
                            drop(waits);
                            svm.vmcb.save.rip = bp_va;
                            drop(table);
                            return true;
                        }
                    },
                    Err(error) => {
                        if USER_BP_LOGS.fetch_add(1, Ordering::Relaxed) < 8 {
                            klog!(
                                "SVM user bp: wait rejected ({:?}); single-step",
                                error
                            );
                        }
                        drop(waits);
                    }
                }
            }
        }
    }

    // Execute the original instruction: restore the byte and rewind RIP.
    if !write_gpa_byte(entry.gpa, entry.original) {
        return false;
    }

    poke_gpa(entry.gpa);

    svm.vmcb.save.rip = bp_va;
    svm.vmcb.save.rflags = (rflags & !RFLAGS_IF) | RFLAGS_TF;
    svm.hook_pending = Some(HookPending {
        va: bp_va,
        gpa: entry.gpa,
        clean_pfn: entry.clean_pfn,
        slot: index,
        once: entry.once,
        single_step,
        rflags,
    });

    drop(table);

    let page = entry.gpa & !PAGE_MASK;
    let _ = svm.set_npt_view(page, NptView::OriginalExec, page >> 12);

    let hit = HITS.fetch_add(1, Ordering::Relaxed) + 1;
    push_hit_event(svm, &entry, hit as u64, bp_va, rip, ctx);

    if hit <= 4 {
        klog!(
            "SVM hook: breakpoint hit #{} va={:#x} rip={:#x}",
            hit,
            bp_va,
            rip
        );
    }

    true
}

/// Remove every hook on the 4 KiB page containing `gpa`.
pub fn remove_page(svm: &mut SvmVcpu, gpa: u64) -> u32 {
    let page = gpa & !PAGE_MASK;
    let mut removed = 0u32;

    loop {
        let va = {
            let table = HOOKS.lock();
            table
                .iter()
                .find(|slot| slot.entry.active && (slot.entry.gpa & !PAGE_MASK) == page)
                .map(|slot| slot.entry.va)
        };

        let Some(va) = va else {
            break;
        };

        let result = remove(svm, va);

        if result != 0 {
            return result;
        }

        removed += 1;
    }

    if removed == 0 {
        HookError::NotFound.as_u32()
    } else {
        0
    }
}

/// Toggle "disable after first hit" on the hook at `va`.
pub fn set_oneshot(va: u64, enable: bool) -> u32 {
    let mut table = HOOKS.lock();

    let Some(slot) = table
        .iter_mut()
        .find(|slot| slot.entry.active && slot.entry.va == va)
    else {
        return HookError::NotFound.as_u32();
    };

    slot.entry.once = enable;

    0
}

/// The SVM backend writes `0xCC` during install, so the commit step only has to
/// validate that the hook exists.
pub fn commit(va: u64) -> u32 {
    let table = HOOKS.lock();

    if table
        .iter()
        .any(|slot| slot.entry.active && slot.entry.va == va)
    {
        0
    } else {
        HookError::NotFound.as_u32()
    }
}

/// Handle the single-step `#DB`. Returns `false` when it is not ours.
pub fn handle_db(svm: &mut SvmVcpu) -> bool {
    handle_db_impl(svm, false)
}

fn handle_db_impl(svm: &mut SvmVcpu, aborted: bool) -> bool {
    let Some(pending) = svm.hook_pending.take() else {
        return false;
    };

    let current = svm.vmcb.save.rflags;

    // Keep the flags produced by the instruction, restore the guest's own
    // TF/IF, and drop our TF.
    svm.vmcb.save.rflags = (current & !(RFLAGS_TF | RFLAGS_IF))
        | (pending.rflags & (RFLAGS_TF | RFLAGS_IF));

    let page = pending.gpa & !PAGE_MASK;

    if pending.once && !aborted {
        // One-shot: the byte was already restored at the hit; retire the slot
        // instead of re-arming it.
        let generation = GENERATION.fetch_add(1, Ordering::AcqRel) + 1;

        {
            let mut table = HOOKS.lock();
            if let Some(slot) = table.get_mut(pending.slot) {
                if slot.entry.va == pending.va {
                    slot.entry.active = false;
                    slot.entry.retired_generation = generation;
                }
            }
            let _ = WAITS.lock().cancel_slot(pending.slot);
        }

        let _ = svm.set_npt_view(page, NptView::OriginalExec, page >> 12);

        if (svm.cpu_id as usize) < MAX_CPUS {
            APPLIED[svm.cpu_id as usize].store(generation, Ordering::Release);
        }

        klog!("SVM hook: one-shot breakpoint disabled va={:#x}", pending.va);

        return true;
    }

    // Re-arm the breakpoint and return to the clean data view.
    let _ = write_gpa_byte(pending.gpa, 0xcc);
    poke_gpa(pending.gpa);

    if pending.single_step {
        let next_rip = svm.vmcb.save.rip;

        if (next_rip & !PAGE_MASK) == (pending.va & !PAGE_MASK) && next_rip != pending.va {
            let gpa = (pending.gpa & !PAGE_MASK) | (next_rip & PAGE_MASK);
            let mut original = 0u8;

            if copy_from_gpa(gpa, &mut original as *mut u8, 1)
                && original != 0xcc
                && write_gpa_byte(gpa, 0xcc)
            {
                poke_gpa(gpa);
                *STOP_AFTER.lock() = Some(StopAfter {
                    va: next_rip,
                    gpa,
                    original,
                });

                let _ = svm.set_npt_view(page, NptView::OriginalExec, page >> 12);

                return true;
            }
        }

        klog!("SVM hook: single-step leaves the page; continuing normally");
    }

    let _ = svm.set_npt_view(page, NptView::ShadowNx, pending.clean_pfn);

    true
}

/// Abort a single step that was interrupted by another exit. A one-shot hook is
/// re-armed: the original instruction did not execute.
pub fn abort_pending(svm: &mut SvmVcpu) {
    if svm.hook_pending.is_some() {
        let _ = handle_db_impl(svm, true);
    }
}

pub fn pending(svm: &SvmVcpu) -> bool {
    svm.hook_pending.is_some()
}

/// Undo a temporary single-step breakpoint before leaving virtualization.
pub fn restore_stop_after() {
    if let Some(stop) = STOP_AFTER.lock().take() {
        let _ = write_gpa_byte(stop.gpa, stop.original);
        poke_gpa(stop.gpa);
    }
}

/// Page of a pending single-step stop, if any.
pub fn stop_after_page() -> Option<u64> {
    STOP_AFTER.lock().map(|stop| stop.gpa & !PAGE_MASK)
}

/// Windows x64 KPCR: `Prcb` at 0x180, `CurrentThread` is the first pointer.
const KPCR_CURRENT_THREAD_OFFSET: u64 = 0x180 + 0x8;

fn read_guest_u64(cr3: u64, address: u64) -> Option<u64> {
    let phys = guest_memory::translate_guest(cr3, address, false, false, false).ok()?;
    let mut value = 0u64;

    copy_from_gpa(
        phys,
        &mut value as *mut u64 as *mut u8,
        core::mem::size_of::<u64>(),
    )
    .then_some(value)
}

/// Current guest `ETHREAD`, used to bind a wait to a thread. Kernel mode uses
/// the VMCB GS base (KPCR); user mode uses `KernelGsBase` (after `swapgs`).
fn current_guest_thread(svm: &SvmVcpu) -> Option<u64> {
    let user = svm.vmcb.save.cs.selector & 3 == 3;
    let kpcr = if user {
        svm.vmcb.save.kernel_gs_base
    } else {
        svm.vmcb.save.gs.base
    };

    if kpcr == 0 || (kpcr >> 47) & 1 == 0 {
        return None;
    }

    let system_cr3 = crate::hv::GLOBAL_ARGS.get().system_cr3 as u64;
    let thread = read_guest_u64(system_cr3, kpcr + KPCR_CURRENT_THREAD_OFFSET)?;

    if thread == 0 || (thread >> 47) & 1 == 0 {
        return None;
    }

    Some(thread)
}

fn gpr_snapshot(ctx: &GuestContext) -> [u64; 16] {
    [
        ctx.rax, ctx.rcx, ctx.rdx, ctx.rbx, 0, ctx.rbp, ctx.rsi, ctx.rdi, ctx.r8, ctx.r9, ctx.r10,
        ctx.r11, ctx.r12, ctx.r13, ctx.r14, ctx.r15,
    ]
}

/// Write a wait entry's register snapshot (including debugger edits) back to the
/// guest before the continue step.
fn apply_wait_context(svm: &mut SvmVcpu, ctx: &mut GuestContext, wait: &wait::HookWait) {
    ctx.rax = wait.rax;
    ctx.rcx = wait.rcx;
    ctx.rdx = wait.rdx;
    ctx.rbx = wait.rbx;
    ctx.rbp = wait.rbp;
    ctx.rsi = wait.rsi;
    ctx.rdi = wait.rdi;
    ctx.r8 = wait.r8;
    ctx.r9 = wait.r9;
    ctx.r10 = wait.r10;
    ctx.r11 = wait.r11;
    ctx.r12 = wait.r12;
    ctx.r13 = wait.r13;
    ctx.r14 = wait.r14;
    ctx.r15 = wait.r15;
    svm.vmcb.save.rsp = wait.rsp;
}

fn push_hit_event(
    svm: &SvmVcpu,
    entry: &HookEntry,
    id: u64,
    va: u64,
    rip: u64,
    ctx: &GuestContext,
) {
    hook_events::push_event(HookEvent {
        // SAFETY: RDTSC has no side effects and is available at CPL0.
        id,
        tsc: unsafe { core::arch::x86_64::_rdtsc() },
        cpu: svm.cpu_id,
        kind: entry.kind,
        va,
        rip,
        rax: ctx.rax,
        rcx: ctx.rcx,
        rdx: ctx.rdx,
        r8: ctx.r8,
        r9: ctx.r9,
    });
}

fn wait_error(error: WaitError) -> u32 {
    match error {
        WaitError::NotFound => HookError::NotFound.as_u32(),
        WaitError::Unsupported | WaitError::AlreadyPending => HookError::Unsupported.as_u32(),
        _ => HookError::InvalidAddress.as_u32(),
    }
}

pub fn wait_reply(seq: u64, action: u32, arg: u64) -> u32 {
    match WAITS.lock().ack(seq, action, arg) {
        Ok(_) => 0,
        Err(error) => wait_error(error),
    }
}

/// `ms == INFINITE_TIMEOUT` parks until ACK; `0` restores the default.
pub fn wait_timeout(ms: u64) -> u64 {
    let value = if ms == wait::INFINITE_TIMEOUT {
        wait::INFINITE_TIMEOUT
    } else if ms == 0 {
        wait::DEFAULT_TIMEOUT_MS
    } else {
        ms.min(wait::MAX_TIMEOUT_MS)
    };

    WAIT_TIMEOUT_MS.store(value, Ordering::Release);

    0
}

pub fn wait_set_reg(seq: u64, index: u8, value: u64) -> u32 {
    match WAITS.lock().set_reg(seq, index, value) {
        Ok(_) => 0,
        Err(error) => wait_error(error),
    }
}

pub fn wait_filter(va: u64, thread: u64) -> u32 {
    let index = {
        let table = HOOKS.lock();
        table
            .iter()
            .position(|slot| slot.entry.active && slot.entry.va == va)
    };

    let Some(index) = index else {
        return HookError::NotFound.as_u32();
    };

    match WAITS.lock().set_filter(index, va, thread) {
        Ok(()) => 0,
        Err(error) => wait_error(error),
    }
}

pub fn wait_cancel(seq: u64) -> u64 {
    WAITS.lock().cancel_seq(seq) as u64
}

/// Copy up to `capacity` wait entries into the guest buffer at `destination`.
pub fn wait_list(svm: &SvmVcpu, destination: u64, capacity: u64) -> u64 {
    let capacity = (capacity as usize).min(wait::MAX_HOOK_WAITS);

    if capacity == 0 {
        return 0;
    }

    let cr3 = svm.vmcb.save.cr3;
    let user = svm.vmcb.save.cs.selector & 3 == 3;
    let write_protect = svm.vmcb.save.cr0 & (1 << 16) != 0;
    let mut offset = 0u64;

    let copied = WAITS.lock().snapshot(capacity, |wait| {
        let Some(address) = destination.checked_add(offset) else {
            return false;
        };

        let bytes = unsafe {
            core::slice::from_raw_parts(
                wait as *const wait::HookWait as *const u8,
                core::mem::size_of::<wait::HookWait>(),
            )
        };

        match guest_memory::copy_to_guest(cr3, address, bytes, user, write_protect) {
            Ok(()) => {
                offset += core::mem::size_of::<wait::HookWait>() as u64;
                true
            }
            Err(_) => false,
        }
    });

    copied as u64
}

/// Clean-copy PFN of the hook on `page`, if any.
pub fn page_clean_pfn(page: u64) -> Option<u64> {
    let table = HOOKS.lock();

    table
        .iter()
        .find(|slot| slot.entry.active && slot.entry.kind != 3 && (slot.entry.gpa & !PAGE_MASK) == page)
        .map(|slot| slot.entry.clean_pfn)
}

fn copy_from_gpa(gpa: u64, destination: *mut u8, length: usize) -> bool {
    let Some(end) = gpa.checked_add(length as u64) else {
        return false;
    };

    if end > DIRECT_MAP_SIZE {
        return false;
    }

    let source = paging::phys_to_virt(PhysAddr::new(gpa)).as_ptr::<u8>();
    let mut info = HostExceptionInfo::new();

    safe_routine::memcpy_safe(&mut info, destination, source, length)
}

fn write_gpa_byte(gpa: u64, value: u8) -> bool {
    write_gpa(gpa, &[value])
}

/// Force the CPU to discard already-decoded instructions for this line.
///
/// x86 has no instruction that clears the DSB/uop cache; `INVLPGA`/TLB flushes
/// only affect translations. The architecturally guaranteed trigger is
/// self-modifying-code detection: a store to the same physical line makes the
/// core re-decode on the next fetch. Do a same-value volatile store, then
/// `CLFLUSH` + `MFENCE` so the write is visible to every cache level.
///
/// Without this, a hot code page keeps executing its cached decoded form and a
/// freshly patched `0xCC` is never seen (the VMX backend's `poke_gpa` does the
/// same thing).
fn poke_gpa(gpa: u64) {
    if gpa >= DIRECT_MAP_SIZE {
        return;
    }

    let pointer = paging::phys_to_virt(PhysAddr::new(gpa)).as_mut_ptr::<u8>();

    unsafe {
        let byte = core::ptr::read_volatile(pointer);
        core::ptr::write_volatile(pointer, byte);

        core::arch::asm!(
            "clflush [{pointer}]",
            pointer = in(reg) pointer,
            options(nostack, preserves_flags)
        );
        core::arch::asm!("mfence", options(nostack, preserves_flags));
    }
}

/// Poke every byte of a patch span (detour may cross cache lines).
fn poke_range(gpa: u64, length: usize) {
    for offset in 0..length {
        poke_gpa(gpa + offset as u64);
    }
}

fn write_gpa(gpa: u64, source: &[u8]) -> bool {
    let Some(end) = gpa.checked_add(source.len() as u64) else {
        return false;
    };

    if end > DIRECT_MAP_SIZE {
        return false;
    }

    let destination = paging::phys_to_virt(PhysAddr::new(gpa)).as_mut_ptr::<u8>();
    let mut info = HostExceptionInfo::new();

    safe_routine::memcpy_safe(&mut info, destination, source.as_ptr(), source.len())
}
