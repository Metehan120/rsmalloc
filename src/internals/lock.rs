use std::{
    hint::spin_loop,
    sync::atomic::{AtomicBool, Ordering},
};

#[cfg(feature = "debug-exact")]
use crate::{GLOBAL_LOCK_RETRIES, GLOBAL_LOCKS};

#[repr(transparent)]
pub struct LockGuard(*const AtomicBool);

impl Drop for LockGuard {
    #[inline(always)]
    fn drop(&mut self) {
        unsafe {
            (*self.0).store(false, Ordering::Release);
        };
    }
}

#[repr(align(64))]
pub struct SerialLock {
    state: AtomicBool,
}

impl SerialLock {
    #[inline(always)]
    pub const fn new() -> Self {
        SerialLock {
            state: AtomicBool::new(false),
        }
    }

    #[inline(always)]
    pub fn lock(&self) -> LockGuard {
        #[cfg(feature = "debug-exact")]
        GLOBAL_LOCKS.fetch_add(1, Ordering::Relaxed);

        // Acquire the lock
        while self
            .state
            .compare_exchange(false, true, Ordering::Acquire, Ordering::Relaxed)
            .is_err()
        {
            #[cfg(feature = "debug-exact")]
            GLOBAL_LOCK_RETRIES.fetch_add(1, Ordering::Relaxed);
            spin_loop();
        }

        LockGuard(&self.state as *const AtomicBool)
    }

    #[inline(always)]
    pub fn spin_until_unlock(&self) {
        while self.get_lock() {
            spin_loop();
        }
    }

    #[inline(always)]
    pub fn get_lock(&self) -> bool {
        self.state.load(Ordering::Relaxed)
    }
}
