use std::os::raw::{c_int, c_void};

use crate::{
    Header,
    core_prim::wrappers::MaybeNull,
    inner::{
        alloc::{rs_alloc, usable_size},
        preload::libc_int::set_nomem,
    },
};

#[unsafe(no_mangle)]
pub unsafe extern "C" fn malloc(size: usize) -> *mut c_void {
    let ptr = rs_alloc(size, false);
    if ptr.is_null() {
        set_nomem();
    }
    ptr.cast_as_ptr()
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn malloc_usable_size(ptr: *mut c_void) -> usize {
    usable_size(MaybeNull::new(ptr as *mut Header)) as usize
}

/// Posts an asynchronous trim request and always returns 1.
///
/// Unlike glibc, the argument is a reclaim target (zero means unbounded), not
/// retained padding. The return value does not indicate memory was released.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn malloc_trim(requested_size: usize) -> c_int {
    crate::frontend::post_trim_request(requested_size);
    1
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::inner::preload::libc_int::{__errno_location, NOMEM};

    #[test]
    fn malloc_overflow_sets_enomem() {
        unsafe {
            *__errno_location() = 0;
            // Keep LLVM from eliding a malloc whose storage is never used.
            let allocate =
                std::hint::black_box(malloc as unsafe extern "C" fn(usize) -> *mut c_void);
            let ptr = allocate(std::hint::black_box(usize::MAX));
            let errno = *__errno_location();
            assert!(ptr.is_null());
            assert_eq!(errno, NOMEM);
        }
    }
}
