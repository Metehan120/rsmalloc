#[cfg(not(feature = "preload"))]
use std::sync::atomic::Ordering::{Acquire, Relaxed, Release};

#[cfg(not(feature = "preload"))]
use crate::backend::background_thread::{BACKGROUND_THREAD, maybe_unpark};

#[cfg(not(feature = "preload"))]
pub(crate) fn post_trim_request(size: usize) -> bool {
    let (requested, requested_size) = BACKGROUND_THREAD.trim_request.get_trim();
    if requested.load(Acquire) {
        return false;
    }

    let is_ok = requested
        .compare_exchange(false, true, Release, Relaxed)
        .is_ok();
    if is_ok {
        requested_size.store(size, Relaxed);
        maybe_unpark();
    }
    is_ok
}

#[cfg(feature = "preload")]
pub mod abi;
#[deprecated = "please use new v2 API"]
#[cfg(not(feature = "preload"))]
pub mod global_alloc;
#[cfg(not(feature = "preload"))]
pub mod global_alloc2;
