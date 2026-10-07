use std::{
    mem::transmute,
    sync::{
        Mutex, MutexGuard,
        atomic::{AtomicUsize, Ordering},
    },
};

use crate::{
    GLOBAL_TRIM_LOCK, RSMallocError,
    backend::page_allocator::PAGE_ALLOCATOR,
    big_allocations::segmented_bitmap::SEGMENTED_BITMAP_BACKEND,
    inner::{fallback::fallback_reinit_on_fork, preload::libc_int::pthread_atfork},
    internals::{big_meta_map::BIG_MAP, lock::SpinLockGuard, radix_tree::RADIX},
    rseq_core::rseq_offsets::__rseq_size,
};
use crate::{rseq_core::rseq_offsets::__rseq_offset, traits::Lock};

pub static BOOTSTRAP_LOCK: Mutex<()> = Mutex::new(());
static FORK_PROCESS_ID: AtomicUsize = AtomicUsize::new(0);

pub(crate) fn current_tid() -> usize {
    unsafe { syscalls::syscall!(syscalls::Sysno::gettid).unwrap_or_else(|_| std::process::abort()) }
}

fn current_pid() -> usize {
    unsafe { syscalls::syscall!(syscalls::Sysno::getpid).unwrap_or_else(|_| std::process::abort()) }
}

#[cold]
pub(crate) fn can_borrow_frozen_lock(owner_tid: usize) -> bool {
    let tid = current_tid();
    if tid == owner_tid {
        return true;
    }

    let prepared_pid = FORK_PROCESS_ID.load(Ordering::Acquire);
    if prepared_pid == 0 {
        return false;
    }
    // Older child callbacks run before ours. Linux fork makes the surviving
    // thread the child group leader; only that thread may borrow reservations.
    let pid = current_pid();
    pid != prepared_pid && tid == pid
}
static mut ATFORK_GUARD: Option<MutexGuard<'static, ()>> = None;
static mut TRIM_ATFORK_GUARD: Option<SpinLockGuard<()>> = None;

unsafe extern "C" fn fork_prepare() {
    let guard = BOOTSTRAP_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    ATFORK_GUARD = Some(transmute::<MutexGuard<'_, ()>, MutexGuard<'static, ()>>(
        guard,
    ));
    let owner_tid = current_tid();
    FORK_PROCESS_ID.store(current_pid(), Ordering::Release);
    TRIM_ATFORK_GUARD = Some(GLOBAL_TRIM_LOCK.lock());

    SEGMENTED_BITMAP_BACKEND.lock_all_for_fork(owner_tid);
    BIG_MAP.lock_for_fork(owner_tid);
    RADIX.lock_for_fork(owner_tid);
    PAGE_ALLOCATOR.lock_all_for_fork(owner_tid);
}

unsafe extern "C" fn fork_parent() {
    PAGE_ALLOCATOR.reset_locks_on_fork();
    RADIX.reset_lock_on_fork();
    BIG_MAP.reset_lock_on_fork();
    SEGMENTED_BITMAP_BACKEND.reset_locks_on_fork();

    if let Some(guard) = TRIM_ATFORK_GUARD.take() {
        drop(guard);
    }
    FORK_PROCESS_ID.store(0, Ordering::Release);
    if let Some(guard) = ATFORK_GUARD.take() {
        drop(guard);
    }
}

unsafe extern "C" fn fork_child() {
    PAGE_ALLOCATOR.reset_locks_on_fork();
    RADIX.reset_lock_on_fork();
    BIG_MAP.reset_lock_on_fork();
    SEGMENTED_BITMAP_BACKEND.reset_locks_on_fork();
    fallback_reinit_on_fork();
    if let Some(guard) = TRIM_ATFORK_GUARD.take() {
        drop(guard);
    }

    {
        use std::sync::atomic::Ordering;

        crate::backend::background_thread::RECLAIM_GUARD.store(false, Ordering::Relaxed);
    }

    if __rseq_size == 0 || __rseq_offset == 0 {
        RSMallocError::RseqUnavailable.log_and_abort();
    }
    FORK_PROCESS_ID.store(0, Ordering::Release);
    if let Some(guard) = ATFORK_GUARD.take() {
        drop(guard);
    }
}

pub unsafe fn register_fork_handlers() {
    let _ = pthread_atfork(Some(fork_prepare), Some(fork_parent), Some(fork_child));
}
