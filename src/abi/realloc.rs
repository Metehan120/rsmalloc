use std::{os::raw::c_void, ptr::null_mut};

use crate::{
    Header,
    core_prim::wrappers::UnsafePointer,
    inner::{
        calloc::rs_calloc,
        libc_int::{__errno_location, NOMEM},
        realloc::rs_realloc,
    },
};

#[unsafe(no_mangle)]
pub unsafe extern "C" fn realloc(ptr: *mut c_void, new_size: usize) -> *mut c_void {
    rs_realloc(UnsafePointer::new(ptr as *mut Header), new_size).cast_as_ptr()
}

static REALLOC: unsafe extern "C" fn(*mut c_void, usize) -> *mut c_void = realloc;

#[unsafe(no_mangle)]
pub unsafe extern "C" fn reallocarray(ptr: *mut c_void, nmemb: usize, size: usize) -> *mut c_void {
    let total_size = match nmemb.checked_mul(size) {
        Some(s) => s,
        None => {
            if let Some(errno_ptr) = __errno_location().as_mut() {
                *errno_ptr = NOMEM;
            }
            return null_mut();
        }
    };

    (REALLOC)(ptr, total_size)
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn recallocarray(
    ptr: *mut c_void,
    oldnmemb: usize,
    newnmemb: usize,
    size: usize,
) -> *mut c_void {
    if ptr.is_null() {
        return rs_calloc(size, newnmemb).cast_as_ptr();
    }

    let new_size = match newnmemb.checked_mul(size) {
        Some(s) => s,
        None => {
            if let Some(errno_ptr) = __errno_location().as_mut() {
                *errno_ptr = NOMEM;
            }
            return null_mut();
        }
    };

    let old_size = match oldnmemb.checked_mul(size) {
        Some(s) => s,
        None => 0,
    };

    if new_size <= old_size {
        return (REALLOC)(ptr, new_size);
    }

    let new_ptr = (REALLOC)(ptr, new_size);
    if !new_ptr.is_null() {
        let grow_size = new_size - old_size;
        std::ptr::write_bytes((new_ptr as *mut u8).add(old_size), 0, grow_size);
    }

    new_ptr
}
