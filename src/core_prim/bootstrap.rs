use std::ptr::null_mut;

#[cfg(feature = "debug")]
use crate::START_TIME;
use crate::backend::page_allocator::ARENA_SIZE;
use crate::core_prim::random::{init_align, init_magic};
use crate::get_clock;
use crate::rseq_core::rseq_offsets::__rseq_offset;
use crate::{
    ALIGN_TAG, BUDDY_ATTEMPT_HUGE, BUDDY_MAX_CACHE, DISABLE_TRIM_THREAD, RS_DISABLE_THP,
    RSMallocError, TRIM_THRESHOLD,
    big_allocations::buddy::BUDDY_BACKEND,
    core_prim::predictor::{DEFAULT_BATCH, PREDICTOR_INIT_BATCH},
    inner::alloc::MAX_REFILL_RETRIES,
    internals::{
        env::get_env_usize,
        radix_tree::{RADIX, RadixTree},
    },
    rseq_core::{rseq_offsets::__rseq_size, slab_cache::SLAB_CACHE},
    trim::{BUDDY_DISABLE_PERCENTAGE, BUDDY_ENABLE_PERCENTAGE, DISABLE_RELIEF},
};

#[inline(never)]
pub unsafe fn bootstrap() {
    if __rseq_size == 0 || __rseq_offset == 0 {
        RSMallocError::RSEQRegFailed.log_and_abort(
            null_mut(),
            "RSEQ register failed, cannot initialize rseq cache.",
            None,
        );
    }

    #[cfg(feature = "debug")]
    {
        START_TIME = Some(std::time::Instant::now());
    }

    get_clock();

    ARENA_SIZE = get_env_usize("RS_ARENA_SIZE".as_bytes())
        .unwrap_or(ARENA_SIZE)
        .max(256 * 1024);

    MAX_REFILL_RETRIES = get_env_usize("RS_MAX_REFILL_RETRIES".as_bytes()).unwrap_or(3);

    let predictor = get_env_usize("RS_PREDICTOR_INIT_BATCH".as_bytes()).unwrap_or(DEFAULT_BATCH);
    PREDICTOR_INIT_BATCH = predictor;

    RADIX = RadixTree::new();
    SLAB_CACHE.ensure_cache();
    BUDDY_MAX_CACHE = get_env_usize("RS_BUDDY_PER_CACHE_SIZE".as_bytes())
        .unwrap_or(1024 * 1024 * 64)
        .clamp(1024 * 1024 * 64, 2 << 46)
        .next_power_of_two();

    BUDDY_ATTEMPT_HUGE = get_env_usize("RS_BUDDY_ATTEMPT_HUGEPAGE".as_bytes()).unwrap_or(0) != 0;
    DISABLE_TRIM_THREAD = get_env_usize("RS_DISABLE_TRIM_THREAD".as_bytes()).unwrap_or(0) != 0;
    TRIM_THRESHOLD = get_env_usize("RS_TRIMMER_THRESHOLD".as_bytes()).unwrap_or(1024 * 1024 * 10);

    DISABLE_RELIEF = get_env_usize("RS_ENABLE_RELIEF".as_bytes()).unwrap_or(1) != 0;
    BUDDY_DISABLE_PERCENTAGE = get_env_usize("RS_BUDDY_RELIEF_DISABLE_PERCENTAGE".as_bytes())
        .unwrap_or(85)
        .min(100);
    BUDDY_ENABLE_PERCENTAGE = get_env_usize("RS_BUDDY_RELIEF_ENABLE_PERCENTAGE".as_bytes())
        .unwrap_or(80)
        .min(BUDDY_DISABLE_PERCENTAGE);

    let disable_thp = get_env_usize("RS_DISABLE_THP".as_bytes()).unwrap_or(0);
    RS_DISABLE_THP = disable_thp == 1;

    BUDDY_BACKEND.init(BUDDY_MAX_CACHE, BUDDY_ATTEMPT_HUGE && !RS_DISABLE_THP);

    crate::core_prim::fork::register_fork_handlers();

    let random_magic = get_env_usize("RS_DISABLE_RANDOMIZING".as_bytes()).unwrap_or(0) == 0;
    if random_magic {
        init_magic();
        init_align();

        if ALIGN_TAG == 0 {
            while ALIGN_TAG == 0 {
                init_align();
            }
        }
    };
}
