use std::os::raw::{c_int, c_void};

use crate::{
    Header,
    core_prim::wrappers::UnsafePointer,
    inner::malloc::{rs_alloc, usable_size},
};

#[unsafe(no_mangle)]
pub unsafe extern "C" fn malloc(size: usize) -> *mut c_void {
    rs_alloc(size).cast_as_ptr()
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn malloc_usable_size(ptr: *mut c_void) -> usize {
    usable_size(UnsafePointer::new(ptr as *mut Header)) as usize
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn malloc_trim(_: usize) -> c_int {
    1
}
