use std::os::raw::{c_int, c_void};

use crate::{
    Header,
    big_allocations::segmented_bitmap::SEGMENTED_BITMAP_BACKEND,
    core_prim::wrappers::UnsafePointer,
    inner::alloc::{rs_alloc, usable_size},
    rseq_core::slab_cache::SLAB_CACHE,
};

#[unsafe(no_mangle)]
pub unsafe extern "C" fn malloc(size: usize) -> *mut c_void {
    rs_alloc(size, false).cast_as_ptr()
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn malloc_usable_size(ptr: *mut c_void) -> usize {
    usable_size(UnsafePointer::new(ptr as *mut Header)) as usize
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn malloc_trim(requested_size: usize) -> c_int {
    let segmented_bitmap_trim = SEGMENTED_BITMAP_BACKEND.trim_old(requested_size);
    let mut small_trim = 0;
    if requested_size.saturating_sub(segmented_bitmap_trim) != 0 || requested_size == 0 {
        small_trim = SLAB_CACHE.trim_small(requested_size.saturating_sub(segmented_bitmap_trim));
    }
    let total = segmented_bitmap_trim.saturating_add(small_trim);

    if total != 0 { 1 } else { 0 }
}
