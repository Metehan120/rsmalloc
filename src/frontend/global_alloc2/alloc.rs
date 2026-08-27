use std::{
    alloc::{GlobalAlloc, Layout},
    num::NonZero,
    ptr::NonNull,
};

use portable_atomic::hint::likely;

use crate::{
    GLOBAL_ALLOC_ONCE, Header,
    backend::trim::trim_small,
    big_allocations::buddy::BUDDY_BACKEND,
    core_prim::wrappers::UnsafePointer,
    inner::{
        align::memalign_inner,
        alloc::{rs_alloc, usable_size},
        calloc::{rs_calloc, zero},
        free::rs_free,
        realloc::rs_realloc,
    },
    v2::config::Config,
};

pub struct RSMalloc {
    pub(crate) _config: Config,
}

impl RSMalloc {
    pub const fn new(config: Config) -> RSMalloc {
        RSMalloc { _config: config }
    }

    #[inline(always)]
    unsafe fn init(&self) {
        GLOBAL_ALLOC_ONCE.call_once(|| {});
        #[cfg(feature = "debug-printer-thread")]
        crate::debug_printer_thread::start();
    }

    #[inline(never)]
    unsafe fn memalign_non_inline(align: usize, size: usize) -> UnsafePointer<Header> {
        memalign_inner(align, size)
    }

    #[inline(never)]
    unsafe fn alloc_non_inline(&self, layout: Layout) -> *mut u8 {
        self.alloc(layout)
    }
}

unsafe impl GlobalAlloc for RSMalloc {
    /// Allocates memory.
    ///
    /// This path is designed to behave like the POSIX-style rsmalloc allocation
    /// path where that does not conflict with Rust's `GlobalAlloc` contract.
    ///
    /// # Safety
    ///
    /// The caller must uphold Rust's `GlobalAlloc::alloc` safety contract for
    /// `layout`.
    #[inline]
    unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
        unsafe { self.init() };

        if likely(layout.align() <= 16) {
            rs_alloc(layout.size(), false).cast_as_ptr()
        } else {
            Self::memalign_non_inline(layout.align(), layout.size()).cast_as_ptr()
        }
    }

    /// Deallocates memory.
    ///
    /// This path is designed to match the allocator's POSIX-style free behavior
    /// while still being used through Rust's `GlobalAlloc` interface.
    ///
    /// # Safety
    ///
    /// `ptr` must have been allocated by this allocator with a compatible `layout`.
    #[inline]
    unsafe fn dealloc(&self, ptr: *mut u8, _: Layout) {
        rs_free(UnsafePointer::new(ptr as *mut Header));
    }

    /// Reallocates memory.
    ///
    /// This path is designed to follow POSIX-style realloc behavior where that does
    /// not conflict with Rust's `GlobalAlloc` contract.
    ///
    /// # Safety
    ///
    /// `ptr` must have been allocated by this allocator with `layout`, and the
    /// caller must uphold Rust's `GlobalAlloc::realloc` safety contract.
    #[inline]
    unsafe fn realloc(&self, ptr: *mut u8, _: Layout, new_size: usize) -> *mut u8 {
        unsafe { self.init() };

        rs_realloc(UnsafePointer::new(ptr as *mut Header), new_size).cast_as_ptr()
    }

    /// Allocates zeroed memory.
    ///
    /// This path is designed to match POSIX-style calloc behavior where possible.
    ///
    /// # Safety
    ///
    /// The caller must uphold Rust's `GlobalAlloc::alloc_zeroed` safety contract
    /// for `layout`.
    #[inline]
    unsafe fn alloc_zeroed(&self, layout: Layout) -> *mut u8 {
        unsafe { self.init() };

        if likely(layout.align() <= 16) {
            rs_calloc(1, layout.size()).cast_as_ptr()
        } else {
            let ptr = self.alloc_non_inline(layout);
            if !ptr.is_null() {
                zero(ptr, layout.size());
            }
            ptr
        }
    }
}

pub enum SimpleTrimSize {
    All,
    Bytes(NonZero<usize>),
}

impl SimpleTrimSize {
    const fn get_size(&self) -> usize {
        match self {
            SimpleTrimSize::All => 0,
            SimpleTrimSize::Bytes(byte) => byte.get(),
        }
    }
}

impl RSMalloc {
    pub fn usable_size(&self, pointer: NonNull<u8>) -> Option<usize> {
        unsafe { self.init() };

        let usable = unsafe { usable_size(UnsafePointer::new(pointer.as_ptr()).cast()) };
        (usable != 0).then_some(usable)
    }

    #[inline(never)]
    pub fn trim(&self, size: SimpleTrimSize) -> Option<usize> {
        unsafe { self.init() };

        let requested = size.get_size();
        let size = unsafe { BUDDY_BACKEND.trim(requested) };
        if size < requested && requested != 0 {
            let small = unsafe { trim_small(requested.saturating_sub(size)) };
            if small > 0 {
                return Some(size + small);
            }
        }

        None
    }

    /// Initializes the allocator manually.
    ///
    /// You can ignore this, rsmalloc will automatically initialize itself on first use.
    #[inline(never)]
    pub fn manual_init(&self) {
        unsafe { self.init() };
    }
}
