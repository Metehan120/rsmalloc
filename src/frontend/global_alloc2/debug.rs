#[cfg(any(feature = "debug", doc))]
use crate::{Header, core_prim::wrappers::UnsafePointer, inner::alloc::rs_alloc};
use crate::{
    big_allocations::segmented_bitmap::{BIG_BUDDY_MAX_ORDER, BIG_BUDDY_MIN_ORDER},
    v2::alloc::RSMalloc,
};

impl RSMalloc {
    #[cfg(feature = "debug")]
    pub fn get_stats(&self) -> RSMallocStats {
        use crate::{
            ABORTS, BUDDY_AVERAGE_BLOCK_TIMES, CURRENT_STAMP, HIGH_WATER_BUDDY_CACHED_VA,
            HIGH_WATER_SLAB_CACHED_VA, HIGH_WATER_TOTAL_CACHED_VA, NCPU, REFILL_OVER_PREDICTS,
            REFILL_UNDER_PREDICTS, REFILLS_BY_CLASS, START_TIME, TOTAL_CACHED_VA, TOTAL_MMAP_BYTES,
            TOTAL_MMAP_CALLS, TOTAL_REFILL_CALLS,
            backend::{
                page_allocator::{ARENA_SIZE, PAGE_ALLOCATOR, TOTAL_LIVED, TOTAL_REMOVED},
                trim::{DISABLE_BUDDY, TOTAL_TRIM_CALLS, TOTAL_TRIMMED_VA},
            },
            big_allocations::segmented_bitmap::{BIG_BUDDY_MIN_ORDER, SEGMENTED_BITMAP_BACKEND, BUDDY_TOTAL_CACHED_VA},
            internals::radix_tree::{CHUNK_SIZE, RADIX},
            rseq_core::slab_cache::SLAB_CACHE,
            utility::{NUM_SIZE_CLASSES, SIZE_CLASSES},
            v2::alloc::RSMallocCoreAPI,
        };
        use std::sync::atomic::Ordering::{self, Relaxed};

        self.manual_init();

        let uptime_ms = unsafe { START_TIME }
            .map(|start| start.elapsed().as_millis() as usize)
            .unwrap_or(0);
        let clock_ms = CURRENT_STAMP.load(Relaxed) as u64 * 100;

        let arena_counts = unsafe { PAGE_ALLOCATOR.arena_counts() };
        let total_arenas: usize = arena_counts.iter().sum();

        let under = REFILL_UNDER_PREDICTS.load(Ordering::Relaxed);
        let over = REFILL_OVER_PREDICTS.load(Ordering::Relaxed);
        let misses = under.saturating_add(over);
        let total = TOTAL_REFILL_CALLS.load(Ordering::Relaxed);
        let percentage = if total == 0 {
            0.0
        } else {
            (misses as f64 / total as f64) * 100.0
        };
        let success_rates = 100.0 - percentage;
        let aborts = ABORTS.load(Ordering::Relaxed);

        let slab_cached_va = TOTAL_CACHED_VA.load(Relaxed);
        let buddy_cached_va = BUDDY_TOTAL_CACHED_VA.load(Relaxed);
        let total_cached_va = slab_cached_va.saturating_add(buddy_cached_va);

        let mut rseq_cpu_total_cached_bytes = 0usize;
        let mut rseq_cpu_min_cached_bytes = usize::MAX;
        let mut rseq_cpu_max_cached_bytes = 0usize;
        let mut rseq_cpu_nonempty = 0usize;
        let cpu_limit = unsafe { NCPU };
        let rseq_cpu_buffer = unsafe { alloc_usize_array(cpu_limit) };
        let rseq_cpu_cached_bytes = rseq_cpu_buffer.cast_as_ptr() as *mut usize;
        for cpu in 0..cpu_limit {
            let bytes = unsafe { SLAB_CACHE.get_rseq_cpu_usage_bytes(cpu) };
            if !rseq_cpu_cached_bytes.is_null() {
                unsafe { *rseq_cpu_cached_bytes.add(cpu) = bytes };
            }
            rseq_cpu_total_cached_bytes = rseq_cpu_total_cached_bytes.saturating_add(bytes);
            rseq_cpu_min_cached_bytes = rseq_cpu_min_cached_bytes.min(bytes);
            rseq_cpu_max_cached_bytes = rseq_cpu_max_cached_bytes.max(bytes);
            if bytes != 0 {
                rseq_cpu_nonempty += 1;
            }
        }
        if cpu_limit == 0 {
            rseq_cpu_min_cached_bytes = 0;
        }

        let mut refills_by_class = [0usize; NUM_SIZE_CLASSES];
        let mut class_cached_bytes = [0usize; NUM_SIZE_CLASSES];
        let mut class_active_cpus = [0usize; NUM_SIZE_CLASSES];
        let mut class_min_cached_bytes = [0usize; NUM_SIZE_CLASSES];
        let mut class_max_cached_bytes = [0usize; NUM_SIZE_CLASSES];
        let mut class_avg_cached_bytes = [0usize; NUM_SIZE_CLASSES];

        for class in 0..NUM_SIZE_CLASSES {
            refills_by_class[class] = REFILLS_BY_CLASS[class].load(Relaxed);
            let mut min = usize::MAX;
            let mut max = 0usize;
            let mut active = 0usize;
            let mut total_cached = 0usize;

            for cpu in 0..cpu_limit {
                let bytes = unsafe { SLAB_CACHE.get_rseq_cpu_class_usage_bytes(cpu, class) };
                total_cached = total_cached.saturating_add(bytes);
                if bytes != 0 {
                    active += 1;
                    min = min.min(bytes);
                    max = max.max(bytes);
                }
            }

            class_cached_bytes[class] = total_cached;
            class_active_cpus[class] = active;
            class_min_cached_bytes[class] = if active == 0 { 0 } else { min };
            class_max_cached_bytes[class] = max;
            class_avg_cached_bytes[class] = if active == 0 {
                0
            } else {
                total_cached / active
            };
        }

        let (numa, inner) = unsafe { SLAB_CACHE.get_numa_and_inner() };
        let buddy = unsafe { SEGMENTED_BITMAP_BACKEND.report() };
        let radix = unsafe { RADIX.report() };
        let buddy_used_bytes = buddy.total_region_bytes.saturating_sub(buddy.free_bytes);
        let buddy_free_blocks = buddy.free_blocks.iter().sum();
        let buddy_never_allocated_bytes = buddy
            .never_allocated_by_order
            .iter()
            .enumerate()
            .map(|(index, blocks)| blocks.saturating_mul(1usize << (BIG_BUDDY_MIN_ORDER + index)))
            .sum();
        let buddy_reused_bytes = buddy
            .reused_by_order
            .iter()
            .enumerate()
            .map(|(index, blocks)| blocks.saturating_mul(1usize << (BIG_BUDDY_MIN_ORDER + index)))
            .sum();
        let buddy_trimmed_bytes = buddy
            .trimmed_by_order
            .iter()
            .enumerate()
            .map(|(index, blocks)| blocks.saturating_mul(1usize << (BIG_BUDDY_MIN_ORDER + index)))
            .sum();
        let radix_owned_bytes = radix.owned_chunks.saturating_mul(CHUNK_SIZE);
        let radix_metadata_per_chunk = if radix.owned_chunks == 0 {
            0.0
        } else {
            radix.metadata_bytes as f64 / radix.owned_chunks as f64
        };

        RSMallocStats {
            pid: std::process::id(),
            uptime_ms,
            clock_ms,
            mmap_calls: TOTAL_MMAP_CALLS.load(Relaxed),
            mmap_bytes_requested: TOTAL_MMAP_BYTES.load(Relaxed),
            total_arenas,
            arenas_lived: TOTAL_LIVED.load(Relaxed),
            arenas_removed: TOTAL_REMOVED.load(Relaxed),
            arena_size: unsafe { ARENA_SIZE },
            total_refills: total,
            refill_under_predicts: under,
            refill_over_predicts: over,
            total_misses: misses,
            miss_percentage: percentage,
            success_rates,
            rseq_aborts: aborts,
            total_cached_va,
            slab_cached_va,
            buddy_cached_va,
            high_water_slab_cached_va: HIGH_WATER_SLAB_CACHED_VA.load(Relaxed),
            high_water_buddy_cached_va: HIGH_WATER_BUDDY_CACHED_VA.load(Relaxed),
            high_water_total_cached_va: HIGH_WATER_TOTAL_CACHED_VA.load(Relaxed),
            numa_enabled: inner.is_numa,
            numa_cpus: numa.ncpu,
            numa_nodes: numa.nnodes,
            numa_ranges: numa.nranges,
            rseq_cpu_count: cpu_limit,
            rseq_cpu_cached_bytes,
            rseq_cpu_buffer,
            rseq_cpu_total_cached_bytes,
            rseq_cpu_min_cached_bytes,
            rseq_cpu_max_cached_bytes,
            rseq_cpu_nonempty,
            size_classes: SIZE_CLASSES,
            refills_by_class,
            class_cached_bytes,
            class_active_cpus,
            class_min_cached_bytes,
            class_max_cached_bytes,
            class_avg_cached_bytes,
            trim_calls: TOTAL_TRIM_CALLS.load(Relaxed),
            trimmed_va: TOTAL_TRIMMED_VA.load(Relaxed),
            avg_small_life_ms: crate::AVERAGE_BLOCK_TIMES.load(Relaxed).saturating_mul(100),
            avg_buddy_life_ms: BUDDY_AVERAGE_BLOCK_TIMES.load(Relaxed).saturating_mul(100),
            buddy_disabled: DISABLE_BUDDY.load(Relaxed),
            buddy_regions: buddy.regions,
            buddy_total_region_bytes: buddy.total_region_bytes,
            buddy_used_bytes,
            buddy_free_bytes: buddy.free_bytes,
            buddy_free_blocks,
            buddy_never_allocated_blocks: buddy.never_allocated_blocks,
            buddy_reused_blocks: buddy.reused_blocks,
            buddy_trimmed_blocks: buddy.trimmed_blocks,
            buddy_never_allocated_bytes,
            buddy_reused_bytes,
            buddy_trimmed_bytes,
            buddy_free_blocks_by_order: buddy.free_blocks,
            buddy_never_allocated_by_order: buddy.never_allocated_by_order,
            buddy_reused_by_order: buddy.reused_by_order,
            buddy_trimmed_by_order: buddy.trimmed_by_order,
            buddy_grow_order: buddy.grow_order,
            buddy_thp: buddy.thp,
            radix_l1_nodes: radix.l1_nodes,
            radix_l2_nodes: radix.l2_nodes,
            radix_leaves: radix.leaves,
            radix_owned_chunks: radix.owned_chunks,
            radix_chunk_size: CHUNK_SIZE,
            radix_owned_bytes,
            radix_metadata_bytes: radix.metadata_bytes,
            radix_metadata_per_chunk,
        }
    }

    #[cfg(feature = "debug-exact")]
    pub fn get_exact_stats(&self) -> RSMallocExactStats {
        #[cfg(feature = "debug-full-critic")]
        use crate::inner::{alloc::RS_ALLOC_CALLS_DEBUG, free::RS_FREE_CALLS_DEBUG};
        #[cfg(feature = "transfer-debug-exact")]
        use crate::{
            DRY_TRANSFER_STEALS, TOTAL_TRANSFER_POP_CALLS, TOTAL_TRANSFER_PUSH_CALLS,
            TOTAL_TRANSFER_RETRIES, TOTAL_TRANSFER_STEALS,
        };
        use crate::{
            GLOBAL_LOCK_RETRIES, GLOBAL_LOCKS, GLOBAL_SPIN_WAITS, GLOBAL_TRY_LOCK_MISSES,
            GLOBAL_TRY_LOCKS,
            backend::trim::{TOTAL_TRIMMED_BLOCKS, TOTAL_TRIMMED_TIME},
        };
        use std::sync::atomic::Ordering::Relaxed;

        let stats = self.get_stats();
        let trimmed_blocks_small = TOTAL_TRIMMED_BLOCKS.load(Relaxed);
        let total_locks = GLOBAL_LOCKS.load(Relaxed);
        let total_lock_retries = GLOBAL_LOCK_RETRIES.load(Relaxed);
        let aborts_vs_locks = if total_locks == 0 {
            0.0
        } else {
            stats.rseq_aborts as f64 / total_locks as f64
        };

        RSMallocExactStats {
            under_vs_over: stats
                .refill_under_predicts
                .saturating_sub(stats.refill_over_predicts),
            over_vs_under: stats
                .refill_over_predicts
                .saturating_sub(stats.refill_under_predicts),
            total_locks,
            total_lock_retries,
            total_try_locks: GLOBAL_TRY_LOCKS.load(Relaxed),
            total_try_lock_misses: GLOBAL_TRY_LOCK_MISSES.load(Relaxed),
            total_spin_waits: GLOBAL_SPIN_WAITS.load(Relaxed),
            #[cfg(feature = "transfer-debug-exact")]
            total_transfer_pop_calls: TOTAL_TRANSFER_POP_CALLS.load(Relaxed),
            #[cfg(feature = "transfer-debug-exact")]
            total_transfer_push_calls: TOTAL_TRANSFER_PUSH_CALLS.load(Relaxed),
            #[cfg(feature = "transfer-debug-exact")]
            total_transfer_steals: TOTAL_TRANSFER_STEALS.load(Relaxed),
            #[cfg(feature = "transfer-debug-exact")]
            total_transfer_retries: TOTAL_TRANSFER_RETRIES.load(Relaxed),
            #[cfg(feature = "transfer-debug-exact")]
            dry_transfer_steals: DRY_TRANSFER_STEALS.load(Relaxed),
            #[cfg(feature = "debug-full-critic")]
            alloc_calls: RS_ALLOC_CALLS_DEBUG.load(Relaxed),
            #[cfg(feature = "debug-full-critic")]
            free_calls: RS_FREE_CALLS_DEBUG.load(Relaxed),
            trimmed_blocks_small,
            avg_madvise_cycles_small: TOTAL_TRIMMED_TIME.load(Relaxed)
                / trimmed_blocks_small.max(1),
            aborts_vs_locks,
            stats,
        }
    }
}

pub const RSMALLOC_BUDDY_NUM_ORDERS: usize = BIG_BUDDY_MAX_ORDER - BIG_BUDDY_MIN_ORDER + 1;

#[cfg(any(feature = "debug", doc))]
unsafe fn alloc_usize_array(count: usize) -> UnsafePointer<Header> {
    let Some(bytes) = count.checked_mul(core::mem::size_of::<usize>()) else {
        return UnsafePointer::NULL;
    };

    rs_alloc(bytes, false)
}

#[cfg(any(feature = "debug", doc))]
/// rsmalloc structured debug stats.
pub struct RSMallocStats {
    pub pid: u32,
    pub uptime_ms: usize,
    pub clock_ms: u64,

    pub mmap_calls: usize,
    pub mmap_bytes_requested: usize,

    pub total_arenas: usize,
    pub arenas_lived: usize,
    pub arenas_removed: usize,
    pub arena_size: usize,

    pub total_refills: usize,
    pub refill_under_predicts: usize,
    pub refill_over_predicts: usize,
    pub total_misses: usize,
    pub success_rates: f64,
    pub miss_percentage: f64,
    pub rseq_aborts: usize,

    pub total_cached_va: usize,
    pub slab_cached_va: usize,
    pub buddy_cached_va: usize,
    pub high_water_slab_cached_va: usize,
    pub high_water_buddy_cached_va: usize,
    pub high_water_total_cached_va: usize,

    pub numa_enabled: bool,
    pub numa_cpus: usize,
    pub numa_nodes: usize,
    pub numa_ranges: usize,

    pub rseq_cpu_count: usize,
    pub rseq_cpu_cached_bytes: *mut usize,
    rseq_cpu_buffer: UnsafePointer<Header>,
    pub rseq_cpu_total_cached_bytes: usize,
    pub rseq_cpu_min_cached_bytes: usize,
    pub rseq_cpu_max_cached_bytes: usize,
    pub rseq_cpu_nonempty: usize,

    pub size_classes: [usize; crate::utility::NUM_SIZE_CLASSES],
    pub refills_by_class: [usize; crate::utility::NUM_SIZE_CLASSES],
    pub class_cached_bytes: [usize; crate::utility::NUM_SIZE_CLASSES],
    pub class_active_cpus: [usize; crate::utility::NUM_SIZE_CLASSES],
    pub class_min_cached_bytes: [usize; crate::utility::NUM_SIZE_CLASSES],
    pub class_max_cached_bytes: [usize; crate::utility::NUM_SIZE_CLASSES],
    pub class_avg_cached_bytes: [usize; crate::utility::NUM_SIZE_CLASSES],

    pub trim_calls: usize,
    pub trimmed_va: usize,
    pub avg_small_life_ms: u32,
    pub avg_buddy_life_ms: u32,
    pub buddy_disabled: bool,

    pub buddy_regions: usize,
    pub buddy_total_region_bytes: usize,
    pub buddy_used_bytes: usize,
    pub buddy_free_bytes: usize,
    pub buddy_free_blocks: usize,
    pub buddy_never_allocated_blocks: usize,
    pub buddy_reused_blocks: usize,
    pub buddy_trimmed_blocks: usize,
    pub buddy_never_allocated_bytes: usize,
    pub buddy_reused_bytes: usize,
    pub buddy_trimmed_bytes: usize,
    pub buddy_free_blocks_by_order: [usize; RSMALLOC_BUDDY_NUM_ORDERS],
    pub buddy_never_allocated_by_order: [usize; RSMALLOC_BUDDY_NUM_ORDERS],
    pub buddy_reused_by_order: [usize; RSMALLOC_BUDDY_NUM_ORDERS],
    pub buddy_trimmed_by_order: [usize; RSMALLOC_BUDDY_NUM_ORDERS],
    pub buddy_grow_order: usize,
    pub buddy_thp: bool,

    pub radix_l1_nodes: usize,
    pub radix_l2_nodes: usize,
    pub radix_leaves: usize,
    pub radix_owned_chunks: usize,
    pub radix_chunk_size: usize,
    pub radix_owned_bytes: usize,
    pub radix_metadata_bytes: usize,
    pub radix_metadata_per_chunk: f64,
}

#[cfg(any(feature = "debug", doc))]
impl Drop for RSMallocStats {
    fn drop(&mut self) {
        if self.rseq_cpu_cached_bytes.is_null() || self.rseq_cpu_count == 0 {
            return;
        }

        unsafe {
            use crate::{core_prim::wrappers::UnsafePointer, inner::free::rs_free};
            rs_free(UnsafePointer::new(self.rseq_cpu_buffer.as_ptr()))
        };
    }
}

#[cfg(any(feature = "debug-exact", doc))]
/// rsmalloc structured exact debug stats.
pub struct RSMallocExactStats {
    pub stats: RSMallocStats,
    pub total_locks: usize,
    pub total_lock_retries: usize,
    pub total_try_locks: usize,
    pub total_try_lock_misses: usize,
    pub total_spin_waits: usize,
    #[cfg(feature = "transfer-debug-exact")]
    pub total_transfer_pop_calls: usize,
    #[cfg(feature = "transfer-debug-exact")]
    pub total_transfer_push_calls: usize,
    #[cfg(feature = "transfer-debug-exact")]
    pub total_transfer_steals: usize,
    #[cfg(feature = "transfer-debug-exact")]
    pub total_transfer_retries: usize,
    #[cfg(feature = "transfer-debug-exact")]
    pub dry_transfer_steals: usize,
    #[cfg(feature = "debug-full-critic")]
    pub alloc_calls: usize,
    #[cfg(feature = "debug-full-critic")]
    pub free_calls: usize,
    pub trimmed_blocks_small: usize,
    pub avg_madvise_cycles_small: usize,
    pub aborts_vs_locks: f64,
    pub under_vs_over: usize,
    pub over_vs_under: usize,
}
