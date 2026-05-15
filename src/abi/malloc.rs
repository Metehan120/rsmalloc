use std::os::raw::c_void;

use crate::{
    Header,
    core_prim::wrappers::UnsafePointer,
    inner::malloc::{alloc, usable_size},
};

#[unsafe(no_mangle)]
pub unsafe extern "C" fn malloc(size: usize) -> *mut c_void {
    alloc(size).cast_as_ptr()
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn malloc_usable_size(ptr: *mut c_void) -> usize {
    usable_size(UnsafePointer::new(ptr as *mut Header)) as usize
}
