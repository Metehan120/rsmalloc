use std::{
    hint::spin_loop,
    sync::atomic::{AtomicBool, Ordering},
};

pub struct LockGuard(*const AtomicBool);

impl Drop for LockGuard {
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
        // Acquire the lock
        while self
            .state
            .compare_exchange(false, true, Ordering::Acquire, Ordering::Relaxed)
            .is_err()
        {
            spin_loop();
        }

        let guard = LockGuard(&self.state as *const AtomicBool);
        guard
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

    #[inline(always)]
    pub fn unlock(&self) {
        self.state.store(false, Ordering::Release);
    }

    pub fn reset_on_fork(&self) {
        self.unlock();
    }
}
