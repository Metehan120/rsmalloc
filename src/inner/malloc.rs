use std::{
    hint::{likely, unlikely},
    ptr::null_mut,
};

use crate::{
    BIG_MAGIC, GenericCache, Header, MAGIC, RSMallocError,
    big_allocation::big_malloc,
    core_prim::{bootstrap::bootstrap, wrappers::UnsafePointer},
    inner::{fallback::malloc_usable_size_fallback, free::find_original_ptr},
    internals::{hashmap::BIG_ALLOC_MAP, l3_main_radix::L3_RADIX, once::Once},
    rseq_core::{bulk_core::FreeList, bulk_fill::bulk_fill, rseq_cache::RSEQ_CACHE},
    utility::{RSEQ_MAX_BLOCKS, SIZE_CLASSES, match_size_class},
};

static ONCE: Once = Once::new();
const REFILL_ATTEMPTS: usize = 3;
const FILL_ATTEMPTS: usize = 2;

#[inline(always)]
unsafe fn refill(class: usize) {
    let batch = RSEQ_MAX_BLOCKS[class].max(1);
    let thread = FreeList::get_or_init();

    for _ in 0..REFILL_ATTEMPTS {
        let (start, tail, count) = RSEQ_CACHE.try_pop(class, batch);
        if !start.is_null() {
            RSEQ_CACHE.push_tailed(class, start.as_ptr(), tail.as_ptr(), count);
            return;
        }

        if bulk_fill(thread, class).is_err() {
            continue;
        }
    }
}

#[inline(never)]
pub unsafe fn fill(class: usize) -> UnsafePointer<Header> {
    let batch = RSEQ_MAX_BLOCKS[class].max(1);

    refill(class);

    for _ in 0..FILL_ATTEMPTS {
        let local = RSEQ_CACHE.pop_non_inline(class);
        if !local.is_null() {
            return local;
        }

        let (start, tail, count) = RSEQ_CACHE.try_pop(class, batch);
        if !start.is_null() {
            let first = start.as_ptr();

            if count > 1 {
                let rest = (*first).next;
                if !rest.is_null() {
                    RSEQ_CACHE.push_tailed(class, rest, tail.as_ptr(), count - 1);
                }
            }

            (*first).next = null_mut();
            return UnsafePointer::new(first);
        }
    }

    RSEQ_CACHE.try_pop(class, 1).0
}

#[inline(always)]
pub unsafe fn alloc(size: usize) -> UnsafePointer<Header> {
    ONCE.call_once(|| bootstrap());

    let class = match_size_class(size);

    if let Some(class) = class {
        let cache = RSEQ_CACHE.pop(class);

        let cache = if unlikely(cache.is_null()) {
            let class = fill(class);

            if class.is_null() {
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
            let meta = BIG_ALLOC_MAP.get(base_addr).unwrap_or_else(|| {
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
