use std::{
    cell::UnsafeCell,
    hint::spin_loop,
    ops::{Deref, DerefMut},
    sync::atomic::{
        AtomicBool,
        Ordering::{self, Acquire},
    },
};

use crate::traits::Lock;
#[cfg(feature = "debug-exact")]
use crate::{GLOBAL_LOCK_RETRIES, GLOBAL_LOCKS, GLOBAL_SPIN_WAITS, GLOBAL_TRY_LOCK_MISSES};

#[derive(Debug, PartialEq)]
pub enum LockState {
    Locked,
    Free,
}

#[derive(Debug, PartialEq)]
pub enum LockGuard<G> {
    Locked,
    Free(G),
}

pub struct SpinLockGuard<'a, T> {
    lock: &'a AtomicBool,
    data: &'a mut T,
}

impl<'a, T> SpinLockGuard<'a, T> {
    #[inline(always)]
    pub const fn new(lock: &'a AtomicBool, data: &'a mut T) -> Self {
        Self { lock, data }
    }
}

impl<T> Drop for SpinLockGuard<'_, T> {
    #[inline(always)]
    fn drop(&mut self) {
        self.lock.store(false, Ordering::Release);
    }
}

impl<T> Deref for SpinLockGuard<'_, T> {
    type Target = T;
    #[inline(always)]
    fn deref(&self) -> &Self::Target {
        self.data
    }
}

impl<T> DerefMut for SpinLockGuard<'_, T> {
    #[inline(always)]
    fn deref_mut(&mut self) -> &mut Self::Target {
        self.data
    }
}

pub struct SpinLock<T> {
    state: AtomicBool,
    data: UnsafeCell<T>,
}

unsafe impl<T: Send> Send for SpinLock<T> {}
unsafe impl<T: Sync> Sync for SpinLock<T> {}

impl<T> SpinLock<T> {
    #[inline(always)]
    pub const fn new(data: T) -> Self {
        Self {
            state: AtomicBool::new(false),
            data: UnsafeCell::new(data),
        }
    }
}

impl<T> Lock for SpinLock<T> {
    type LockError<Guard> = LockGuard<Guard>;
    type LockState = LockState;
    type Out = T;

    type Guard<'a, U>
        = SpinLockGuard<'a, U>
    where
        Self: 'a,
        U: 'a;

    #[inline(always)]
    fn lock(&self) -> Self::Guard<'_, Self::Out> {
        #[cfg(feature = "debug-exact")]
        GLOBAL_LOCKS.fetch_add(1, Ordering::Relaxed);

        while self
            .state
            .compare_exchange(false, true, Ordering::Acquire, Ordering::Relaxed)
            .is_err()
        {
            #[cfg(feature = "debug-exact")]
            GLOBAL_LOCK_RETRIES.fetch_add(1, Ordering::Relaxed);
            spin_loop();
        }

        SpinLockGuard::new(&self.state, unsafe { &mut *self.data.get() })
    }

    #[inline(always)]
    fn try_lock(&self) -> LockGuard<Self::Guard<'_, Self::Out>> {
        #[cfg(feature = "debug-exact")]
        GLOBAL_LOCKS.fetch_add(1, Ordering::Relaxed);

        match self
            .state
            .compare_exchange(false, true, Ordering::Acquire, Ordering::Relaxed)
        {
            Ok(_) => LockGuard::Free(SpinLockGuard::new(&self.state, unsafe {
                &mut *self.data.get()
            })),
            Err(_) => {
                #[cfg(feature = "debug-exact")]
                GLOBAL_TRY_LOCK_MISSES.fetch_add(1, Ordering::Relaxed);
                LockGuard::Locked
            }
        }
    }

    #[inline(always)]
    fn spin_until_unlock(&self) {
        while self.state.load(Ordering::Acquire) {
            #[cfg(feature = "debug-exact")]
            GLOBAL_SPIN_WAITS.fetch_add(1, Ordering::Relaxed);
            spin_loop();
        }
    }

    #[inline(always)]
    fn get_lock(&self) -> Self::LockState {
        if self.state.load(Acquire) == true {
            return LockState::Locked;
        }
        LockState::Free
    }
}

impl<T> SpinLock<T> {
    #[cfg(feature = "preload")]
    pub fn reset_at_fork(&self) {
        self.state.store(false, Ordering::Relaxed);
    }
}
