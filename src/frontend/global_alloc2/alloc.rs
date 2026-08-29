use std::{
    alloc::{GlobalAlloc, Layout},
    num::NonZero,
    ptr::NonNull,
};

use crate::{
    GLOBAL_ALLOC_ONCE, Header,
    backend::{bootstrap::main_bootstrap, trim::trim_small},
    big_allocations::buddy::BUDDY_BACKEND,
    core_prim::wrappers::UnsafePointer,
    inner::{
        align::memalign_inner,
        alloc::{rs_alloc, usable_size},
        calloc::{rs_calloc, zero},
        free::rs_free,
        realloc::rs_realloc,
    },
    v2::{
        allocation_api::{AllocationAPI, AllocationError, AllocationSize},
        config::Config,
    },
};
use portable_atomic::hint::likely;

pub trait RSMallocCoreAPI {
    type TrimIn;
    type TrimOut;

    fn trim(&self, size: Self::TrimIn) -> Self::TrimOut;
    fn usable_size(&self, pointer: NonNull<u8>) -> Option<usize>;
    fn manual_init(&self);
}

/// The v2 Rust global allocator.
///
/// Construct this in a `static` and install it with `#[global_allocator]`.
pub struct RSMalloc {
    pub(crate) config: Config,
}

impl RSMalloc {
    /// Creates an allocator using the supplied v2 configuration.
    ///
    /// The configuration belonging to the first allocator that initializes is
    /// applied process-wide. Later instances do not reconfigure global state.
    pub const fn new(config: Config) -> RSMalloc {
        RSMalloc { config }
    }

    /// Creates an allocator using [`Config::DEFAULT`].
    pub const fn new_default() -> RSMalloc {
        Self::new(Config::DEFAULT)
    }

    #[inline(always)]
    unsafe fn init(&self) {
        GLOBAL_ALLOC_ONCE.call_once(|| unsafe {
            main_bootstrap(self.config.bootstrap());
        });
        #[cfg(feature = "debug-printer-thread")]
        crate::debug_printer_thread::start();
    }

    #[inline(never)]
    unsafe fn memalign_non_inline(align: usize, size: usize) -> UnsafePointer<Header> {
        memalign_inner(align, size, false)
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

unsafe impl AllocationAPI for RSMalloc {
    type Size = AllocationSize;

    fn allocate(&self, size: Self::Size) -> Result<NonNull<u8>, AllocationError> {
        let pointer = unsafe { rs_alloc(size.bytes(), false) };

        NonNull::new(pointer.cast_as_ptr()).ok_or(AllocationError::OutOfMemory)
    }

    fn allocate_zeroed(&self, size: Self::Size) -> Result<NonNull<u8>, AllocationError> {
        let pointer = unsafe { rs_calloc(1, size.bytes()) };

        NonNull::new(pointer.cast_as_ptr()).ok_or(AllocationError::OutOfMemory)
    }

    fn allocate_aligned(
        &self,
        size: Self::Size,
        alignment: usize,
    ) -> Result<NonNull<u8>, AllocationError> {
        if !alignment.is_power_of_two() {
            return Err(AllocationError::InvalidAlignment);
        }

        let pointer = unsafe { memalign_inner(alignment, size.bytes(), true) };

        NonNull::new(pointer.cast_as_ptr()).ok_or(AllocationError::OutOfMemory)
    }

    unsafe fn deallocate(&self, pointer: NonNull<u8>) {
        rs_free(UnsafePointer::new(pointer.as_ptr()).cast());
    }

    unsafe fn reallocate(
        &self,
        pointer: NonNull<u8>,
        new_size: Self::Size,
    ) -> Result<NonNull<u8>, AllocationError> {
        let pointer = rs_realloc(
            UnsafePointer::new(pointer.as_ptr()).cast(),
            new_size.bytes(),
        );

        NonNull::new(pointer.cast_as_ptr()).ok_or(AllocationError::OutOfMemory)
    }

    unsafe fn usable_size(&self, pointer: NonNull<u8>) -> Result<usize, AllocationError> {
        let size = usable_size(UnsafePointer::new(pointer.as_ptr()).cast());
        if size != 0 {
            return Ok(size);
        }
        Err(AllocationError::NotOwned)
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

impl RSMallocCoreAPI for RSMalloc {
    type TrimIn = SimpleTrimSize;
    type TrimOut = Option<usize>;

    fn usable_size(&self, pointer: NonNull<u8>) -> Option<usize> {
        unsafe { self.init() };

        let usable = unsafe { usable_size(UnsafePointer::new(pointer.as_ptr()).cast()) };
        (usable != 0).then_some(usable)
    }

    #[inline(never)]
    fn trim(&self, size: Self::TrimIn) -> Self::TrimOut {
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
    fn manual_init(&self) {
        unsafe { self.init() };
    }
}
