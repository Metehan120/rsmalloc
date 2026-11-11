use crate::backend::bootstrap::{BootstrapConfig, main_bootstrap};
use crate::backend::page_allocator::ARENA_SIZE;
use crate::{core_prim::predictor::DEFAULT_BATCH, internals::env::get_env_usize};

#[inline(never)]
pub unsafe fn bootstrap() {
    let arena_size = get_env_usize("RS_ARENA_SIZE".as_bytes()).unwrap_or(ARENA_SIZE);
    let max_refill = get_env_usize("RS_MAX_REFILL_RETRIES".as_bytes()).unwrap_or(3);

    let init_batch = get_env_usize("RS_PREDICTOR_INIT_BATCH".as_bytes()).unwrap_or(DEFAULT_BATCH);

    let segmented_bitmap_max = get_env_usize("RS_SEGMENTED_BITMAP_PER_CACHE_SIZE".as_bytes())
        .unwrap_or(1024 * 1024 * 64)
        .clamp(1024 * 1024 * 64, 2 << 46)
        .next_power_of_two();

    let attempt_huge =
        get_env_usize("RS_SEGMENTED_BITMAP_ATTEMPT_HUGEPAGE".as_bytes()).unwrap_or(0) != 0;
    let disable_trim = get_env_usize("RS_DISABLE_TRIM_THREAD".as_bytes()).unwrap_or(0) != 0;
    let trim_threshold =
        get_env_usize("RS_TRIMMER_THRESHOLD".as_bytes()).unwrap_or(1024 * 1024 * 10);
    let big_trim_threshold =
        get_env_usize("RS_BIG_TRIMMER_THRESHOLD".as_bytes()).unwrap_or(1024 * 1024 * 512);

    let disable_thp = get_env_usize("RS_DISABLE_THP".as_bytes()).unwrap_or(0) == 1;

    let random_magic = get_env_usize("RS_DISABLE_RANDOMIZING".as_bytes()).unwrap_or(0) == 0;
    let config = BootstrapConfig::new(
        arena_size,
        max_refill,
        init_batch,
        segmented_bitmap_max,
        attempt_huge,
        disable_trim,
        trim_threshold,
        big_trim_threshold,
        disable_thp,
        random_magic,
        false,
    );

    main_bootstrap(config);
}
