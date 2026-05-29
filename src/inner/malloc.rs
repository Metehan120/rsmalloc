use std::{
    hint::{likely, unlikely},
    ptr::null_mut,
};

use crate::{
    BIG_MAGIC, GenericCache, Header, MAGIC, RSMallocError,
    big_allocations::big_allocation::big_malloc,
    core_prim::{bootstrap::bootstrap, predictor::PREDICTOR, wrappers::UnsafePointer},
    inner::{
        fallback::malloc_usable_size_fallback,
        free::find_original_ptr,
        libc_int::{__errno_location, NOMEM},
    },
    internals::{hashmap::BIG_ALLOC_MAP, l3_main_radix::L3_RADIX, once::Once},
    rseq_core::{
        bulk_core::FreeList, bulk_fill::bulk_fill, rseq_cache::RSEQ_CACHE, rseq_main::get_rseq,
    },
    utility::{ITERATIONS, SIZE_CLASSES, match_size_class},
};

static ONCE: Once = Once::new();

#[inline(always)]
unsafe fn refill_batch(class: usize) -> usize {
    PREDICTOR[class].batch(ITERATIONS[class])
}

#[inline(always)]
unsafe fn take_one_from_batch(
    class: usize,
    start: *mut Header,
    tail: *mut Header,
    count: usize,
) -> UnsafePointer<Header> {
    let first = start;

    if count > 1 {
        let rest = (*first).next;
        if !rest.is_null() {
            RSEQ_CACHE.push_tailed(class, rest, tail, count - 1);
        }
    }

    (*first).next = null_mut();
    UnsafePointer::new(first)
}

#[inline(never)]
pub unsafe fn fill(class: usize) -> UnsafePointer<Header> {
    let batch = refill_batch(class);
    let cpu_id = get_rseq().cpu_id as usize;

    let (start, tail, count) = RSEQ_CACHE.try_pop(class, batch, cpu_id);
    if count > 0 {
        PREDICTOR[class].update_refill(count, 1, ITERATIONS[class]);
        return take_one_from_batch(class, start.as_ptr(), tail.as_ptr(), count);
    }

    let thread = FreeList::get_or_init();
    match bulk_fill(thread, class, batch) {
        Ok((start, tail, count)) => {
            PREDICTOR[class].update_refill(count, 1, ITERATIONS[class]);
            take_one_from_batch(class, start, tail, count)
        }
        Err(_) => RSEQ_CACHE.try_pop(class, 1, cpu_id).0,
    }
}

#[inline(always)]
pub unsafe fn rs_alloc(size: usize) -> UnsafePointer<Header> {
    ONCE.call_once(|| bootstrap());

    let class = match_size_class(size);

    if let Some(class) = class {
        let cache = RSEQ_CACHE.pop(class);

        let cache = if unlikely(cache.is_null()) {
            let class = fill(class);

            if class.is_null() {
                *__errno_location() = NOMEM;
                return UnsafePointer::NULL;
            }

            class
        } else {
            cache
        };

        cache.apply_safe().magic = MAGIC;
        return cache.walk_header();
    }

    let cache = big_malloc(size);
    cache.cast()
}

#[inline(always)]
pub unsafe fn usable_size(ptr: UnsafePointer<Header>) -> usize {
    let ptr_addr = ptr.cast_usize();

    if likely(L3_RADIX.is_owned(ptr_addr)) {
        let base_ptr = find_original_ptr(ptr);
        let base_addr = base_ptr.cast_usize();

        let offset = ptr_addr - base_addr;

        let header = base_ptr.get_actual_header().apply_safe();

        if header.magic == BIG_MAGIC {
            let meta = BIG_ALLOC_MAP
                .get(base_addr + Header::SIZE)
                .unwrap_or_else(|| {
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

    malloc_usable_size_fallback(ptr.cast_as_ptr())
}
