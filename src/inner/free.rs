#[cfg(feature = "debug-full-critic")]
use std::sync::atomic::AtomicUsize;
use std::{hint::unlikely, os::raw::c_void, ptr::read_unaligned, sync::atomic::Ordering};

use crate::{
    ALIGN_TAG, BIG_MAGIC, CURRENT_STAMP, FREED_MAGIC, Header, MAGIC, OFFSET_SIZE, RSMallocError,
    TAG_SIZE, big_allocations::big_allocation::big_free, core_prim::wrappers::UnsafePointer,
    internals::radix_tree::RADIX, rseq_core::slab_cache::SLAB_CACHE, traits::GenericCache,
};

#[cfg(feature = "zero-small-on-free")]
#[inline(always)]
unsafe fn zero_small_payload(header_ptr: *mut Header, class: usize) {
    use crate::utility::SIZE_CLASSES;

    let payload = header_ptr.add(1) as *mut u8;
    match class {
        0 => std::ptr::write_bytes(payload, 0, SIZE_CLASSES[0]),
        1 => std::ptr::write_bytes(payload, 0, SIZE_CLASSES[1]),
        2 => std::ptr::write_bytes(payload, 0, SIZE_CLASSES[2]),
        3 => std::ptr::write_bytes(payload, 0, SIZE_CLASSES[3]),
        _ => {}
    }
}

#[inline(always)]
pub unsafe fn find_original_ptr(ptr: UnsafePointer<Header>) -> UnsafePointer<Header> {
    let mut header_search_ptr = ptr;
    let tag_loc = (header_search_ptr.cast_usize()).wrapping_sub(TAG_SIZE) as *const usize;

    if read_unaligned(tag_loc) == ALIGN_TAG {
        let raw_loc = (header_search_ptr.cast_usize()).wrapping_sub(OFFSET_SIZE) as *const usize;
        let presumed_original_ptr = read_unaligned(raw_loc) as *mut c_void;

        // Do not dereference the recovered aligned allocation base until ownership is
        // verified the offset preceding an arbitrary pointer is untrusted and may
        // contain forged allocator metadata
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

#[cfg(feature = "debug-full-critic")]
pub static RS_FREE_CALLS_DEBUG: AtomicUsize = AtomicUsize::new(0);

#[inline(always)]
pub unsafe fn rs_free(ptr: UnsafePointer<Header>) {
    if unlikely(ptr.is_null()) {
        return;
    }

    #[cfg(feature = "debug-full-critic")]
    RS_FREE_CALLS_DEBUG.fetch_add(1, std::sync::atomic::Ordering::Relaxed);

    // Classify ownership before reading allocator metadata unowned pointers within
    // the supported user-address range follow the configured foreign-pointer
    // policy; addresses outside that range are rejected as invalid
    if !RADIX.is_owned(ptr.cast_usize()) {
        #[cfg(feature = "preload")]
        crate::inner::fallback::free_fallback(ptr.cast_as_ptr() as *mut _);

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

        #[cfg(feature = "zero-small-on-free")]
        zero_small_payload(header.as_ptr(), header.class as usize);

        SLAB_CACHE.push(header.class as usize, header.as_ptr());
        return;
    }

    if header.magic == BIG_MAGIC {
        big_free(searched.cast_usize());
        return;
    }

    // if it is double free, abort just to keep heap intact
    // if it is not double free, we have a memory corruption or a security violation
    if !cfg!(feature = "disable-magic-security-checks") {
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
