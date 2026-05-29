use std::{os::raw::c_void, ptr::null_mut};

use rustix::io::Errno;

use crate::{
    Header, OFFSET_SIZE, TAG_SIZE, core_prim::wrappers::UnsafePointer, inner::malloc::rs_alloc,
};

#[inline(always)]
pub unsafe fn align_inner(memptr: *mut *mut c_void, alignment: usize, size: usize) -> i32 {
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

    let raw = rs_alloc(total_requested);
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
pub unsafe fn memalign_inner(alignment: usize, size: usize) -> UnsafePointer<Header> {
    let mut ptr: *mut c_void = null_mut();
    let adjusted_alignment = if alignment < 8 { 8 } else { alignment };

    if !adjusted_alignment.is_power_of_two() {
        return UnsafePointer::NULL;
    }

    if align_inner(&mut ptr, adjusted_alignment, size) == 0 {
        UnsafePointer::new(ptr as *mut Header)
    } else {
        UnsafePointer::NULL
    }
}
