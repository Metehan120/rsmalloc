use std::{cell::UnsafeCell, mem::MaybeUninit};

use crate::internals::once::Once;

pub struct OnceLock<T> {
    init: Once,
    value: UnsafeCell<MaybeUninit<T>>,
}

unsafe impl<T> Sync for OnceLock<T> {}
unsafe impl<T> Send for OnceLock<T> {}

// Use Spinlock-based OnceLock implementation for better Fork Safety
impl<T> OnceLock<T> {
    #[inline(always)]
    pub const fn new() -> Self {
        Self {
            init: Once::new(),
            value: UnsafeCell::new(MaybeUninit::uninit()),
        }
    }

    #[inline(always)]
    pub fn get_or_init<F>(&self, f: F) -> &T
    where
        F: FnOnce() -> T,
    {
        self.init.call_once(|| {
            let val = f();
            unsafe {
                (*self.value.get()).write(val);
            }
        });

        unsafe { &*(*self.value.get()).as_ptr() }
    }

    pub unsafe fn reset_at_fork(&self) {
        self.init.reset_at_fork_oncelock();
    }
}
