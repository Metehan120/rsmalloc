#[cfg(not(feature = "preload"))]
pub use super::lock::SpinLock;

#[cfg(feature = "preload")]
pub use preload::ForkLock as SpinLock;

#[cfg(feature = "preload")]
mod preload {
    use std::{
        cell::UnsafeCell,
        hint::spin_loop,
        marker::PhantomData,
        ops::{Deref, DerefMut},
        sync::atomic::{AtomicUsize, Ordering},
    };

    #[cfg(feature = "debug-exact")]
    use crate::{GLOBAL_LOCK_RETRIES, GLOBAL_LOCKS, GLOBAL_SPIN_WAITS, GLOBAL_TRY_LOCK_MISSES};
    use crate::{
        core_prim::fork::can_borrow_frozen_lock, internals::lock::LockGuard, traits::Lock,
    };

    const HELD: usize = 1;
    const FROZEN: usize = 2;
    const OWNER_SHIFT: usize = 2;

    pub struct ForkLock<T> {
        // Frozen states encode the prepare thread's TID, with HELD marking a
        // temporary callback guard. A frozen reservation itself borrows no data.
        state: AtomicUsize,
        data: UnsafeCell<T>,
    }

    unsafe impl<T: Send> Send for ForkLock<T> {}
    unsafe impl<T: Send> Sync for ForkLock<T> {}

    pub struct ForkLockGuard<'a, T> {
        state: &'a AtomicUsize,
        data: &'a mut T,
        release_state: usize,
        _not_send: PhantomData<*mut ()>,
    }

    impl<T> Drop for ForkLockGuard<'_, T> {
        fn drop(&mut self) {
            self.state.store(self.release_state, Ordering::Release);
        }
    }

    impl<T> Deref for ForkLockGuard<'_, T> {
        type Target = T;

        fn deref(&self) -> &T {
            self.data
        }
    }

    impl<T> DerefMut for ForkLockGuard<'_, T> {
        fn deref_mut(&mut self) -> &mut T {
            self.data
        }
    }

    impl<T> ForkLock<T> {
        pub const fn new(data: T) -> Self {
            Self {
                state: AtomicUsize::new(0),
                data: UnsafeCell::new(data),
            }
        }

        pub fn freeze_for_fork(&self, owner_tid: usize) {
            if owner_tid == 0 || owner_tid > usize::MAX >> OWNER_SHIFT {
                std::process::abort();
            }
            let frozen = (owner_tid << OWNER_SHIFT) | FROZEN;
            while self
                .state
                .compare_exchange(0, frozen, Ordering::Acquire, Ordering::Relaxed)
                .is_err()
            {
                spin_loop();
            }
        }

        pub fn thaw_after_fork(&self) {
            let state = self.state.load(Ordering::Acquire);
            if state & FROZEN == 0
                || state & HELD != 0
                || !can_borrow_frozen_lock(state >> OWNER_SHIFT)
                || self
                    .state
                    .compare_exchange(state, 0, Ordering::Release, Ordering::Relaxed)
                    .is_err()
            {
                std::process::abort();
            }
        }

        #[inline(always)]
        fn acquire(&self, blocking: bool) -> Option<usize> {
            match self
                .state
                .compare_exchange(0, HELD, Ordering::Acquire, Ordering::Relaxed)
            {
                Ok(_) => Some(0),
                Err(state) if state & FROZEN != 0 => self.borrow_frozen(state, blocking),
                Err(_) => None,
            }
        }

        #[cold]
        fn borrow_frozen(&self, state: usize, blocking: bool) -> Option<usize> {
            if !can_borrow_frozen_lock(state >> OWNER_SHIFT) {
                return None;
            }
            if state & HELD != 0 {
                // Do not turn a fork reservation into a generally recursive
                // mutex: a second live data guard would violate exclusivity.
                if blocking {
                    std::process::abort();
                }
                return None;
            }
            self.state
                .compare_exchange(state, state | HELD, Ordering::Acquire, Ordering::Relaxed)
                .ok()
        }

        unsafe fn guard(&self, release_state: usize) -> ForkLockGuard<'_, T> {
            ForkLockGuard {
                state: &self.state,
                data: unsafe { &mut *self.data.get() },
                release_state,
                _not_send: PhantomData,
            }
        }
    }

    impl<T> Lock for ForkLock<T> {
        type Out = T;
        type LockError<Guard> = LockGuard<Guard>;
        type LockState = bool;
        type Guard<'a, U>
            = ForkLockGuard<'a, U>
        where
            Self: 'a,
            U: 'a;

        #[inline(always)]
        fn lock(&self) -> Self::Guard<'_, T> {
            #[cfg(feature = "debug-exact")]
            GLOBAL_LOCKS.fetch_add(1, Ordering::Relaxed);

            loop {
                if let Some(release_state) = self.acquire(true) {
                    return unsafe { self.guard(release_state) };
                }
                #[cfg(feature = "debug-exact")]
                GLOBAL_LOCK_RETRIES.fetch_add(1, Ordering::Relaxed);
                spin_loop();
            }
        }

        #[inline(always)]
        fn try_lock(&self) -> LockGuard<Self::Guard<'_, T>> {
            #[cfg(feature = "debug-exact")]
            GLOBAL_LOCKS.fetch_add(1, Ordering::Relaxed);

            if let Some(release_state) = self.acquire(false) {
                LockGuard::Free(unsafe { self.guard(release_state) })
            } else {
                #[cfg(feature = "debug-exact")]
                GLOBAL_TRY_LOCK_MISSES.fetch_add(1, Ordering::Relaxed);
                LockGuard::Locked
            }
        }

        fn spin_until_unlock(&self) {
            while self.get_lock() {
                #[cfg(feature = "debug-exact")]
                GLOBAL_SPIN_WAITS.fetch_add(1, Ordering::Relaxed);
                spin_loop();
            }
        }

        fn get_lock(&self) -> bool {
            self.state.load(Ordering::Acquire) != 0
        }

        fn unlock(&self) {
            if self
                .state
                .compare_exchange(HELD, 0, Ordering::Release, Ordering::Relaxed)
                .is_err()
            {
                std::process::abort();
            }
        }
    }

    #[cfg(test)]
    mod tests {
        use super::*;
        use crate::core_prim::fork::current_tid;
        use std::sync::Arc;

        #[test]
        fn ordinary_guard_releases_lock() {
            let lock = ForkLock::new(7usize);
            {
                let mut guard = lock.lock();
                *guard = 9;
                assert!(matches!(lock.try_lock(), LockGuard::Locked));
            }
            assert!(!lock.get_lock());
            assert_eq!(*lock.lock(), 9);
        }

        #[test]
        fn callback_guard_preserves_fork_reservation() {
            let lock = ForkLock::new(7usize);
            lock.freeze_for_fork(current_tid());
            {
                let mut guard = lock.lock();
                *guard = 9;
                assert!(matches!(lock.try_lock(), LockGuard::Locked));
            }
            assert!(lock.get_lock());
            let LockGuard::Free(guard) = lock.try_lock() else {
                panic!("fork owner should be able to borrow its reservation");
            };
            assert_eq!(*guard, 9);
            drop(guard);
            assert!(lock.get_lock());
            lock.thaw_after_fork();
            assert!(!lock.get_lock());
        }

        #[test]
        fn another_thread_cannot_borrow_fork_reservation() {
            let lock = Arc::new(ForkLock::new(7usize));
            lock.freeze_for_fork(current_tid());
            let other = Arc::clone(&lock);
            std::thread::spawn(move || {
                assert!(matches!(other.try_lock(), LockGuard::Locked));
            })
            .join()
            .unwrap();
            assert!(lock.get_lock());
            lock.thaw_after_fork();
            let other = Arc::clone(&lock);
            std::thread::spawn(move || {
                *other.lock() = 9;
            })
            .join()
            .unwrap();
            assert_eq!(*lock.lock(), 9);
        }
    }
}
