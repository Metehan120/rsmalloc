use std::{os::raw::c_void, ptr::null_mut};

use rustix::io::Errno;

use crate::{
    Header, OFFSET_SIZE, TAG_SIZE, core_prim::wrappers::UnsafePointer, inner::alloc::rs_alloc,
};

#[inline(always)]
pub unsafe fn posix_align_inner(memptr: *mut *mut c_void, alignment: usize, size: usize) -> i32 {
    if memptr.is_null() {
        return Errno::INVAL.raw_os_error();
    }

    let min = size_of::<*mut c_void>();
    if alignment < min || !alignment.is_power_of_two() {
        return Errno::INVAL.raw_os_error();
    }

    let Some(total_requested) = size
        .checked_add(alignment)
        .and_then(|v| v.checked_add(TAG_SIZE))
    else {
        return Errno::NOMEM.raw_os_error();
    };

    let raw = rs_alloc(total_requested, true);
    if raw.is_null() {
        return Errno::NOMEM.raw_os_error();
    }

    let addr = raw.cast_usize();
    let start_search = addr.saturating_add(TAG_SIZE);
    let aligned = (start_search + alignment - 1) & !(alignment - 1);

    let tag_location = aligned.saturating_sub(TAG_SIZE) as *mut usize;
    let original_ptr_location = aligned.saturating_sub(OFFSET_SIZE) as *mut usize;
    *tag_location = crate::ALIGN_TAG;
    *original_ptr_location = raw.cast_usize();

    *memptr = aligned as *mut c_void;
    0
}

#[inline(always)]
pub unsafe fn memalign_inner(
    alignment: usize,
    size: usize,
    skip_checks: bool,
) -> UnsafePointer<Header> {
    let mut ptr: *mut c_void = null_mut();
    let adjusted_alignment = alignment.max(size_of::<*mut c_void>());

    if !adjusted_alignment.is_power_of_two() && !skip_checks {
        #[cfg(feature = "preload")]
        {
            use crate::inner::preload::libc_int::__errno_location;
            *__errno_location() = Errno::INVAL.raw_os_error();
        }
        return UnsafePointer::NULL;
    }

    let success = posix_align_inner(&mut ptr, adjusted_alignment, size);

    if success == 0 {
        UnsafePointer::new(ptr as *mut Header)
    } else {
        #[cfg(feature = "preload")]
        {
            use crate::inner::preload::libc_int::__errno_location;
            *__errno_location() = success;
        }

        UnsafePointer::NULL
    }
}
