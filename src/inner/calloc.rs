use std::{alloc::Layout, hint::unlikely, os::raw::c_void};

#[cfg(feature = "preload")]
use crate::inner::libc_int::set_nomem;
use crate::{
    Flags, Header, RSMallocError,
    core_prim::wrappers::UnsafePointer,
    inner::alloc::rs_alloc,
    internals::rbtree::BIG_META_MAP,
    utility::{SIZE_CLASSES, match_size_class},
};

pub unsafe fn zero(pointer: *mut u8, len: usize) {
    if cfg!(feature = "explicit-zero") {
        unsafe extern "C" {
            fn explicit_bzero(s: *mut c_void, len: usize);
        }

        explicit_bzero(pointer as *mut c_void, len);
    } else {
        std::ptr::write_bytes(pointer, 0, len);
    }
}

macro_rules! calloc_zero {
    ($header:expr, $ptr:expr, $actual_size:expr, $effective_size:expr) => {
        let flags = unsafe { (*$header.as_ptr()).flags };

        #[cfg(not(feature = "lazy-page-trim"))]
        if flags == Flags::Allocated || flags == Flags::BigAlloc {
            zero(
                $ptr.cast_as_ptr() as *mut u8,
                $actual_size.min($effective_size),
            )
        }

        #[cfg(feature = "lazy-page-trim")]
        if flags == Flags::Allocated || flags == Flags::Trimmed || flags == Flags::BigAlloc {
            zero(
                $ptr.cast_as_ptr() as *mut u8,
                $actual_size.min($effective_size),
            )
        }
    };
}

#[inline(never)]
unsafe fn calc_and_get(size: Layout, nmem: usize) -> Option<(UnsafePointer<Header>, usize)> {
    let size = size.size();
    let total_size = match nmem.checked_mul(size) {
        Some(s) => s,
        None => {
            #[cfg(feature = "preload")]
            set_nomem();

            return None;
        }
    };

    let effective_size = total_size.max(1);

    let ptr = rs_alloc(effective_size, false);
    if unlikely(ptr.is_null()) {
        return None;
    }
    Some((ptr, effective_size))
}

#[inline(always)]
pub unsafe fn rs_calloc(size: usize, zero_size: usize) -> UnsafePointer<Header> {
    let layout = match Layout::array::<u8>(size) {
        Ok(layout) => layout,
        Err(_) => {
            #[cfg(feature = "preload")]
            set_nomem();
            return UnsafePointer::NULL;
        }
    };

    let (ptr, effective_size) = match calc_and_get(layout, zero_size) {
        Some(ptr) => ptr,
        None => return UnsafePointer::NULL,
    };

    let header = ptr.get_actual_header();

    match match_size_class(effective_size) {
        Some(class) => {
            let actual_size = SIZE_CLASSES[class];

            calloc_zero!(header, ptr, actual_size, effective_size);

            ptr
        }
        None => {
            let payload_size = BIG_META_MAP
                .get(ptr.cast_usize())
                .map(|meta| meta.size)
                .unwrap_or_else(|| {
                    RSMallocError::AttackOrCorruption.log_and_abort(
                        header.as_ptr() as *mut c_void,
                        "missing big allocation metadata during calloc",
                        None,
                    )
                });

            calloc_zero!(header, ptr, payload_size, effective_size);

            ptr
        }
    }
}
