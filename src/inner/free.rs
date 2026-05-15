use std::{hint::unlikely, os::raw::c_void};

use crate::{
    ALIGN_TAG, BIG_MAGIC, FREED_MAGIC, GenericCache, Header, MAGIC, OFFSET_SIZE, RSMallocError,
    TAG_SIZE, big_allocation::big_free, core_prim::wrappers::UnsafePointer,
    inner::fallback::free_fallback, internals::l3_main_radix::L3_RADIX,
    rseq_core::rseq_cache::RSEQ_CACHE,
};

pub unsafe fn find_original_ptr(ptr: UnsafePointer<Header>) -> UnsafePointer<Header> {
    let mut header_search_ptr = ptr;
    let tag_loc = (header_search_ptr.cast_usize()).wrapping_sub(TAG_SIZE) as *const usize;

    if std::ptr::read_unaligned(tag_loc) == ALIGN_TAG {
        let raw_loc = (header_search_ptr.cast_usize()).wrapping_sub(OFFSET_SIZE) as *const usize;
        let presumed_original_ptr = std::ptr::read_unaligned(raw_loc) as *mut c_void;
        header_search_ptr = UnsafePointer::new(presumed_original_ptr as *mut Header);
    }

    header_search_ptr
}

#[inline(always)]
pub unsafe fn rs_free(ptr: UnsafePointer<Header>) {
    if unlikely(!L3_RADIX.is_owned(ptr.cast_usize())) {
        free_fallback(ptr.cast_as_ptr());
        return;
    }

    let searched = find_original_ptr(ptr);

    let mut header = searched.cast::<Header>().get_actual_header().apply_safe();

    if header.magic != MAGIC {
        if header.magic == BIG_MAGIC {
            big_free(searched.cast_usize());
            return;
        }

        if header.magic == FREED_MAGIC {
            RSMallocError::DoubleFree.log_and_abort(*header.cast(), "Magic mismatch", None)
        }

        RSMallocError::AttackOrCorruption.log_and_abort(*header.cast(), "Magic mismatch", None)
    };

    header.magic = FREED_MAGIC;

    let class = header.class as usize;
    RSEQ_CACHE.push(class, header.as_ptr());
}
