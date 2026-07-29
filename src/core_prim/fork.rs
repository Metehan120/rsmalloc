use std::{
    mem::transmute,
    ptr::null_mut,
    sync::{Mutex, MutexGuard},
};

use crate::rseq_core::rseq_offsets::__rseq_offset;
use crate::{
    GLOBAL_TRIM_LOCK, RSMallocError,
    big_allocations::buddy::BUDDY_BACKEND,
    inner::{fallback::fallback_reinit_on_fork, libc_int::pthread_atfork},
    internals::{lock::LockGuard, rbtree::BIG_MAP},
    rseq_core::rseq_offsets::__rseq_size,
};

pub static BOOTSTRAP_LOCK: Mutex<()> = Mutex::new(());
static mut ATFORK_GUARD: Option<MutexGuard<'static, ()>> = None;
static mut TRIM_ATFORK_GUARD: Option<LockGuard> = None;

unsafe extern "C" fn fork_prepare() {
    let guard = BOOTSTRAP_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    ATFORK_GUARD = Some(transmute::<MutexGuard<'_, ()>, MutexGuard<'static, ()>>(
        guard,
    ));
    TRIM_ATFORK_GUARD = Some(GLOBAL_TRIM_LOCK.lock());
}

unsafe extern "C" fn fork_parent() {
    if let Some(guard) = TRIM_ATFORK_GUARD.take() {
        drop(guard);
    }
    if let Some(guard) = ATFORK_GUARD.take() {
        drop(guard);
    }
}

unsafe extern "C" fn fork_child() {
    if let Some(guard) = TRIM_ATFORK_GUARD.take() {
        drop(guard);
    }

    if let Some(guard) = ATFORK_GUARD.take() {
        drop(guard);
    }

    fallback_reinit_on_fork();
    BUDDY_BACKEND.reset_locks_on_fork();
    BIG_MAP.reset_lock_on_fork();
    GLOBAL_TRIM_LOCK.reset_at_fork();

    {
        use std::sync::atomic::Ordering;

        crate::inner::alloc::TRIM_GUARD.store(false, Ordering::Relaxed);
    }

    if __rseq_size == 0 || __rseq_offset == 0 {
        RSMallocError::RSEQRegFailed.log_and_abort(
            null_mut(),
            "RSEQ register failed, cannot initialize rseq cache.",
            None,
        );
    }
}

pub unsafe fn register_fork_handlers() {
    let _ = pthread_atfork(Some(fork_prepare), Some(fork_parent), Some(fork_child));
}
