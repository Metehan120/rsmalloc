use std::num::NonZero;

use crate::{
    backend::trim::trim_small,
    big_allocations::buddy::BUDDY_BACKEND,
    core_prim::wrappers::UnsafePointer,
    inner::{
        align::memalign_inner, alloc::rs_alloc, calloc::rs_calloc, free::rs_free,
        realloc::rs_realloc,
    },
    traits::global_alloc::RawInterface,
    v2::alloc::RSMalloc,
};

pub struct RSMallocRaw {
    global: &'static RSMalloc,
}

impl RSMalloc {
    pub const fn raw(&'static self) -> RSMallocRaw {
        RSMallocRaw::from_global(self)
    }
}

impl RSMallocRaw {
    pub const fn from_global(global: &'static RSMalloc) -> RSMallocRaw {
        RSMallocRaw { global }
    }
}

pub enum AdvancedTrimSize {
    All,
    AllSlab,
    AllBuddy,
    Bytes(NonZero<usize>),
    SlabBytes(NonZero<usize>),
    BuddyBytes(NonZero<usize>),
}

pub struct TrimReport {
    pub buddy_bytes: usize,
    pub slab_bytes: usize,
}

impl RawInterface for RSMallocRaw {
    type TrimIn = AdvancedTrimSize;
    type TrimOut = TrimReport;

    unsafe fn rs_alloc(&self, size: usize) -> *mut u8 {
        self.global.manual_init();
        rs_alloc(size, false).cast_as_ptr()
    }

    unsafe fn rs_free(&self, ptr: *mut u8) {
        rs_free(UnsafePointer::new(ptr).cast());
    }

    unsafe fn rs_aligned(&self, alignment: usize, size: usize) -> *mut u8 {
        self.global.manual_init();
        memalign_inner(alignment, size).cast_as_ptr()
    }

    unsafe fn rs_realloc(&self, old: *mut u8, new_size: usize) -> *mut u8 {
        self.global.manual_init();
        rs_realloc(UnsafePointer::new(old).cast(), new_size).cast_as_ptr()
    }

    unsafe fn rs_zeroed(&self, size: usize) -> *mut u8 {
        self.global.manual_init();
        rs_calloc(1, size).cast_as_ptr()
    }

    #[inline(never)]
    unsafe fn trim(&self, trim_size: Self::TrimIn) -> Self::TrimOut {
        self.global.manual_init();
        match trim_size {
            AdvancedTrimSize::All => {
                let buddy = BUDDY_BACKEND.trim(0);
                let slab = trim_small(0);
                TrimReport {
                    buddy_bytes: buddy,
                    slab_bytes: slab,
                }
            }
            AdvancedTrimSize::AllBuddy => {
                let buddy = BUDDY_BACKEND.trim(0);
                TrimReport {
                    buddy_bytes: buddy,
                    slab_bytes: 0,
                }
            }
            AdvancedTrimSize::AllSlab => {
                let slab = trim_small(0);
                TrimReport {
                    buddy_bytes: 0,
                    slab_bytes: slab,
                }
            }
            AdvancedTrimSize::Bytes(requested) => {
                let requested = requested.get();

                let buddy = BUDDY_BACKEND.trim(requested);
                let mut slab = 0;
                if buddy < requested && requested != 0 {
                    slab = trim_small(requested.saturating_sub(buddy));
                }
                TrimReport {
                    buddy_bytes: buddy,
                    slab_bytes: slab,
                }
            }
            AdvancedTrimSize::BuddyBytes(requested) => {
                let requested = requested.get();

                let buddy = BUDDY_BACKEND.trim(requested);
                TrimReport {
                    buddy_bytes: buddy,
                    slab_bytes: 0,
                }
            }
            AdvancedTrimSize::SlabBytes(requested) => {
                let requested = requested.get();

                let slab = trim_small(requested);
                TrimReport {
                    buddy_bytes: 0,
                    slab_bytes: slab,
                }
            }
        }
    }
}
