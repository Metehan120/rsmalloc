use std::os::raw::c_void;

use crate::inner::{calloc::rs_calloc, preload::libc_int::set_nomem};

#[unsafe(no_mangle)]
pub unsafe extern "C" fn calloc(nmemb: usize, size: usize) -> *mut c_void {
    let ptr = rs_calloc(size, nmemb);
    if ptr.is_null() {
        set_nomem();
    }
    ptr.cast_as_ptr()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::inner::preload::libc_int::{__errno_location, NOMEM};

    #[test]
    fn calloc_overflow_sets_enomem() {
        unsafe {
            // Invalid layout, product overflow, and a valid product too large to allocate.
            for (nmemb, size) in [(1, usize::MAX), (3, usize::MAX / 2), (2, usize::MAX / 2)] {
                *__errno_location() = 0;
                let ptr = calloc(std::hint::black_box(nmemb), std::hint::black_box(size));
                let errno = *__errno_location();
                assert!(ptr.is_null());
                assert_eq!(errno, NOMEM);
            }
        }
    }
}
