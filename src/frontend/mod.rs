use std::sync::atomic::Ordering::Relaxed;

use crate::backend::background_thread::maybe_unpark;

pub(crate) fn post_trim_request(size: usize) -> bool {
    let (requested, requested_size) = crate::TRIM_MESSAGE.get_trim();
    if requested.load(Relaxed) {
        return false;
    }

    requested_size.store(size, Relaxed);
    let is_ok = !requested.swap(true, Relaxed);
    if is_ok {
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
