use std::ptr::null_mut;

use rustix::rand::{GetRandomFlags, getrandom};

use crate::{
    BIG_MAGIC, FREED_MAGIC, IS_RSEQ_INTERNAL, MAGIC, RS_DISABLE_THP, RSMallocError,
    big_allocations::buddy::BIG_BUDDY_ALLOCATOR,
    core_prim::{
        fork::register_fork_handlers,
        predictor::{EMA_ALPHA, PREDICTOR_INIT_BATCH},
        rseq_register::register_rseq_raw,
    },
    internals::{
        env::{get_env_f32, get_env_usize},
        l3_main_radix::{L3_RADIX, RadixTree},
    },
    rseq_core::{
        rseq_cache::RSEQ_CACHE,
        rseq_main::{__rseq_offset, __rseq_size},
    },
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
    if register_rseq_raw() == 0 {
        IS_RSEQ_INTERNAL = true;
    } else {
        if __rseq_offset.is_null() || __rseq_size.is_null() {
            RSMallocError::RSEQRegFailed.log_and_abort(
                null_mut(),
                "RSEQ register failed, cannot initialize rseq cache. No kernel RSEQ support",
                None,
            );
        }
    };

    EMA_ALPHA = get_env_f32("RS_EMA_ALPHA".as_bytes()).unwrap_or(0.15);
    let predictor = get_env_usize("RS_PREDICTOR_INIT_BATCH".as_bytes()).unwrap_or(8);
    PREDICTOR_INIT_BATCH = predictor;

    L3_RADIX = RadixTree::new();
    RSEQ_CACHE.ensure_cache();
    let cache_size = get_env_usize("RS_BUDDY_PER_CACHE_SIZE".as_bytes())
        .unwrap_or(1024 * 1024 * 256)
        .clamp(1024 * 1024 * 256, 2 << 46)
        .next_power_of_two();

    let thp = get_env_usize("RS_BUDDY_ATTEMPT_HUGEPAGE".as_bytes()).unwrap_or(0) != 0;

    let disable_thp = get_env_usize("RS_DISABLE_THP".as_bytes()).unwrap_or(0);
    RS_DISABLE_THP = disable_thp == 1;

    BIG_BUDDY_ALLOCATOR.init(cache_size, thp && !RS_DISABLE_THP);

    register_fork_handlers();

    let random_magic = get_env_usize("RS_DISABLE_RANDOMIZING".as_bytes()).unwrap_or(0) == 0;
    if random_magic {
        init_magic()
    };
}
