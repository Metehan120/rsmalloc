use std::{
    mem::transmute,
    ptr::null_mut,
    sync::{Mutex, MutexGuard},
};

#[cfg(not(feature = "legacy-glibc-support"))]
use crate::rseq_core::rseq_main::__rseq_offset;
use crate::{
    RSMallocError,
    inner::{fallback::fallback_reinit_on_fork, libc_int::pthread_atfork},
    internals::l3_main_radix::L3_RADIX,
    rseq_core::rseq_main::__rseq_size,
};

#[cfg(feature = "legacy-glibc-support")]
use crate::rseq_core::rseq_main::__rseq_offset;

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
    L3_RADIX.lock.reset_at_fork();

    #[cfg(feature = "legacy-glibc-support")]
    if __rseq_offset.is_null() || __rseq_size.is_null() {
        use crate::core_prim::rseq_register::register_rseq_raw;

        if register_rseq_raw() == 0 {
            crate::IS_RSEQ_INTERNAL = true;
        } else {
            RSMallocError::RSEQRegFailed.log_and_abort(
                null_mut(),
                "RSEQ register failed, cannot initialize rseq cache. No kernel RSEQ support",
                None,
            );
        }
    }

    #[cfg(not(feature = "legacy-glibc-support"))]
    if __rseq_size.is_null() || __rseq_offset.is_null() {
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
