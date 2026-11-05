//! Breakpoint/detour event ring shared by the VMX and SVM backends.
//!
//! `HYPERCALL_BREAKPOINT_EVENT_LIST` drains this ring into a guest buffer as a
//! packed array of 80-byte [`HookEvent`] records, so the layout is ABI.

#[repr(C)]
#[derive(Clone, Copy, Debug)]
pub struct HookEvent {
    pub id: u64,
    pub tsc: u64,
    pub cpu: u32,
    pub kind: u32,
    pub va: u64,
    pub rip: u64,
    pub rax: u64,
    pub rcx: u64,
    pub rdx: u64,
    pub r8: u64,
    pub r9: u64,
}

pub const MAX_HOOK_EVENTS: usize = 256;

const _: () = assert!(core::mem::size_of::<HookEvent>() == 80);

const EMPTY_HOOK_EVENT: HookEvent = HookEvent {
    id: 0,
    tsc: 0,
    cpu: 0,
    kind: 0,
    va: 0,
    rip: 0,
    rax: 0,
    rcx: 0,
    rdx: 0,
    r8: 0,
    r9: 0,
};

struct HookEventRing {
    items: [HookEvent; MAX_HOOK_EVENTS],
    head: usize,
    tail: usize,
    len: usize,
}

static HOOK_EVENTS: spin::Mutex<HookEventRing> = spin::Mutex::new(HookEventRing {
    items: [EMPTY_HOOK_EVENT; MAX_HOOK_EVENTS],
    head: 0,
    tail: 0,
    len: 0,
});

/// Push one event; the oldest entry is dropped when the ring is full.
pub fn push_event(event: HookEvent) {
    let mut ring = HOOK_EVENTS.lock();

    if ring.len == MAX_HOOK_EVENTS {
        ring.head = (ring.head + 1) % MAX_HOOK_EVENTS;
        ring.len -= 1;
    }

    let tail = ring.tail;
    ring.items[tail] = event;
    ring.tail = (tail + 1) % MAX_HOOK_EVENTS;
    ring.len += 1;
}

/// Drain up to `capacity` events into `consume`. An event is removed only when
/// the callback returns `true`.
#[cfg(test)]
pub fn drain_events(capacity: usize, mut consume: impl FnMut(&HookEvent) -> bool) -> usize {
    let mut ring = HOOK_EVENTS.lock();
    let limit = capacity.min(ring.len);
    let mut copied = 0;

    while copied < limit {
        let event = ring.items[ring.head];

        if !consume(&event) {
            break;
        }

        ring.head = (ring.head + 1) % MAX_HOOK_EVENTS;
        ring.len -= 1;
        copied += 1;
    }

    copied
}

#[cfg(test)]
mod tests {
    use super::*;

    fn event(id: u64) -> HookEvent {
        HookEvent {
            id,
            tsc: 0,
            cpu: 0,
            kind: 0,
            va: 0,
            rip: 0,
            rax: 0,
            rcx: 0,
            rdx: 0,
            r8: 0,
            r9: 0,
        }
    }

    #[test]
    fn record_is_eighty_bytes() {
        assert_eq!(core::mem::size_of::<HookEvent>(), 80);
    }

    /// The ring is global, so keep every ring-mutating assertion in one test;
    /// the harness runs tests in parallel.
    #[test]
    fn ring_order_and_overflow() {
        let _ = drain_events(MAX_HOOK_EVENTS, |_| true);

        push_event(event(1));
        push_event(event(2));

        let mut seen = [0u64; 2];
        let mut index = 0;
        let copied = drain_events(8, |event| {
            seen[index] = event.id;
            index += 1;
            true
        });

        assert_eq!(copied, 2);
        assert_eq!(seen, [1, 2]);

        for id in 0..(MAX_HOOK_EVENTS as u64 + 3) {
            push_event(event(id));
        }

        let mut first = None;
        let copied = drain_events(1, |event| {
            first = Some(event.id);
            true
        });

        // The oldest three were dropped when the ring wrapped.
        assert_eq!(copied, 1);
        assert_eq!(first, Some(3));
    }
}
