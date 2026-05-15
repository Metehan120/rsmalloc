use std::os::raw::c_void;

use crate::inner::calloc::rs_calloc;

#[unsafe(no_mangle)]
pub unsafe extern "C" fn calloc(nmemb: usize, size: usize) -> *mut c_void {
    rs_calloc(size, nmemb).cast_as_ptr()
}

pub static CALLOC: unsafe extern "C" fn(nmemb: usize, size: usize) -> *mut c_void = calloc;
