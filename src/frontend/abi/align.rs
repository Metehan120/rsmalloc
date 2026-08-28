use std::{
    os::raw::{c_int, c_void},
    ptr::null_mut,
};

use rustix::io::Errno;

use crate::inner::{
    align::{memalign_inner, posix_align_inner},
    preload::libc_int::__errno_location,
};

#[unsafe(no_mangle)]
pub unsafe extern "C" fn posix_memalign(
    memptr: *mut *mut c_void,
    alignment: usize,
    size: usize,
) -> c_int {
    posix_align_inner(memptr, alignment, size)
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn memalign(alignment: usize, size: usize) -> *mut c_void {
    memalign_inner(alignment, size).cast_as_ptr()
}

static MEMALIGN: unsafe extern "C" fn(alignment: usize, size: usize) -> *mut c_void = memalign;

#[unsafe(no_mangle)]
pub unsafe extern "C" fn aligned_alloc(alignment: usize, size: usize) -> *mut c_void {
    if alignment == 0 || !alignment.is_power_of_two() || !size.is_multiple_of(alignment) {
        *__errno_location() = Errno::INVAL.raw_os_error();
        return null_mut();
    }

    (MEMALIGN)(alignment, size)
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn valloc(size: usize) -> *mut c_void {
    (MEMALIGN)(4096, size)
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn pvalloc(size: usize) -> *mut c_void {
    let page_size = 4096;
    let rounded_size = if size == 0 {
        page_size
    } else {
        match size.checked_add(page_size - 1) {
            Some(v) => v & !(page_size - 1),
            None => return null_mut(),
        }
    };

    (MEMALIGN)(page_size, rounded_size)
}
