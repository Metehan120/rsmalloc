#[cfg(feature = "allocator-api")]
use std::alloc::AllocError;
use std::{
    alloc::{GlobalAlloc, Layout},
    num::NonZero,
    ptr::NonNull,
};

pub use crate::frontend::global_alloc2::{debug::*, raw::*};
use crate::{
    GLOBAL_ALLOC_ONCE, Header,
    backend::bootstrap::main_bootstrap,
    big_allocations::segmented_bitmap::SEGMENTED_BITMAP_BACKEND,
    core_prim::wrappers::UnsafePointer,
    inner::{
        align::memalign_inner,
        alloc::{rs_alloc, usable_size},
        calloc::{rs_calloc, zero},
        free::rs_free,
        realloc::rs_realloc,
    },
    rseq_core::slab_cache::SLAB_CACHE,
    utility::likely,
    v2::{
        allocation_api::{AllocationAPI, AllocationError, AllocationSize},
        config::Config,
    },
};

pub trait RSMallocCoreAPI {
    type TrimIn;
    type TrimOut;

    fn trim(&self, size: Self::TrimIn) -> Self::TrimOut;
    fn rs_usable_size(&self, pointer: NonNull<u8>) -> Option<usize>;
    fn manual_init(&self);
}

/// The v2 Rust global allocator.
///
/// Construct this in a `static` and install it with `#[global_allocator]`, or
/// use it with allocator-aware collections through [`std::alloc::Allocator`].
/// That interface reports the requested size and backs zero-sized layouts with
/// real allocations, which must also be deallocated through the allocator.
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

    #[cfg(feature = "allocator-api")]
    #[inline(always)]
    fn allocate_layout<const ZEROED: bool>(
        &self,
        layout: Layout,
    ) -> Result<NonNull<[u8]>, AllocError> {
        unsafe {
            self.init();
            let pointer = if likely(layout.align() <= 16) {
                if ZEROED {
                    rs_calloc(1, layout.size())
                } else {
                    rs_alloc(layout.size(), false)
                }
            } else {
                let pointer = Self::memalign_non_inline(layout.align(), layout.size());
                if ZEROED && !pointer.is_null() {
                    zero(pointer.cast_as_ptr(), layout.size());
                }
                pointer
            };
            let pointer = NonNull::new(pointer.cast_as_ptr::<u8>()).ok_or(AllocError)?;
            Ok(NonNull::slice_from_raw_parts(pointer, layout.size()))
        }
    }

    #[cfg(feature = "allocator-api")]
    #[inline(always)]
    unsafe fn resize_layout(
        &self,
        pointer: NonNull<u8>,
        new_layout: Layout,
    ) -> Result<NonNull<[u8]>, AllocError> {
        self.init();
        let pointer = rs_realloc(
            UnsafePointer::new(pointer.as_ptr()).cast(),
            new_layout.size().max(1),
            Some(new_layout.align()),
        );
        let pointer = NonNull::new(pointer.cast_as_ptr::<u8>()).ok_or(AllocError)?;
        Ok(NonNull::slice_from_raw_parts(pointer, new_layout.size()))
    }
}

unsafe impl GlobalAlloc for RSMalloc {
    #[inline]
    unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
        unsafe { self.init() };

        if likely(layout.align() <= 16) {
            rs_alloc(layout.size(), false).cast_as_ptr()
        } else {
            Self::memalign_non_inline(layout.align(), layout.size()).cast_as_ptr()
        }
    }

    #[inline]
    unsafe fn dealloc(&self, ptr: *mut u8, _: Layout) {
        rs_free(UnsafePointer::new(ptr as *mut Header));
    }

    #[inline]
    unsafe fn realloc(&self, ptr: *mut u8, _: Layout, new_size: usize) -> *mut u8 {
        unsafe { self.init() };
        rs_realloc(UnsafePointer::new(ptr as *mut Header), new_size, None).cast_as_ptr()
    }

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

#[cfg(feature = "allocator-api")]
unsafe impl std::alloc::Allocator for RSMalloc {
    #[inline]
    fn allocate(&self, layout: Layout) -> Result<NonNull<[u8]>, AllocError> {
        self.allocate_layout::<false>(layout)
    }

    #[inline]
    fn allocate_zeroed(&self, layout: Layout) -> Result<NonNull<[u8]>, AllocError> {
        self.allocate_layout::<true>(layout)
    }

    #[inline]
    unsafe fn deallocate(&self, pointer: NonNull<u8>, _: Layout) {
        rs_free(UnsafePointer::new(pointer.as_ptr()).cast());
    }

    #[inline]
    unsafe fn grow(
        &self,
        pointer: NonNull<u8>,
        _: Layout,
        new_layout: Layout,
    ) -> Result<NonNull<[u8]>, AllocError> {
        self.resize_layout(pointer, new_layout)
    }

    #[inline]
    unsafe fn grow_zeroed(
        &self,
        pointer: NonNull<u8>,
        old_layout: Layout,
        new_layout: Layout,
    ) -> Result<NonNull<[u8]>, AllocError> {
        let block = self.resize_layout(pointer, new_layout)?;
        zero(
            block.cast::<u8>().as_ptr().add(old_layout.size()),
            new_layout.size() - old_layout.size(),
        );
        Ok(block)
    }

    #[inline]
    unsafe fn shrink(
        &self,
        pointer: NonNull<u8>,
        _: Layout,
        new_layout: Layout,
    ) -> Result<NonNull<[u8]>, AllocError> {
        self.resize_layout(pointer, new_layout)
    }
}

unsafe impl AllocationAPI for RSMalloc {
    type Size = AllocationSize;

    #[inline]
    fn allocate(&self, size: Self::Size) -> Result<NonNull<u8>, AllocationError> {
        unsafe { self.init() };
        let pointer = unsafe { rs_alloc(size.bytes(), false) };

        NonNull::new(pointer.cast_as_ptr()).ok_or(AllocationError::OutOfMemory)
    }

    #[inline]
    fn allocate_zeroed(&self, size: Self::Size) -> Result<NonNull<u8>, AllocationError> {
        unsafe { self.init() };
        let pointer = unsafe { rs_calloc(1, size.bytes()) };

        NonNull::new(pointer.cast_as_ptr()).ok_or(AllocationError::OutOfMemory)
    }

    #[inline]
    fn allocate_aligned(
        &self,
        size: Self::Size,
        alignment: usize,
    ) -> Result<NonNull<u8>, AllocationError> {
        unsafe { self.init() };
        if !alignment.is_power_of_two() {
            return Err(AllocationError::InvalidAlignment);
        }

        let pointer = unsafe { memalign_inner(alignment, size.bytes(), true) };

        NonNull::new(pointer.cast_as_ptr()).ok_or(AllocationError::OutOfMemory)
    }

    #[inline]
    unsafe fn deallocate(&self, pointer: NonNull<u8>) {
        rs_free(UnsafePointer::new(pointer.as_ptr()).cast());
    }

    #[inline]
    unsafe fn reallocate(
        &self,
        pointer: NonNull<u8>,
        new_size: Self::Size,
    ) -> Result<NonNull<u8>, AllocationError> {
        self.init();

        let size = new_size.bytes();
        if size == 0 {
            return Err(AllocationError::NotSupported);
        }

        let pointer = rs_realloc(UnsafePointer::new(pointer.as_ptr()).cast(), size, None);
        NonNull::new(pointer.cast_as_ptr()).ok_or(AllocationError::OutOfMemory)
    }

    #[inline]
    unsafe fn usable_size(&self, pointer: NonNull<u8>) -> Result<usize, AllocationError> {
        self.init();
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

    fn rs_usable_size(&self, pointer: NonNull<u8>) -> Option<usize> {
        unsafe { self.init() };

        let usable = unsafe { usable_size(UnsafePointer::new(pointer.as_ptr()).cast()) };
        (usable != 0).then_some(usable)
    }

    #[inline(never)]
    fn trim(&self, size: Self::TrimIn) -> Self::TrimOut {
        unsafe { self.init() };

        let requested = size.get_size();
        let size = unsafe { SEGMENTED_BITMAP_BACKEND.trim(requested) };
        if size < requested && requested != 0 {
            let small = unsafe { SLAB_CACHE.trim_small(requested.saturating_sub(size)) };
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
