//! Interrupt-safe mutex.
//!
//! A plain [`spin::Mutex`] deadlocks when the CPU that holds it takes an
//! interrupt whose handler locks the same mutex: the interrupted context can
//! never make progress, so the handler spins forever. This mutex disables local
//! interrupts before spinning and restores the previous IF state once the guard
//! is dropped.

use core::ops::{Deref, DerefMut};

use x86_64::instructions::interrupts;

/// Mutual exclusion that keeps local interrupts off while the guard is alive.
pub struct Mutex<T: ?Sized> {
    inner: spin::Mutex<T>,
}

impl<T> Mutex<T> {
    pub const fn new(value: T) -> Self {
        Self {
            inner: spin::Mutex::new(value),
        }
    }

    pub fn lock(&self) -> MutexGuard<'_, T> {
        let restore_interrupts = interrupts::are_enabled();
        interrupts::disable();

        MutexGuard {
            guard: Some(self.inner.lock()),
            restore_interrupts,
        }
    }

    /// Non-blocking; for panic/exception paths that may already own the lock.
    #[allow(dead_code)]
    pub fn try_lock(&self) -> Option<MutexGuard<'_, T>> {
        let restore_interrupts = interrupts::are_enabled();
        interrupts::disable();

        match self.inner.try_lock() {
            Some(guard) => Some(MutexGuard {
                guard: Some(guard),
                restore_interrupts,
            }),
            None => {
                if restore_interrupts {
                    interrupts::enable();
                }
                None
            }
        }
    }
}

pub struct MutexGuard<'a, T: ?Sized> {
    guard: Option<spin::MutexGuard<'a, T>>,
    restore_interrupts: bool,
}

impl<T: ?Sized> Deref for MutexGuard<'_, T> {
    type Target = T;

    fn deref(&self) -> &T {
        self.guard.as_deref().unwrap()
    }
}

impl<T: ?Sized> DerefMut for MutexGuard<'_, T> {
    fn deref_mut(&mut self) -> &mut T {
        self.guard.as_deref_mut().unwrap()
    }
}

impl<T: ?Sized> Drop for MutexGuard<'_, T> {
    fn drop(&mut self) {
        drop(self.guard.take());

        if self.restore_interrupts {
            interrupts::enable();
        }
    }
}
