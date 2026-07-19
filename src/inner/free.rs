use std::{hint::unlikely, os::raw::c_void, ptr::read_unaligned, sync::atomic::Ordering};

use crate::{
    ALIGN_TAG, BIG_MAGIC, CURRENT_STAMP, FREED_MAGIC, GenericCache, Header, MAGIC, MAGIC_DISABLE,
    OFFSET_SIZE, RSMallocError, TAG_SIZE, big_allocations::big_allocation::big_free,
    core_prim::wrappers::UnsafePointer, internals::l3_main_radix::RADIX,
    rseq_core::slab_cache::SLAB_CACHE,
};

#[inline(always)]
pub unsafe fn find_original_ptr(ptr: UnsafePointer<Header>) -> UnsafePointer<Header> {
    let mut header_search_ptr = ptr;
    let tag_loc = (header_search_ptr.cast_usize()).wrapping_sub(TAG_SIZE) as *const usize;

    if read_unaligned(tag_loc) == ALIGN_TAG {
        let raw_loc = (header_search_ptr.cast_usize()).wrapping_sub(OFFSET_SIZE) as *const usize;
        let presumed_original_ptr = read_unaligned(raw_loc) as *mut c_void;

        if !RADIX.is_owned(presumed_original_ptr as usize) {
            RSMallocError::AttackOrCorruption.log_and_abort(
                header_search_ptr.as_ptr() as *mut c_void,
                "CRITICAL: possible aligned-path metadata injection: recovered pointer is not owned by rsmalloc",
                None,
            );
        }

        header_search_ptr = UnsafePointer::new(presumed_original_ptr as *mut Header);
    }

    header_search_ptr
}

#[inline(always)]
pub unsafe fn rs_free(ptr: UnsafePointer<Header>) {
    if unlikely(ptr.is_null()) {
        return;
    }

    if !RADIX.is_owned(ptr.cast_usize()) {
        if unlikely(!RADIX.is_valid_user_addr(ptr.cast_usize())) {
            RSMallocError::InvalidPointer.log_and_abort(
                ptr.as_ptr() as *mut c_void,
                "invalid pointer adress",
                None,
            );
        }

        #[cfg(feature = "preload")]
        crate::inner::fallback::free_fallback(ptr.cast_as_ptr() as *mut c_void);

        #[cfg(not(feature = "preload"))]
        {
            if crate::FOREIGN_POINTER_ABORT {
                RSMallocError::ForeignPointer.log_and_abort(
                    ptr.as_ptr() as *mut c_void,
                    "Foreign pointer",
                    None,
                );
            }
        }

        return;
    }

    let searched = find_original_ptr(ptr.cast());
    let mut header = searched.cast::<Header>().get_actual_header().apply_safe();

    if header.magic == MAGIC {
        header.life_time = CURRENT_STAMP.load(Ordering::Relaxed);
        header.magic = FREED_MAGIC;

        SLAB_CACHE.push(header.class as usize, header.as_ptr());
        return;
    }

    if header.magic == BIG_MAGIC {
        big_free(searched.cast_usize());
        return;
    }

    if !MAGIC_DISABLE {
        if header.magic == FREED_MAGIC {
            RSMallocError::DoubleFree.log_and_abort(header.cast_as_ptr(), "magic mismatch", None)
        }

        RSMallocError::AttackOrCorruption.log_and_abort(
            header.cast_as_ptr(),
            "magic mismatch",
            None,
        )
    }
}
