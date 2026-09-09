use crate::{
    backend::{
        page_allocator::{ARENA_SIZE, PAGE_ALLOCATOR},
        reclaimer::{
            DISABLE_RELIEF, SEGMENTED_BITMAP_DISABLE_PERCENTAGE, SEGMENTED_BITMAP_ENABLE_PERCENTAGE,
        },
    },
    big_allocations::segmented_bitmap::SEGMENTED_BITMAP_BACKEND,
    core_prim::{
        predictor::PREDICTOR_INIT_BATCH,
        random::{init_align, init_magic},
    },
    global_vals::{
        ALIGN_TAG, BIG_TRIM_THRESHOLD, DISABLE_TRIM_THREAD, RS_DISABLE_THP,
        SEGMENTED_BITMAP_ATTEMPT_HUGE, SEGMENTED_BITMAP_MAX_CACHE, SMALL_TRIM_THRESHOLD, get_clock,
    },
    inner::alloc::MAX_REFILL_RETRIES,
    internals::radix_tree::{RADIX, RadixTree},
    result_handling::RSMallocError,
    rseq_core::{
        rseq_offsets::{__rseq_offset, __rseq_size},
        slab_cache::SLAB_CACHE,
    },
};

pub struct BootstrapConfig {
    arena_size: usize,
    max_refill: usize,
    init_batch: usize,
    segmented_bitmap_max_cache: usize,
    segmented_bitmap_attempt_huge: bool,
    disable_trimmer: bool,
    small_trim_threshold: usize,
    big_trim_threshold: usize,
    disable_relief: bool,
    segmented_bitmap_disable_percentage: usize,
    segmented_bitmap_enable_percentage: usize,
    disable_thp: bool,
    random_magic: bool,
    foreign_pointer_abort: bool,
}

impl BootstrapConfig {
    #[allow(clippy::too_many_arguments)]
    pub const fn new(
        arena_size: usize,
        max_refill: usize,
        init_batch: usize,
        segmented_bitmap_max_cache: usize,
        segmented_bitmap_attempt_huge: bool,
        disable_trimmer: bool,
        small_trim_threshold: usize,
        big_trim_threshold: usize,
        disable_relief: bool,
        segmented_bitmap_disable_percentage: usize,
        segmented_bitmap_enable_percentage: usize,
        disable_thp: bool,
        random_magic: bool,
        foreign_pointer_abort: bool,
    ) -> Self {
        Self {
            arena_size,
            max_refill,
            init_batch,
            segmented_bitmap_max_cache,
            segmented_bitmap_attempt_huge,
            disable_trimmer,
            small_trim_threshold,
            big_trim_threshold,
            disable_relief,
            segmented_bitmap_disable_percentage,
            segmented_bitmap_enable_percentage,
            disable_thp,
            random_magic,
            foreign_pointer_abort,
        }
    }
}

#[inline(never)]
pub unsafe fn main_bootstrap(config: BootstrapConfig) {
    if __rseq_size == 0 || __rseq_offset == 0 {
        RSMallocError::RseqUnavailable.log_and_abort();
    }

    #[cfg(feature = "debug")]
    {
        crate::START_TIME = Some(std::time::Instant::now());
    }

    get_clock();

    ARENA_SIZE = config.arena_size.max(1024 * 512);
    MAX_REFILL_RETRIES = config.max_refill;
    PREDICTOR_INIT_BATCH = config.init_batch;

    RADIX = RadixTree::new();
    SLAB_CACHE.ensure_cache();

    let node_count = SLAB_CACHE.get_numa_and_inner().0.nranges;
    PAGE_ALLOCATOR.init(node_count);

    SEGMENTED_BITMAP_MAX_CACHE = config.segmented_bitmap_max_cache;

    SEGMENTED_BITMAP_ATTEMPT_HUGE = config.segmented_bitmap_attempt_huge;
    DISABLE_TRIM_THREAD = config.disable_trimmer;
    SMALL_TRIM_THRESHOLD = config.small_trim_threshold;
    BIG_TRIM_THRESHOLD = config.big_trim_threshold;
    DISABLE_RELIEF = config.disable_relief;

    SEGMENTED_BITMAP_DISABLE_PERCENTAGE = config.segmented_bitmap_disable_percentage;
    SEGMENTED_BITMAP_ENABLE_PERCENTAGE = config.segmented_bitmap_enable_percentage;
    RS_DISABLE_THP = config.disable_thp;

    #[cfg(not(feature = "preload"))]
    {
        crate::global_vals::FOREIGN_POINTER_ABORT = config.foreign_pointer_abort;
    }

    #[cfg(feature = "preload")]
    let _ = config.foreign_pointer_abort;

    SEGMENTED_BITMAP_BACKEND.init(
        SEGMENTED_BITMAP_MAX_CACHE,
        SEGMENTED_BITMAP_ATTEMPT_HUGE && !RS_DISABLE_THP,
    );

    #[cfg(feature = "preload")]
    crate::core_prim::fork::register_fork_handlers();

    if config.random_magic {
        init_magic();
        init_align();

        if ALIGN_TAG == 0 {
            while ALIGN_TAG == 0 {
                init_align();
            }
        }
    };
}
