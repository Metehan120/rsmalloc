use crate::backend::bootstrap::{BootstrapConfig, main_bootstrap};
use crate::backend::page_allocator::ARENA_SIZE;
use crate::{
    backend::trim::BUDDY_DISABLE_PERCENTAGE, core_prim::predictor::DEFAULT_BATCH,
    internals::env::get_env_usize,
};

#[inline(never)]
pub unsafe fn bootstrap() {
    let arena_size = get_env_usize("RS_ARENA_SIZE".as_bytes())
        .unwrap_or(ARENA_SIZE)
        .max(256 * 1024);

    let max_refill = get_env_usize("RS_MAX_REFILL_RETRIES".as_bytes()).unwrap_or(3);

    let init_batch = get_env_usize("RS_PREDICTOR_INIT_BATCH".as_bytes()).unwrap_or(DEFAULT_BATCH);

    let buddy_max = get_env_usize("RS_BUDDY_PER_CACHE_SIZE".as_bytes())
        .unwrap_or(1024 * 1024 * 64)
        .clamp(1024 * 1024 * 64, 2 << 46)
        .next_power_of_two();

    let attempt_huge = get_env_usize("RS_BUDDY_ATTEMPT_HUGEPAGE".as_bytes()).unwrap_or(0) != 0;
    let disable_trim = get_env_usize("RS_DISABLE_TRIM_THREAD".as_bytes()).unwrap_or(0) != 0;
    let trim_threshold =
        get_env_usize("RS_TRIMMER_THRESHOLD".as_bytes()).unwrap_or(1024 * 1024 * 10);

    let disable_relief = get_env_usize("RS_ENABLE_RELIEF".as_bytes()).unwrap_or(1) != 0;
    let disable_percentage = get_env_usize("RS_BUDDY_RELIEF_DISABLE_PERCENTAGE".as_bytes())
        .unwrap_or(85)
        .min(100);
    let enable_percentage = get_env_usize("RS_BUDDY_RELIEF_ENABLE_PERCENTAGE".as_bytes())
        .unwrap_or(80)
        .min(BUDDY_DISABLE_PERCENTAGE);

    let disable_thp = get_env_usize("RS_DISABLE_THP".as_bytes()).unwrap_or(0) == 1;

    let random_magic = get_env_usize("RS_DISABLE_RANDOMIZING".as_bytes()).unwrap_or(0) == 0;
    let config = BootstrapConfig::new(
        arena_size,
        max_refill,
        init_batch,
        buddy_max,
        attempt_huge,
        disable_trim,
        trim_threshold,
        disable_relief,
        disable_percentage,
        enable_percentage,
        disable_thp,
        random_magic,
        false,
    );

    main_bootstrap(config);
}
