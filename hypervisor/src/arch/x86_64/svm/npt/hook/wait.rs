//! User-breakpoint wait table for the SVM backend.

use super::vmcb::Vmcb;

pub const MAX_HOOK_WAITS: usize = 32;

pub const WAIT_STATE_IDLE: u32 = 0;
pub const WAIT_STATE_PENDING: u32 = 1;
pub const WAIT_STATE_APPLYING: u32 = 2;

pub const WAIT_ACTION_CONTINUE: u32 = 0;
pub const WAIT_ACTION_SINGLE_STEP: u32 = 5;
pub const WAIT_ACTION_INJECT: u32 = 2;
pub const WAIT_ACTION_SET_REG: u32 = 3;
pub const WAIT_ACTION_KILL: u32 = 4;

pub const DEFAULT_TIMEOUT_MS: u64 = 1000;
pub const MAX_TIMEOUT_MS: u64 = 60_000;
pub const INFINITE_TIMEOUT: u64 = u64::MAX;

/// Wait entry as seen by 调用方 (ABI, 216 bytes).
#[repr(C)]
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct HookWait {
    pub seq: u64,
    pub thread: u64,
    pub cr3: u64,
    pub va: u64,
    pub rip: u64,
    pub rsp: u64,

    pub rax: u64,
    pub rcx: u64,
    pub rdx: u64,
    pub rbx: u64,
    pub rbp: u64,
    pub rsi: u64,
    pub rdi: u64,
    pub r8: u64,
    pub r9: u64,
    pub r10: u64,
    pub r11: u64,
    pub r12: u64,
    pub r13: u64,
    pub r14: u64,
    pub r15: u64,

    pub tsc: u64,
    pub deadline: u64,
    pub slot: u32,
    pub cpu: u32,
    pub action: u32,
    pub state: u32,
    pub arg: u64,
    pub hits: u64,
}

const _: () = assert!(core::mem::size_of::<HookWait>() == 216);

impl HookWait {
    #[cfg(test)]
    pub const fn owned_by(&self, va: u64, thread: u64) -> bool {
        self.va == va && thread != 0 && self.thread == thread
    }

    #[cfg(test)]
    pub const fn gpr(&self, index: u8) -> Option<u64> {
        Some(match index {
            0 => self.rax,
            1 => self.rcx,
            2 => self.rdx,
            3 => self.rbx,
            4 => self.rsp,
            5 => self.rbp,
            6 => self.rsi,
            7 => self.rdi,
            8 => self.r8,
            9 => self.r9,
            10 => self.r10,
            11 => self.r11,
            12 => self.r12,
            13 => self.r13,
            14 => self.r14,
            15 => self.r15,
            _ => return None,
        })
    }

    pub fn set_gpr(&mut self, index: u8, value: u64) -> bool {
        match index {
            0 => self.rax = value,
            1 => self.rcx = value,
            2 => self.rdx = value,
            3 => self.rbx = value,
            4 => self.rsp = value,
            5 => self.rbp = value,
            6 => self.rsi = value,
            7 => self.rdi = value,
            8 => self.r8 = value,
            9 => self.r9 = value,
            10 => self.r10 = value,
            11 => self.r11 = value,
            12 => self.r12 = value,
            13 => self.r13 = value,
            14 => self.r14 = value,
            15 => self.r15 = value,
            _ => return false,
        }

        true
    }
}

/// Guest context snapshot taken at the hit.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct WaitContext {
    pub thread: u64,
    pub cr3: u64,
    pub va: u64,
    pub rip: u64,
    pub rsp: u64,
    pub rax: u64,
    pub rcx: u64,
    pub rdx: u64,
    pub rbx: u64,
    pub rbp: u64,
    pub rsi: u64,
    pub rdi: u64,
    pub r8: u64,
    pub r9: u64,
    pub r10: u64,
    pub r11: u64,
    pub r12: u64,
    pub r13: u64,
    pub r14: u64,
    pub r15: u64,
}

/// Per-breakpoint thread filter.
#[repr(C)]
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct WaitFilter {
    pub va: u64,
    pub thread: u64,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum WaitError {
    NoSlot,
    AlreadyPending,
    NotFound,
    InvalidAction,
    Unsupported,
    Filtered,
    NoThread,
    InvalidRegister,
}

#[derive(Clone, Copy, Debug)]
struct Entry {
    info: HookWait,
    requested: Option<u32>,
}

impl Entry {
    const fn is_idle(&self) -> bool {
        self.info.state == WAIT_STATE_IDLE
    }
}

pub struct WaitTable<const N: usize> {
    entries: [Option<Entry>; N],
    filters: [Option<WaitFilter>; N],
    next_seq: u64,
}

impl<const N: usize> WaitTable<N> {
    pub const fn new() -> Self {
        Self {
            entries: [None; N],
            filters: [None; N],
            next_seq: 1,
        }
    }

    #[cfg(test)]
    pub fn len(&self) -> usize {
        self.entries.iter().flatten().count()
    }

    #[cfg(test)]
    pub fn lookup(&self, slot: usize) -> Option<&HookWait> {
        self.entries.get(slot)?.as_ref().map(|entry| &entry.info)
    }

    #[cfg(test)]
    pub fn filter(&self, slot: usize) -> Option<WaitFilter> {
        self.filters.get(slot).copied().flatten()
    }

    /// Record a hit. The first hit creates the entry; later hits update it in
    /// place and keep `seq`.
    pub fn hit(
        &mut self,
        slot: usize,
        cpu: u32,
        context: WaitContext,
        now: u64,
        timeout_ticks: u64,
    ) -> Result<u64, WaitError> {
        if slot >= N {
            return Err(WaitError::NoSlot);
        }

        if context.thread == 0 {
            return Err(WaitError::NoThread);
        }

        if let Some(filter) = self.filters[slot] {
            if filter.va == context.va && filter.thread != 0 && filter.thread != context.thread {
                return Err(WaitError::Filtered);
            }
        }

        let deadline = if timeout_ticks == INFINITE_TIMEOUT {
            INFINITE_TIMEOUT
        } else {
            now.saturating_add(timeout_ticks)
        };

        if let Some(entry) = self.entries[slot].as_mut() {
            if !entry.is_idle() {
                return Err(WaitError::AlreadyPending);
            }

            entry.requested = None;
            entry.info.thread = context.thread;
            entry.info.cr3 = context.cr3;
            entry.info.va = context.va;
            entry.info.rip = context.rip;
            entry.info.rsp = context.rsp;
            entry.info.rax = context.rax;
            entry.info.rcx = context.rcx;
            entry.info.rdx = context.rdx;
            entry.info.rbx = context.rbx;
            entry.info.rbp = context.rbp;
            entry.info.rsi = context.rsi;
            entry.info.rdi = context.rdi;
            entry.info.r8 = context.r8;
            entry.info.r9 = context.r9;
            entry.info.r10 = context.r10;
            entry.info.r11 = context.r11;
            entry.info.r12 = context.r12;
            entry.info.r13 = context.r13;
            entry.info.r14 = context.r14;
            entry.info.r15 = context.r15;
            entry.info.tsc = now;
            entry.info.deadline = deadline;
            entry.info.cpu = cpu;
            entry.info.action = WAIT_ACTION_CONTINUE;
            entry.info.state = WAIT_STATE_PENDING;
            entry.info.arg = 0;
            entry.info.hits += 1;

            return Ok(entry.info.seq);
        }

        let seq = self.next_seq;
        self.next_seq += 1;

        let info = HookWait {
            seq,
            thread: context.thread,
            cr3: context.cr3,
            va: context.va,
            rip: context.rip,
            rsp: context.rsp,
            rax: context.rax,
            rcx: context.rcx,
            rdx: context.rdx,
            rbx: context.rbx,
            rbp: context.rbp,
            rsi: context.rsi,
            rdi: context.rdi,
            r8: context.r8,
            r9: context.r9,
            r10: context.r10,
            r11: context.r11,
            r12: context.r12,
            r13: context.r13,
            r14: context.r14,
            r15: context.r15,
            tsc: now,
            deadline,
            slot: slot as u32,
            cpu,
            action: WAIT_ACTION_CONTINUE,
            state: WAIT_STATE_PENDING,
            arg: 0,
            hits: 1,
        };

        self.entries[slot] = Some(Entry {
            info,
            requested: None,
        });

        Ok(seq)
    }

    pub fn set_filter(&mut self, slot: usize, va: u64, thread: u64) -> Result<(), WaitError> {
        if slot >= N {
            return Err(WaitError::NoSlot);
        }

        self.filters[slot] = (thread != 0).then_some(WaitFilter { va, thread });
        self.cancel_va(va);

        Ok(())
    }

    pub fn set_reg(&mut self, seq: u64, index: u8, value: u64) -> Result<usize, WaitError> {
        let slot = self.find_seq(seq)?;
        let wait = self.entries[slot].as_mut().expect("just matched");

        if wait.info.state == WAIT_STATE_IDLE {
            return Err(WaitError::NotFound);
        }

        if !wait.info.set_gpr(index, value) {
            return Err(WaitError::InvalidRegister);
        }

        Ok(slot)
    }

    fn find_seq(&self, seq: u64) -> Result<usize, WaitError> {
        self.entries
            .iter()
            .position(|entry| entry.is_some_and(|entry| entry.info.seq == seq))
            .ok_or(WaitError::NotFound)
    }

    pub fn ack(&mut self, seq: u64, action: u32, arg: u64) -> Result<usize, WaitError> {
        match action {
            WAIT_ACTION_CONTINUE | WAIT_ACTION_SINGLE_STEP => {}
            WAIT_ACTION_INJECT | WAIT_ACTION_SET_REG | WAIT_ACTION_KILL => {
                return Err(WaitError::Unsupported)
            }
            _ => return Err(WaitError::InvalidAction),
        }

        let slot = self.find_seq(seq)?;
        let wait = self.entries[slot].as_mut().expect("just matched");

        if wait.info.state != WAIT_STATE_PENDING {
            return Err(WaitError::AlreadyPending);
        }

        wait.requested = Some(action);
        wait.info.action = action;
        wait.info.arg = arg;
        wait.info.state = WAIT_STATE_APPLYING;

        Ok(slot)
    }

    /// Action the target should apply on its next `#BP`, if any.
    pub fn take_due(&self, slot: usize, now: u64) -> Option<u32> {
        let wait = self.entries.get(slot)?.as_ref()?;

        match wait.info.state {
            WAIT_STATE_APPLYING => wait.requested,
            WAIT_STATE_PENDING if now >= wait.info.deadline => Some(WAIT_ACTION_CONTINUE),
            _ => None,
        }
    }

    /// Apply the action: keep the entry, return to Idle, and return its snapshot.
    pub fn complete(&mut self, slot: usize) -> Option<HookWait> {
        let entry = self.entries.get_mut(slot)?.as_mut()?;

        entry.requested = None;
        entry.info.state = WAIT_STATE_IDLE;

        Some(entry.info)
    }

    pub fn cancel_seq(&mut self, seq: u64) -> usize {
        if seq == 0 {
            return self.cancel_all();
        }

        match self.find_seq(seq) {
            Ok(slot) => {
                self.remove(slot);

                1
            }
            Err(_) => 0,
        }
    }

    fn remove(&mut self, slot: usize) -> Option<HookWait> {
        self.entries.get_mut(slot)?.take().map(|entry| entry.info)
    }

    pub fn cancel_slot(&mut self, slot: usize) -> Option<HookWait> {
        if slot < N {
            self.filters[slot] = None;
        }

        self.remove(slot)
    }

    pub fn cancel_va(&mut self, va: u64) -> usize {
        let mut cancelled = 0;

        for entry in self.entries.iter_mut() {
            if entry.is_some_and(|entry| entry.info.va == va) {
                *entry = None;
                cancelled += 1;
            }
        }

        cancelled
    }

    pub fn cancel_all(&mut self) -> usize {
        let mut cancelled = 0;

        for entry in self.entries.iter_mut() {
            if entry.take().is_some() {
                cancelled += 1;
            }
        }

        cancelled
    }

    /// Visit waits oldest-seq first, up to `capacity`.
    pub fn snapshot(&self, capacity: usize, mut consume: impl FnMut(&HookWait) -> bool) -> usize {
        let limit = capacity.min(N);
        let mut last = 0u64;
        let mut copied = 0;

        while copied < limit {
            let next = self
                .entries
                .iter()
                .flatten()
                .filter(|entry| entry.info.seq > last)
                .min_by_key(|entry| entry.info.seq);

            let Some(entry) = next else {
                break;
            };

            if !consume(&entry.info) {
                break;
            }

            last = entry.info.seq;
            copied += 1;
        }

        copied
    }
}

/// Convert a millisecond timeout to TSC ticks.
pub fn timeout_ticks(timeout_ms: u64, tsc_hz: u64) -> u64 {
    if timeout_ms == INFINITE_TIMEOUT {
        return INFINITE_TIMEOUT;
    }

    let hz = if tsc_hz == 0 { 1 } else { tsc_hz };

    timeout_ms.saturating_mul(hz) / 1_000
}

/// Snapshot of the VMCB registers used to build a [`WaitContext`].
pub fn context_from_vmcb(
    vmcb: &Vmcb,
    thread: u64,
    va: u64,
    rip: u64,
    registers: &[u64; 16],
) -> WaitContext {
    WaitContext {
        thread,
        cr3: vmcb.save.cr3,
        va,
        rip,
        rsp: vmcb.save.rsp,
        rax: registers[0],
        rcx: registers[1],
        rdx: registers[2],
        rbx: registers[3],
        rbp: registers[5],
        rsi: registers[6],
        rdi: registers[7],
        r8: registers[8],
        r9: registers[9],
        r10: registers[10],
        r11: registers[11],
        r12: registers[12],
        r13: registers[13],
        r14: registers[14],
        r15: registers[15],
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ctx(thread: u64) -> WaitContext {
        WaitContext {
            thread,
            cr3: 0x1ad000,
            va: 0x1000,
            rip: 0x1000,
            rsp: 0x8000,
            ..WaitContext::default()
        }
    }

    #[test]
    fn abi_layout_and_gpr_order() {
        assert_eq!(core::mem::size_of::<HookWait>(), 216);
        assert_eq!(core::mem::offset_of!(HookWait, deadline), 176);
        assert_eq!(core::mem::offset_of!(HookWait, hits), 208);

        let mut wait = HookWait::default();
        assert!(wait.set_gpr(4, 0xdead));
        assert_eq!(wait.rsp, 0xdead);
        assert_eq!(wait.gpr(4), Some(0xdead));
        assert!(!wait.set_gpr(16, 0));
    }

    #[test]
    fn lifecycle() {
        let mut waits = WaitTable::<4>::new();
        let seq = waits.hit(0, 1, ctx(0xffff_8000_1000), 100, 1000).unwrap();

        assert_eq!(waits.lookup(0).unwrap().state, WAIT_STATE_PENDING);
        assert_eq!(waits.take_due(0, 100), None);

        assert_eq!(waits.ack(seq, WAIT_ACTION_CONTINUE, 0), Ok(0));
        assert_eq!(waits.take_due(0, 100), Some(WAIT_ACTION_CONTINUE));

        waits.complete(0);
        assert_eq!(waits.lookup(0).unwrap().state, WAIT_STATE_IDLE);
        assert_eq!(
            waits.hit(0, 2, ctx(0xffff_8000_1000), 200, 1000),
            Ok(seq)
        );
        assert_eq!(waits.lookup(0).unwrap().hits, 2);
    }

    #[test]
    fn timeout_and_filter() {
        let mut waits = WaitTable::<4>::new();
        waits.set_filter(0, 0x1000, 0xffff_8000_2000).unwrap();

        assert_eq!(
            waits.hit(0, 0, ctx(0xffff_8000_1000), 0, 10),
            Err(WaitError::Filtered)
        );
        assert_eq!(waits.hit(0, 0, ctx(0), 0, 10), Err(WaitError::NoThread));

        waits.set_filter(0, 0x1000, 0).unwrap();
        waits
            .hit(0, 0, ctx(0xffff_8000_1000), 100, 10)
            .unwrap();
        assert_eq!(waits.take_due(0, 109), None);
        assert_eq!(waits.take_due(0, 110), Some(WAIT_ACTION_CONTINUE));
    }

    #[test]
    fn timeout_ticks_matches_the_vmx_formula() {
        assert_eq!(timeout_ticks(INFINITE_TIMEOUT, 0), INFINITE_TIMEOUT);
        assert_eq!(timeout_ticks(1, 1_000_000_000), 1_000_000);
        assert_eq!(timeout_ticks(10, 0), 0);
    }

    fn ctx_at(thread: u64, va: u64) -> WaitContext {
        WaitContext {
            thread,
            va,
            ..ctx(thread)
        }
    }

    #[test]
    fn snapshot_cancel_and_edit() {
        let mut waits = WaitTable::<4>::new();
        let first = waits
            .hit(0, 0, ctx_at(0xffff_8000_1000, 0x1000), 0, INFINITE_TIMEOUT)
            .unwrap();
        waits
            .hit(1, 1, ctx_at(0xffff_8000_2000, 0x2000), 0, INFINITE_TIMEOUT)
            .unwrap();

        let mut seen = [0u64; 2];
        let mut index = 0;
        let copied = waits.snapshot(8, |wait| {
            seen[index] = wait.seq;
            index += 1;
            true
        });

        // `snapshot` must not consume entries.
        assert_eq!(copied, 2);
        assert_eq!(waits.len(), 2);
        assert_eq!(seen, [first, first + 1]);

        assert_eq!(waits.snapshot(1, |_| true), 1);

        assert!(waits.set_reg(first, 0, 0x1234).is_ok());
        assert_eq!(waits.lookup(0).unwrap().rax, 0x1234);
        assert!(waits.set_reg(first, 16, 0).is_err());

        assert_eq!(waits.cancel_va(0x2000), 1);
        assert_eq!(waits.len(), 1);
        assert_eq!(waits.cancel_all(), 1);
        assert_eq!(waits.len(), 0);
    }
}
