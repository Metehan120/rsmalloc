use std::os::raw::c_void;

use crate::{Header, core_prim::wrappers::UnsafePointer, inner::free::rs_free};

#[unsafe(no_mangle)]
pub unsafe extern "C" fn free(ptr: *mut c_void) {
    rs_free(UnsafePointer::new(ptr as *mut Header));
}

pub static FREE: unsafe extern "C" fn(*mut c_void) = free;

#[unsafe(no_mangle)]
pub unsafe extern "C" fn free_sized(ptr: *mut c_void, _: usize) {
    (FREE)(ptr);
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn free_aligned_sized(ptr: *mut c_void, _: usize, _: usize) {
    (FREE)(ptr);
}
