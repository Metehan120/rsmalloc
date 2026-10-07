use std::{
    os::raw::c_void,
    ptr::{copy_nonoverlapping, null_mut},
};

use rustix::io::Errno;

use crate::{
    Header,
    core_prim::wrappers::UnsafePointer,
    inner::{
        calloc::rs_calloc,
        free::rs_free,
        preload::libc_int::{__errno_location, NOMEM, set_nomem},
        realloc::rs_realloc,
    },
};

#[unsafe(no_mangle)]
pub unsafe extern "C" fn realloc(ptr: *mut c_void, new_size: usize) -> *mut c_void {
    let new_ptr = rs_realloc(UnsafePointer::new(ptr as *mut Header), new_size);
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
        rs_free(UnsafePointer::new(ptr as *mut Header));
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
    rs_free(UnsafePointer::new(ptr as *mut Header));

    new_ptr.cast_as_ptr()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::frontend::abi::{align::memalign, malloc::malloc};

    #[test]
    fn realloc_null_overflow_sets_enomem() {
        unsafe {
            *__errno_location() = 0;
            let resize = std::hint::black_box(
                realloc as unsafe extern "C" fn(*mut c_void, usize) -> *mut c_void,
            );
            let ptr = resize(null_mut(), std::hint::black_box(usize::MAX));
            let errno = *__errno_location();
            assert!(ptr.is_null());
            assert_eq!(errno, NOMEM);
        }
    }

    #[test]
    fn realloc_overflow_preserves_old_allocation() {
        unsafe {
            // Exercise small, big, and explicitly aligned replacement paths.
            for (size, alignment) in [(64, 0), (65536, 0), (64, 4096)] {
                let ptr = if alignment == 0 {
                    malloc(size)
                } else {
                    memalign(alignment, size)
                };
                assert!(!ptr.is_null());
                std::ptr::write_bytes(ptr.cast::<u8>(), 0x5a, size);
                *__errno_location() = 0;
                let new_ptr = realloc(ptr, std::hint::black_box(usize::MAX));
                let errno = *__errno_location();
                assert!(new_ptr.is_null());
                assert_eq!(errno, NOMEM);
                assert!(
                    std::slice::from_raw_parts(ptr.cast::<u8>(), size)
                        .iter()
                        .all(|&byte| byte == 0x5a)
                );
                rs_free(UnsafePointer::new(ptr.cast::<Header>()));
            }
        }
    }

    #[test]
    fn reallocarray_overflow_preserves_old_allocation() {
        unsafe {
            let ptr = malloc(64);
            assert!(!ptr.is_null());
            std::ptr::write_bytes(ptr.cast::<u8>(), 0xa5, 64);
            // Product overflow and a valid product rejected by realloc's allocation path.
            for (nmemb, size) in [(usize::MAX, 2), (1, usize::MAX)] {
                *__errno_location() = 0;
                let new_ptr = reallocarray(ptr, std::hint::black_box(nmemb), size);
                let errno = *__errno_location();
                assert!(new_ptr.is_null());
                assert_eq!(errno, NOMEM);
                assert!(
                    std::slice::from_raw_parts(ptr.cast::<u8>(), 64)
                        .iter()
                        .all(|&byte| byte == 0xa5)
                );
            }
            rs_free(UnsafePointer::new(ptr.cast::<Header>()));
        }
    }

    #[test]
    fn realloc_to_zero_keeps_errno() {
        unsafe {
            for size in [64, 65536] {
                let ptr = malloc(size);
                assert!(!ptr.is_null());
                let sentinel = Errno::INVAL.raw_os_error();
                *__errno_location() = sentinel;
                let new_ptr = realloc(ptr, 0);
                let errno = *__errno_location();
                assert!(new_ptr.is_null());
                assert_eq!(errno, sentinel);
            }
        }
    }
}

#[cfg(test)]
mod recalloc_tests {
    use super::*;

    unsafe fn patterned_allocation(size: usize) -> *mut c_void {
        let ptr = rs_calloc(1, size).cast_as_ptr::<c_void>();
        assert!(!ptr.is_null());
        std::ptr::write_bytes(ptr.cast::<u8>(), 0xa5, size);
        ptr
    }

    unsafe fn assert_pattern(ptr: *mut c_void, size: usize, pattern: u8) {
        assert!(
            std::slice::from_raw_parts(ptr.cast::<u8>(), size)
                .iter()
                .all(|&byte| byte == pattern)
        );
    }

    #[test]
    fn recallocarray_null_zeroes_and_ignores_old_count() {
        unsafe {
            let ptr = recallocarray(null_mut(), usize::MAX, 16, 8);
            assert!(!ptr.is_null());
            assert_pattern(ptr, 128, 0);
            rs_free(UnsafePointer::new(ptr.cast::<Header>()));
        }
    }

    #[test]
    fn recallocarray_shrink_zeroes_suffix_in_place() {
        unsafe {
            let ptr = patterned_allocation(128);
            let new_ptr = recallocarray(ptr, 128, 32, 1);
            assert_eq!(new_ptr, ptr);
            assert_pattern(new_ptr, 32, 0xa5);
            // The implementation retains the original allocation when shrinking.
            assert_pattern(new_ptr.cast::<u8>().add(32).cast(), 96, 0);
            rs_free(UnsafePointer::new(new_ptr.cast::<Header>()));
        }
    }

    #[test]
    fn recallocarray_growth_moves_preserves_prefix_and_zeroes_extension() {
        unsafe {
            for (old_count, new_count, size) in
                [(21, 23, 4), (16, 512, 8), (16384, 3 * 1024 * 1024, 1)]
            {
                let old_size = old_count * size;
                let new_size = new_count * size;
                let ptr = patterned_allocation(old_size);
                let new_ptr = recallocarray(ptr, old_count, new_count, size);
                assert!(!new_ptr.is_null());
                assert_ne!(new_ptr, ptr);
                assert_pattern(new_ptr, old_size, 0xa5);
                assert_pattern(
                    new_ptr.cast::<u8>().add(old_size).cast(),
                    new_size - old_size,
                    0,
                );
                rs_free(UnsafePointer::new(new_ptr.cast::<Header>()));
            }
        }
    }

    #[test]
    fn recallocarray_equal_size_preserves_allocation() {
        unsafe {
            let ptr = patterned_allocation(128);
            let new_ptr = recallocarray(ptr, 16, 16, 8);
            assert_eq!(new_ptr, ptr);
            assert_pattern(new_ptr, 128, 0xa5);
            rs_free(UnsafePointer::new(new_ptr.cast::<Header>()));
        }
    }

    #[test]
    fn recallocarray_zero_frees_and_returns_null() {
        unsafe {
            let ptr = patterned_allocation(128);
            let new_ptr = recallocarray(ptr, 16, 0, 8);
            assert!(new_ptr.is_null());
        }
    }

    #[test]
    fn recallocarray_new_product_overflow_preserves_old_allocation() {
        unsafe {
            let ptr = patterned_allocation(128);
            *__errno_location() = 0;
            let new_ptr = recallocarray(ptr, 16, usize::MAX, 8);
            let errno = *__errno_location();
            assert!(new_ptr.is_null());
            assert_eq!(errno, NOMEM);
            assert_pattern(ptr, 128, 0xa5);
            rs_free(UnsafePointer::new(ptr.cast::<Header>()));
        }
    }

    #[test]
    fn recallocarray_old_product_overflow_preserves_old_allocation() {
        unsafe {
            let ptr = patterned_allocation(128);
            *__errno_location() = 0;
            let new_ptr = recallocarray(ptr, usize::MAX, 16, 8);
            let errno = *__errno_location();
            assert!(new_ptr.is_null());
            assert_eq!(errno, Errno::INVAL.raw_os_error());
            assert_pattern(ptr, 128, 0xa5);
            rs_free(UnsafePointer::new(ptr.cast::<Header>()));
        }
    }

    #[test]
    fn recallocarray_allocation_failure_preserves_old_allocation() {
        unsafe {
            let ptr = patterned_allocation(128);
            *__errno_location() = 0;
            let new_ptr = recallocarray(ptr, 128, std::hint::black_box(usize::MAX), 1);
            let errno = *__errno_location();
            assert!(new_ptr.is_null());
            assert_eq!(errno, NOMEM);
            assert_pattern(ptr, 128, 0xa5);
            rs_free(UnsafePointer::new(ptr.cast::<Header>()));
        }
    }
}
