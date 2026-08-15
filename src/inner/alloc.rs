use std::hint::unlikely;
use std::sync::atomic::AtomicBool;
#[cfg(feature = "debug-full-critic")]
use std::sync::atomic::AtomicUsize;
use std::{hint::likely, ptr::null_mut};

use crate::ALLOCATED_FLAG;
#[cfg(feature = "preload")]
use crate::inner::libc_int::set_nomem;
use crate::{
    BIG_MAGIC, Header, MAGIC, RSMallocError,
    big_allocations::big_allocation::big_malloc,
    core_prim::{
        predictor::{BULK_FILL_BATCHING, TRANSFER_BATCHING},
        wrappers::UnsafePointer,
    },
    inner::free::find_original_ptr,
    internals::{radix_tree::RADIX, rbtree::BIG_META_MAP},
    rseq_core::{bulk_fill::bulk_fill, rseq_offsets::get_rseq, slab_cache::SLAB_CACHE},
    traits::GenericCache,
    utility::{ITERATIONS, SIZE_CLASSES, match_size_class},
};
#[cfg(feature = "debug")]
use crate::{REFILLS_BY_CLASS, TOTAL_REFILL_CALLS};
use crate::{TOTAL_CACHED_VA, TRIM_THRESHOLD, backend::trim::trimmer_main};
#[cfg(feature = "debug")]
use std::sync::atomic::Ordering;

#[cfg(feature = "preload")]
static ONCE: crate::internals::once::Once = crate::internals::once::Once::new();

pub static mut MAX_REFILL_RETRIES: usize = 3;

#[cfg(all(not(feature = "debug-predictor-exact"), feature = "debug"))]
#[inline(always)]
unsafe fn record_refill_prediction(
    class: usize,
    count: usize,
    wanted: usize,
    _cpu_id: usize,
    _can_probe_more: bool,
) {
    if wanted <= 1 || count <= 1 {
        return;
    }

    if count * 2 < wanted {
        crate::REFILL_OVER_PREDICTS.fetch_add(1, Ordering::Relaxed);
        return;
    }

    if count == wanted && wanted >= 8 && wanted < ITERATIONS[class] {
        crate::REFILL_UNDER_PREDICTS.fetch_add(1, Ordering::Relaxed);
    }
}

#[cfg(feature = "debug-predictor-exact")]
#[inline(always)]
unsafe fn record_refill_prediction(
    class: usize,
    count: usize,
    wanted: usize,
    cpu_id: usize,
    can_probe_more: bool,
) {
    let inner = SLAB_CACHE.get_inner();
    if wanted <= 1 || count == 0 {
        return;
    }

    if !can_probe_more {
        if count < wanted {
            crate::REFILL_OVER_PREDICTS.fetch_add(1, Ordering::Relaxed);
        }
        return;
    }

    if count < wanted {
        let missing = wanted - count;
        let (extra, extra_tail, extra_count) = SLAB_CACHE.try_pop(class, missing, cpu_id);
        if extra_count > 0 && !extra.is_null() {
            SLAB_CACHE.transfer_push_batch(
                class,
                extra.as_ptr(),
                extra_tail.as_ptr(),
                cpu_id,
                inner,
            );
        }

        if count + extra_count < wanted {
            crate::REFILL_OVER_PREDICTS.fetch_add(1, Ordering::Relaxed);
        }
        return;
    }

    if wanted >= 8 && wanted < ITERATIONS[class] {
        let (extra, extra_tail, extra_count) = SLAB_CACHE.try_pop(class, 1, cpu_id);
        if extra_count > 0 && !extra.is_null() {
            crate::REFILL_UNDER_PREDICTS.fetch_add(1, Ordering::Relaxed);
            SLAB_CACHE.transfer_push_batch(
                class,
                extra.as_ptr(),
                extra_tail.as_ptr(),
                cpu_id,
                inner,
            );
        }
    }
}

#[inline(always)]
unsafe fn take_one_from_batch(
    class: usize,
    start: *mut Header,
    tail: *mut Header,
    count: usize,
    #[cfg(feature = "debug")] wanted: usize,
    #[cfg(feature = "debug")] cpu_id: usize,
    #[cfg(feature = "debug")] can_probe_more: bool,
) -> UnsafePointer<Header> {
    let first = start;

    #[cfg(feature = "debug-printer-thread")]
    crate::debug_printer_thread::start();

    if count > 1 {
        #[cfg(feature = "debug")]
        record_refill_prediction(class, count, wanted, cpu_id, can_probe_more);

        let rest = (*first).next;
        if !rest.is_null() {
            SLAB_CACHE.push_tailed(class, rest, tail, count - 1);
        }
    }

    (*first).next = null_mut();
    UnsafePointer::new(first)
}

macro_rules! refill {
    ($class:expr) => {
        TRANSFER_BATCHING[$class].batch(ITERATIONS[$class])
    };
}

macro_rules! bulk_refill {
    ($class:expr) => {
        BULK_FILL_BATCHING[$class].batch(ITERATIONS[$class])
    };
}

pub static TRIM_GUARD: AtomicBool = AtomicBool::new(false);

#[cold]
#[inline(never)]
pub unsafe fn spawn(entry: unsafe fn() -> !) -> bool {
    std::thread::Builder::new()
        .name("rsmalloc-trimmer".into())
        .stack_size(64 * 1024)
        .spawn(move || unsafe {
            entry();
        })
        .is_ok()
}

pub unsafe fn maybe_start_trimmer() {
    use std::sync::atomic::Ordering;

    if TOTAL_CACHED_VA.load(Ordering::Relaxed) < TRIM_THRESHOLD
        || TRIM_GUARD.load(Ordering::Relaxed) == true
    {
        return;
    }

    if TRIM_GUARD
        .compare_exchange(false, true, Ordering::Release, Ordering::Acquire)
        .is_ok()
    {
        if !spawn(trimmer_main) {
            TRIM_GUARD.store(false, Ordering::Relaxed);
        }
    };
}

#[inline(never)]
pub unsafe fn refill(class: usize, cpu_id: usize) -> UnsafePointer<Header> {
    for _ in 0..MAX_REFILL_RETRIES {
        let bulk_batch = bulk_refill!(class);

        match bulk_fill(class, cpu_id, bulk_batch) {
            Ok((start, tail, count)) => {
                let observed = if count == bulk_batch && bulk_batch < ITERATIONS[class] {
                    bulk_batch.saturating_add((bulk_batch / 4).max(1))
                } else {
                    count
                };

                BULK_FILL_BATCHING[class].update_refill(observed, ITERATIONS[class]);
                let result = take_one_from_batch(
                    class,
                    start,
                    tail,
                    count,
                    #[cfg(feature = "debug")]
                    bulk_batch,
                    #[cfg(feature = "debug")]
                    cpu_id,
                    #[cfg(feature = "debug")]
                    false,
                );

                maybe_start_trimmer();

                return result;
            }
            Err(_) => continue,
        }
    }

    SLAB_CACHE.try_pop(class, 1, cpu_id).0
}

#[unsafe(link_section = ".text.hot")]
#[inline(never)]
pub unsafe fn fill(class: usize) -> UnsafePointer<Header> {
    #[cfg(feature = "debug")]
    {
        TOTAL_REFILL_CALLS.fetch_add(1, Ordering::Relaxed);
        REFILLS_BY_CLASS[class].fetch_add(1, Ordering::Relaxed);
    }

    let cache_batch = refill!(class);
    let cpu_id = get_rseq().cpu_id as usize;

    let (start, tail, count) = SLAB_CACHE.try_pop(class, cache_batch, cpu_id);

    if !start.is_null() {
        let observed = if count == cache_batch && cache_batch < ITERATIONS[class] {
            cache_batch.saturating_add((cache_batch / 4).max(1))
        } else {
            count
        };

        TRANSFER_BATCHING[class].update_refill(observed, ITERATIONS[class]);
        let one = take_one_from_batch(
            class,
            start.as_ptr(),
            tail.as_ptr(),
            count,
            #[cfg(feature = "debug")]
            cache_batch,
            #[cfg(feature = "debug")]
            cpu_id,
            #[cfg(feature = "debug")]
            true,
        );

        return one;
    }

    refill(class, cpu_id)
}

// In hardened builds, verify that a pointer popped from allocator-managed
// lists still belongs to rsmalloc before dereferencing or stamping its header.
// This detects corrupted or forged freelist metadata; it is not a complete
// freelist-integrity proof.
macro_rules! is_owned {
    ($ptr:expr) => {
        #[cfg(feature = "check-owned-on-alloc")]
        {
            if !RADIX.is_owned($ptr.cast_usize()) {
                RSMallocError::AttackOrCorruption.log_and_abort(
                    $ptr.cast_as_ptr(),
                    "CRITICAL: possible metadata injection: popped pointer is not owned by rsmalloc",
                    None,
                );
            }
        }
    };
}

#[cfg(feature = "debug-full-critic")]
pub static RS_ALLOC_CALLS_DEBUG: AtomicUsize = AtomicUsize::new(0);

#[inline(always)]
pub unsafe fn rs_alloc(size: usize, aligned: bool) -> UnsafePointer<Header> {
    #[cfg(feature = "preload")]
    ONCE.call_once(|| crate::core_prim::bootstrap::bootstrap());

    #[cfg(feature = "debug-full-critic")]
    RS_ALLOC_CALLS_DEBUG.fetch_add(1, std::sync::atomic::Ordering::Relaxed);

    let class = match_size_class(size);

    if let Some(class) = class {
        let cache = SLAB_CACHE.pop(class);

        let cache = if cache.is_null() {
            let class = fill(class);

            if unlikely(class.is_null()) {
                #[cfg(feature = "preload")]
                set_nomem();
                return UnsafePointer::NULL;
            }

            class
        } else {
            cache
        };

        is_owned!(cache);

        let mut safe = cache.apply_safe();
        safe.magic = MAGIC;
        safe.flags = ALLOCATED_FLAG;

        return cache.walk_header();
    }

    let cache = big_malloc(size, aligned);

    if !cache.is_null() && cfg!(feature = "check-owned-on-alloc") {
        is_owned!(cache)
    };

    cache.cast()
}

#[inline(always)]
pub unsafe fn usable_size(ptr: UnsafePointer<Header>) -> usize {
    let ptr_addr = ptr.cast_usize();

    if likely(RADIX.is_owned(ptr_addr)) {
        let original_payload = find_original_ptr(ptr);
        let original_payload_addr = original_payload.cast_usize();
        let offset = ptr_addr - original_payload_addr;
        let header = original_payload.get_actual_header().apply_safe();

        if header.magic == BIG_MAGIC {
            let meta = BIG_META_MAP.get(original_payload_addr).unwrap_or_else(|| {
                RSMallocError::MemoryCorruption.log_and_abort(
                    null_mut(),
                    "missing header for big allocation",
                    None,
                )
            });

            return meta.size.saturating_sub(offset);
        }

        let total_payload = SIZE_CLASSES[header.class as usize];

        return total_payload.saturating_sub(offset);
    }

    #[cfg(feature = "preload")]
    {
        crate::inner::fallback::malloc_usable_size_fallback(ptr.cast_as_ptr())
    }

    #[cfg(not(feature = "preload"))]
    {
        0
    }
}
