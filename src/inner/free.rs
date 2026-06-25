use std::{
    hint::{likely, unlikely},
    os::raw::c_void,
    ptr::read_unaligned,
};

use crate::{
    ALIGN_TAG, BIG_MAGIC, FREED_MAGIC, GenericCache, Header, MAGIC, MAGIC_DISABLE, OFFSET_SIZE,
    RSMallocError, TAG_SIZE, big_allocations::big_allocation::big_free,
    core_prim::wrappers::UnsafePointer, internals::l3_main_radix::L3_RADIX,
    rseq_core::rseq_cache::RSEQ_CACHE,
};

#[inline(always)]
pub unsafe fn find_original_ptr(ptr: UnsafePointer<Header>) -> UnsafePointer<Header> {
    let mut header_search_ptr = ptr;
    let tag_loc = (header_search_ptr.cast_usize()).wrapping_sub(TAG_SIZE) as *const usize;

    if read_unaligned(tag_loc) == ALIGN_TAG {
        let raw_loc = (header_search_ptr.cast_usize()).wrapping_sub(OFFSET_SIZE) as *const usize;
        let presumed_original_ptr = read_unaligned(raw_loc) as *mut c_void;
        header_search_ptr = UnsafePointer::new(presumed_original_ptr as *mut Header);
    }

    header_search_ptr
}

#[inline(always)]
pub unsafe fn rs_free(ptr: UnsafePointer<Header>) {
    if unlikely(ptr.is_null()) {
        return;
    }

    if !L3_RADIX.is_owned(ptr.cast_usize()) {
        #[cfg(feature = "preload")]
        crate::inner::fallback::free_fallback(ptr.cast_as_ptr() as *mut c_void);

        #[cfg(not(feature = "preload"))]
        {
            use crate::FOREIGN_POINTER_ABORT;

            if FOREIGN_POINTER_ABORT {
                crate::RSMallocError::ForeignPointer.log_and_abort(
                    ptr.as_ptr() as *mut c_void,
                    "Foreign pointer",
                    None,
                );
            }
        }

        return;
    }

    let searched = find_original_ptr(ptr);
    let mut header = searched.cast::<Header>().get_actual_header().apply_safe();

    #[cfg(feature = "canary")]
    header.canary_mismatch(header.cast_as_ptr());

    if likely(header.magic == MAGIC) {
        header.magic = FREED_MAGIC;

        RSEQ_CACHE.push(header.class as usize, header.as_ptr());
        return;
    }

    if likely(header.magic == BIG_MAGIC) {
        big_free(searched.cast_usize());
        return;
    }

    if !MAGIC_DISABLE {
        if header.magic == FREED_MAGIC {
            RSMallocError::DoubleFree.log_and_abort(header.cast_as_ptr(), "Magic mismatch", None)
        }

        RSMallocError::AttackOrCorruption.log_and_abort(
            header.cast_as_ptr(),
            "Magic mismatch",
            None,
        )
    }
}
