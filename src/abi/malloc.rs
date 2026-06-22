use std::os::raw::{c_int, c_void};

use crate::{
    ENABLE_TRIM, Header,
    big_allocations::buddy::BIG_BUDDY_ALLOCATOR,
    core_prim::wrappers::UnsafePointer,
    inner::alloc::{rs_alloc, usable_size},
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
    if ENABLE_TRIM {
        if BIG_BUDDY_ALLOCATOR.trim(requested_size) >= requested_size {
            1
        } else {
            0
        }
    } else {
        0
    }
}
