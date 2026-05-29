use std::{alloc::Layout, os::raw::c_void};

use rustix::io::Errno;

use crate::{
    Header, RSMallocError,
    core_prim::wrappers::UnsafePointer,
    inner::{
        libc_int::{__errno_location, NOMEM},
        malloc::rs_alloc,
    },
    internals::hashmap::BIG_ALLOC_MAP,
    utility::{SIZE_CLASSES, match_size_class},
};

#[inline(always)]
unsafe fn calc_and_get(size: Layout, nmem: usize) -> Option<(UnsafePointer<Header>, usize)> {
    let size = size.size();
    let total_size = match nmem.checked_mul(size) {
        Some(s) => s,
        None => {
            *__errno_location() = Errno::NOMEM.raw_os_error();
            return None;
        }
    };

    let effective_size = if total_size == 0 { 1 } else { total_size };
    let ptr = rs_alloc(effective_size);
    if ptr.is_null() {
        return None;
    }
    Some((ptr, effective_size))
}

#[inline(always)]
pub unsafe fn rs_calloc(size: usize, zero_size: usize) -> UnsafePointer<Header> {
    let layout = match Layout::array::<u8>(size) {
        Ok(layout) => layout,
        Err(_) => {
            *__errno_location() = NOMEM;
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
            std::ptr::write_bytes(
                ptr.cast_as_ptr() as *mut u8,
                0,
                actual_size.min(effective_size) as usize,
            );

            ptr
        }
        None => {
            let payload_size = BIG_ALLOC_MAP
                .get(ptr.cast_usize())
                .map(|meta| meta.size)
                .unwrap_or_else(|| {
                    RSMallocError::AttackOrCorruption.log_and_abort(
                        header.as_ptr() as *mut c_void,
                        "missing big allocation metadata during calloc",
                        None,
                    )
                });

            std::ptr::write_bytes(
                ptr.cast_as_ptr() as *mut u8,
                0,
                payload_size.min(effective_size) as usize,
            );

            ptr
        }
    }
}
