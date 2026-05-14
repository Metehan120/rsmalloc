use std::{cell::UnsafeCell, hint::likely, ptr::null_mut};

use crate::{MetaData, rseq_core::bulk_fill::drain_pending, utility::NUM_SIZE_CLASSES};

pub struct Destructor;

impl Drop for Destructor {
    fn drop(&mut self) {
        for i in 0..NUM_SIZE_CLASSES {
            unsafe { drain_pending(&mut BULK_FREE_LIST, i) };
        }
    }
}

pub struct FreeList {
    pub free: [*mut MetaData; NUM_SIZE_CLASSES],
    destructor: UnsafeCell<Option<Destructor>>,
    init: bool,
}

impl FreeList {
    pub const fn new() -> FreeList {
        FreeList {
            free: [const { null_mut() }; NUM_SIZE_CLASSES],
            destructor: UnsafeCell::new(None),
            init: false,
        }
    }
}

#[thread_local]
pub static mut BULK_FREE_LIST: FreeList = FreeList::new();

#[inline(always)]
unsafe fn touch_tls() {
    let slot = BULK_FREE_LIST.destructor.get();
    core::ptr::write_volatile(slot, Some(Destructor));
}

impl FreeList {
    pub unsafe fn get_or_init() -> &'static mut FreeList {
        let bulk = &mut BULK_FREE_LIST;
        if likely(bulk.init) {
            return bulk;
        }
        touch_tls();
        bulk.init = true;
        bulk
    }
}
