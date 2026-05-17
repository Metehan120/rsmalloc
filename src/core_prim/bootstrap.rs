use std::ptr::null_mut;

use rustix::rand::{GetRandomFlags, getrandom};

use crate::{
    BIG_MAGIC, FREED_MAGIC, MAGIC, MAX_BIG_CACHE, RSMallocError,
    internals::{
        env::get_env_usize,
        l3_main_radix::{L3_RADIX, RadixTree},
    },
    rseq_core::rseq_cache::RSEQ_CACHE,
};

#[inline(never)]
unsafe fn init_magic() {
    let mut main = 0u16.to_le_bytes();

    match getrandom(&mut main, GetRandomFlags::empty()) {
        Ok(_) => {
            let small = u16::from_le_bytes(main);
            let medium = small.wrapping_sub(1);
            let large = medium.wrapping_sub(1);

            MAGIC = small;
            FREED_MAGIC = medium;
            BIG_MAGIC = large;
        }
        Err(err) => RSMallocError::SecurityViolation.log_and_abort(
            null_mut(),
            "calling getrandom failed, cannot initialize magic",
            Some(err.raw_os_error()),
        ),
    };
}

#[inline(never)]
pub unsafe fn bootstrap() {
    L3_RADIX = unsafe { RadixTree::new() };
    RSEQ_CACHE.ensure_cache();
    MAX_BIG_CACHE = get_env_usize("RS_MAX_BIG_CACHE".as_bytes()).unwrap_or(1024 * 1024 * 256);
    init_magic();
}
