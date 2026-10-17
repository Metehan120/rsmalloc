use crate::{
    backend::{
        background_thread::{
            DISABLE_RELIEF, SEGMENTED_BITMAP_DISABLE_PERCENTAGE, SEGMENTED_BITMAP_ENABLE_PERCENTAGE,
        },
        page_allocator::{ARENA_SIZE, MIN_ARENA_SIZE, PAGE_ALLOCATOR},
    },
    big_allocations::segmented_bitmap::{
        BIG_SEGMENTED_BITMAP_MAX_ORDER, SEGMENTED_BITMAP_BACKEND, SegmentedBitmapAllocator,
    },
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
    utility::Alignment,
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

    fn prepare(mut self) -> Option<PreparedBootstrapConfig> {
        self.arena_size = self.arena_size.max(MIN_ARENA_SIZE).checked_align_to(4096)?;
        self.segmented_bitmap_max_cache = self
            .segmented_bitmap_max_cache
            .checked_next_power_of_two()?
            .max(1 << BIG_SEGMENTED_BITMAP_MAX_ORDER);
        self.segmented_bitmap_disable_percentage =
            self.segmented_bitmap_disable_percentage.min(100);
        self.segmented_bitmap_enable_percentage = self
            .segmented_bitmap_enable_percentage
            .min(self.segmented_bitmap_disable_percentage);
        self.segmented_bitmap_attempt_huge &= !self.disable_thp;
        Some(PreparedBootstrapConfig { settings: self })
    }
}

struct PreparedBootstrapConfig {
    settings: BootstrapConfig,
}

impl PreparedBootstrapConfig {
    fn big_trim_threshold(&self, node_count: usize, ncpu: usize) -> Option<usize> {
        let footprint = SegmentedBitmapAllocator::initial_cached_bytes(
            self.settings.segmented_bitmap_max_cache,
            node_count,
            ncpu,
        )?
        .checked_add(1)?;
        Some(self.settings.big_trim_threshold.max(footprint))
    }

    unsafe fn apply(&self) {
        let config = &self.settings;
        ARENA_SIZE = config.arena_size;
        MAX_REFILL_RETRIES = config.max_refill;
        PREDICTOR_INIT_BATCH = config.init_batch;
        SEGMENTED_BITMAP_MAX_CACHE = config.segmented_bitmap_max_cache;
        SEGMENTED_BITMAP_ATTEMPT_HUGE = config.segmented_bitmap_attempt_huge;
        DISABLE_TRIM_THREAD = config.disable_trimmer;
        DISABLE_RELIEF = config.disable_relief;
        // Subsystem initialization may check whether to start the worker.
        // Keep both thresholds unreachable until all bootstrap work is done.
        SMALL_TRIM_THRESHOLD = usize::MAX;
        BIG_TRIM_THRESHOLD = usize::MAX;
        SEGMENTED_BITMAP_DISABLE_PERCENTAGE = config.segmented_bitmap_disable_percentage;
        SEGMENTED_BITMAP_ENABLE_PERCENTAGE = config.segmented_bitmap_enable_percentage;
        RS_DISABLE_THP = config.disable_thp;

        #[cfg(not(feature = "preload"))]
        {
            crate::global_vals::FOREIGN_POINTER_ABORT = config.foreign_pointer_abort;
        }
        #[cfg(feature = "preload")]
        let _ = config.foreign_pointer_abort;
    }
}

#[inline(never)]
pub unsafe fn main_bootstrap(config: BootstrapConfig) {
    if __rseq_size == 0 || __rseq_offset == 0 {
        RSMallocError::RseqUnavailable.log_and_abort();
    }

    let prepared = config.prepare().unwrap_or_else(|| {
        RSMallocError::Bootstrap {
            reason: "arena size alignment or segmented bitmap region size overflowed",
        }
        .log_and_abort()
    });

    #[cfg(feature = "debug")]
    {
        crate::START_TIME = Some(std::time::Instant::now());
    }
    get_clock();

    if prepared.settings.random_magic {
        init_magic();
        init_align();
        while ALIGN_TAG == 0 {
            init_align();
        }
    }

    prepared.apply();
    RADIX = RadixTree::new();
    SLAB_CACHE.ensure_cache();

    let numa = SLAB_CACHE.get_numa_and_inner().0;
    let big_trim_threshold = prepared
        .big_trim_threshold(numa.nranges, numa.ncpu)
        .unwrap_or_else(|| {
            RSMallocError::Bootstrap {
                reason: "segmented bitmap initial footprint or big trim threshold overflowed",
            }
            .log_and_abort()
        });
    PAGE_ALLOCATOR.init(numa.nranges);

    SEGMENTED_BITMAP_BACKEND.init(
        prepared.settings.segmented_bitmap_max_cache,
        prepared.settings.segmented_bitmap_attempt_huge,
    );

    #[cfg(feature = "preload")]
    crate::core_prim::fork::register_fork_handlers();

    // Restore the effective thresholds last. Worker startup stays lazy: a
    // later refill/growth checks them after the caller's initialization gate.
    SMALL_TRIM_THRESHOLD = prepared.settings.small_trim_threshold;
    BIG_TRIM_THRESHOLD = big_trim_threshold;
}
