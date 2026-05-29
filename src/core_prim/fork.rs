use std::{
    mem::transmute,
    sync::{Mutex, MutexGuard},
};

use crate::inner::{fallback::fallback_reinit_on_fork, libc_int::pthread_atfork};

pub static BOOTSTRAP_LOCK: Mutex<()> = Mutex::new(());
static mut ATFORK_GUARD: Option<MutexGuard<'static, ()>> = None;

unsafe extern "C" fn fork_prepare() {
    let guard = BOOTSTRAP_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    ATFORK_GUARD = Some(transmute::<MutexGuard<'_, ()>, MutexGuard<'static, ()>>(
        guard,
    ));
}

unsafe extern "C" fn fork_parent() {
    if let Some(guard) = ATFORK_GUARD.take() {
        drop(guard);
    }
}

unsafe extern "C" fn fork_child() {
    if let Some(guard) = ATFORK_GUARD.take() {
        drop(guard);
    }
    fallback_reinit_on_fork();
}

pub unsafe fn register_fork_handlers() {
    let _ = pthread_atfork(Some(fork_prepare), Some(fork_parent), Some(fork_child));
}
