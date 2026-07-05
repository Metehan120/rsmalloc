use std::os::raw::{c_int, c_void};

use crate::{
    Header,
    big_allocations::buddy::BIG_BUDDY_ALLOCATOR,
    core_prim::wrappers::UnsafePointer,
    inner::alloc::{rs_alloc, usable_size},
    trim::trim_small,
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
    let buddy_trim = BIG_BUDDY_ALLOCATOR.trim_old(requested_size);

    if requested_size != 0 && buddy_trim >= requested_size {
        return 1;
    }

    let small_trim = trim_small(requested_size.saturating_sub(buddy_trim));
    let total = buddy_trim.saturating_add(small_trim);

    if requested_size == 0 {
        (total > 0) as c_int
    } else {
        (total >= requested_size) as c_int
    }
}
