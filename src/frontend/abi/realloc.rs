use std::{
    os::raw::c_void,
    ptr::{copy_nonoverlapping, null_mut},
};

use rustix::io::Errno;

use crate::{
    Header,
    core_prim::wrappers::MaybeNull,
    inner::{
        calloc::rs_calloc,
        free::rs_free,
        preload::libc_int::{__errno_location, NOMEM, set_nomem},
        realloc::rs_realloc,
    },
};

#[unsafe(no_mangle)]
pub unsafe extern "C" fn realloc(ptr: *mut c_void, new_size: usize) -> *mut c_void {
    let new_ptr = rs_realloc(MaybeNull::new(ptr as *mut Header), new_size);
    if new_ptr.is_null() && new_size != 0 {
        set_nomem();
    }
    new_ptr.cast_as_ptr()
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

unsafe extern "C" {
    fn explicit_bzero(ptr: *mut c_void, len: usize);
}

/// Resizes an array, zeroing added bytes and explicitly clearing discarded storage.
///
/// # Safety
///
/// A non-null `ptr` must be the start of a live allocation, and `oldnmemb * size`
/// must accurately describe its old array size. On failure, the original
/// allocation and its contents remain unchanged.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn recallocarray(
    ptr: *mut c_void,
    oldnmemb: usize,
    newnmemb: usize,
    size: usize,
) -> *mut c_void {
    if ptr.is_null() {
        let new_ptr = rs_calloc(size, newnmemb);
        if new_ptr.is_null() {
            set_nomem();
        }
        return new_ptr.cast_as_ptr();
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
        None => {
            *__errno_location() = Errno::INVAL.raw_os_error();
            return null_mut();
        }
    };

    if new_size == 0 {
        explicit_bzero(ptr, old_size);
        rs_free(MaybeNull::new(ptr as *mut Header));
        return null_mut();
    }

    if new_size <= old_size {
        explicit_bzero((ptr as *mut u8).add(new_size).cast(), old_size - new_size);
        return ptr;
    }

    // realloc may release the old block before we can erase it.
    let new_ptr = rs_calloc(1, new_size);
    if new_ptr.is_null() {
        set_nomem();
        return null_mut();
    }

    copy_nonoverlapping(ptr as *const u8, new_ptr.cast_as_ptr::<u8>(), old_size);
    explicit_bzero(ptr, old_size);
    rs_free(MaybeNull::new(ptr as *mut Header));

    new_ptr.cast_as_ptr()
}
