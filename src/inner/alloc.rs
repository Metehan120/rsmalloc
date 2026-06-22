use std::{hint::likely, ptr::null_mut};

#[cfg(feature = "debug")]
use crate::TOTAL_REFILL_CALLS;
use crate::{
    BIG_MAGIC, GenericCache, Header, MAGIC, RSMallocError,
    big_allocations::big_allocation::big_malloc,
    core_prim::{
        predictor::{BULK_FILL_PREDICTOR, PREDICTOR},
        wrappers::UnsafePointer,
    },
    inner::free::find_original_ptr,
    internals::{hashmap::BIG_ALLOC_MAP, l3_main_radix::L3_RADIX},
    rseq_core::{bulk_fill::bulk_fill, rseq_cache::RSEQ_CACHE, rseq_main::get_rseq},
    utility::{ITERATIONS, SIZE_CLASSES, match_size_class},
};
#[cfg(feature = "debug")]
use std::sync::atomic::Ordering;

#[cfg(feature = "preload")]
static ONCE: crate::internals::once::Once = crate::internals::once::Once::new();

pub static mut MAX_REFILL_RETRIES: usize = 3;

#[inline(always)]
unsafe fn refill_batch(class: usize) -> usize {
    PREDICTOR[class].batch(ITERATIONS[class])
}

#[inline(always)]
unsafe fn bulk_fill_batch(class: usize) -> usize {
    BULK_FILL_PREDICTOR[class].batch(ITERATIONS[class])
}

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
        let (extra, extra_tail, extra_count) = RSEQ_CACHE.try_pop(class, missing, cpu_id);
        if extra_count > 0 && !extra.is_null() {
            RSEQ_CACHE.mail_push_batch(class, extra.as_ptr(), extra_tail.as_ptr(), cpu_id);
        }

        if count + extra_count < wanted {
            crate::REFILL_OVER_PREDICTS.fetch_add(1, Ordering::Relaxed);
        }
        return;
    }

    if wanted >= 8 && wanted < ITERATIONS[class] {
        let (extra, extra_tail, extra_count) = RSEQ_CACHE.try_pop(class, 1, cpu_id);
        if extra_count > 0 && !extra.is_null() {
            crate::REFILL_UNDER_PREDICTS.fetch_add(1, Ordering::Relaxed);
            RSEQ_CACHE.mail_push_batch(class, extra.as_ptr(), extra_tail.as_ptr(), cpu_id);
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

    if count > 1 {
        #[cfg(feature = "debug")]
        record_refill_prediction(class, count, wanted, cpu_id, can_probe_more);

        let rest = (*first).next;
        if !rest.is_null() {
            RSEQ_CACHE.push_tailed(class, rest, tail, count - 1);
        }
    }

    (*first).next = null_mut();
    UnsafePointer::new(first)
}

#[unsafe(link_section = ".text.hot")]
#[cold]
#[inline(never)]
pub unsafe fn refill(class: usize, cpu_id: usize) -> UnsafePointer<Header> {
    for _ in 0..MAX_REFILL_RETRIES {
        let bulk_batch = bulk_fill_batch(class);
        match bulk_fill(class, cpu_id, bulk_batch) {
            Ok((start, tail, count)) => {
                let observed = if count == bulk_batch && bulk_batch < ITERATIONS[class] {
                    bulk_batch
                        .saturating_add((bulk_batch / 4).max(1))
                        .min(ITERATIONS[class])
                } else {
                    count
                };

                BULK_FILL_PREDICTOR[class].update_refill(observed, 1, ITERATIONS[class]);
                return take_one_from_batch(
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
            }
            Err(_) => continue,
        }
    }

    RSEQ_CACHE.try_pop(class, 1, cpu_id).0
}

#[unsafe(link_section = ".text.hot")]
#[inline(never)]
pub unsafe fn fill(class: usize) -> UnsafePointer<Header> {
    let cache_batch = refill_batch(class);
    let cpu_id = get_rseq().cpu_id as usize;

    #[cfg(feature = "debug")]
    TOTAL_REFILL_CALLS.fetch_add(1, Ordering::Relaxed);

    let (start, tail, count) = RSEQ_CACHE.try_pop(class, cache_batch, cpu_id);
    if count > 0 {
        let observed = if count == cache_batch && cache_batch < ITERATIONS[class] {
            cache_batch
                .saturating_add((cache_batch / 4).max(1))
                .min(ITERATIONS[class])
        } else {
            count
        };

        PREDICTOR[class].update_refill(observed, 1, ITERATIONS[class]);
        return take_one_from_batch(
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
    }

    refill(class, cpu_id)
}

#[inline(always)]
pub unsafe fn rs_alloc(size: usize, aligned: bool) -> UnsafePointer<Header> {
    #[cfg(feature = "preload")]
    ONCE.call_once(|| crate::core_prim::bootstrap::bootstrap());

    let class = match_size_class(size);

    if let Some(class) = class {
        let cache = RSEQ_CACHE.pop(class);

        let cache = if cache.is_null() {
            let class = fill(class);

            if class.is_null() {
                #[cfg(feature = "preload")]
                {
                    *crate::inner::libc_int::__errno_location() =
                        rustix::io::Errno::NOMEM.raw_os_error();
                }
                return UnsafePointer::NULL;
            }

            class
        } else {
            cache
        };

        let mut safe = cache.apply_safe();
        safe.magic = MAGIC;
        #[cfg(feature = "extended-header")]
        {
            safe.next = null_mut();
        }

        return cache.walk_header();
    }

    let cache = big_malloc(size, aligned);
    cache.cast()
}

#[inline(always)]
pub unsafe fn usable_size(ptr: UnsafePointer<Header>) -> usize {
    let ptr_addr = ptr.cast_usize();

    if likely(L3_RADIX.is_owned(ptr_addr)) {
        let original_payload = find_original_ptr(ptr);
        let original_payload_addr = original_payload.cast_usize();
        let offset = ptr_addr - original_payload_addr;
        let header = original_payload.get_actual_header().apply_safe();

        #[cfg(feature = "canary")]
        header.canary_mismatch(header.as_ptr());

        if header.magic == BIG_MAGIC {
            let meta = BIG_ALLOC_MAP.get(original_payload_addr).unwrap_or_else(|| {
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
