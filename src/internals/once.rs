use std::{
    hint::{likely, spin_loop},
    sync::atomic::{AtomicU8, Ordering},
};

pub struct Once {
    state: AtomicU8,
}

// Use Spinlock-based Once implementation for better Fork Safety
impl Once {
    pub const fn new() -> Self {
        Self {
            state: AtomicU8::new(0),
        }
    }

    #[inline(always)]
    pub fn call_once<F>(&self, f: F)
    where
        F: FnOnce(),
    {
        if likely(self.state.load(Ordering::Relaxed) == 2) {
            return;
        }

        self.start(f);
    }

    #[inline(never)]
    fn start<F>(&self, f: F)
    where
        F: FnOnce(),
    {
        if self
            .state
            .compare_exchange(0, 1, Ordering::Acquire, Ordering::Relaxed)
            .is_ok()
        {
            f();
            self.state.store(2, Ordering::Release);
        }

        while self.state.load(Ordering::Acquire) != 2 {
            spin_loop();
        }
    }

    #[cfg(feature = "preload")]
    pub fn reset_at_fork_oncelock(&self) {
        self.state.store(0, Ordering::Relaxed);
    }
}
