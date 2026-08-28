use std::{
    alloc::{GlobalAlloc, Layout},
    num::NonZero,
    ptr::NonNull,
};

#[cfg(any(feature = "allocator-api", doc))]
use std::{
    alloc::{AllocError, Allocator},
    ptr::copy_nonoverlapping,
};

use crate::{
    GLOBAL_ALLOC_ONCE, Header,
    backend::bootstrap::main_bootstrap,
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

#[cfg(any(feature = "allocator-api", doc))]
unsafe impl Allocator for RSMalloc {
    fn allocate(&self, layout: Layout) -> Result<NonNull<[u8]>, std::alloc::AllocError> {
        unsafe { self.init() };

        let size = layout.size();
        let allocated = if layout.align() <= 16 {
            unsafe { rs_alloc(size, false) }
        } else {
            unsafe { Self::memalign_non_inline(layout.align(), size) }
        };

        match NonNull::new(allocated.as_ptr() as *mut u8) {
            Some(pointer) => Ok(NonNull::slice_from_raw_parts(pointer, size)),
            None => Err(AllocError),
        }
    }

    unsafe fn deallocate(&self, ptr: NonNull<u8>, _: Layout) {
        self.init();
        rs_free(UnsafePointer::new(ptr.as_ptr() as *mut Header));
    }

    fn allocate_zeroed(&self, layout: Layout) -> Result<NonNull<[u8]>, AllocError> {
        unsafe { self.init() };

        let size = layout.size();
        let allocated = if likely(layout.align() <= 16) {
            unsafe { rs_calloc(1, size).cast_as_ptr() }
        } else {
            let ptr = unsafe { self.alloc_non_inline(layout) };
            if !ptr.is_null() {
                unsafe { zero(ptr, size) };
            }
            ptr
        };

        match NonNull::new(allocated) {
            Some(pointer) => Ok(NonNull::slice_from_raw_parts(pointer, size)),
            None => Err(AllocError),
        }
    }

    unsafe fn grow(
        &self,
        ptr: NonNull<u8>,
        old_layout: Layout,
        new_layout: Layout,
    ) -> Result<NonNull<[u8]>, AllocError> {
        self.init();

        if new_layout.size() < old_layout.size() {
            return Err(AllocError);
        }

        if new_layout.align() > old_layout.align() {
            let allocated = if new_layout.align() <= 16 {
                rs_alloc(new_layout.size(), false)
            } else {
                Self::memalign_non_inline(new_layout.align(), new_layout.size())
            };

            let Some(new_ptr) = NonNull::new(allocated.as_ptr() as *mut u8) else {
                return Err(AllocError);
            };

            copy_nonoverlapping(ptr.as_ptr(), new_ptr.as_ptr(), old_layout.size());
            rs_free(UnsafePointer::new(ptr.as_ptr() as *mut Header));

            return Ok(NonNull::slice_from_raw_parts(new_ptr, new_layout.size()));
        }

        if new_layout.size() == 0 {
            return Ok(NonNull::slice_from_raw_parts(ptr, 0));
        }

        let allocated = rs_realloc(
            UnsafePointer::new(ptr.as_ptr() as *mut Header),
            new_layout.size(),
        );

        match NonNull::new(allocated.as_ptr() as *mut u8) {
            Some(pointer) => Ok(NonNull::slice_from_raw_parts(pointer, new_layout.size())),
            None => Err(AllocError),
        }
    }

    unsafe fn shrink(
        &self,
        ptr: NonNull<u8>,
        old: Layout,
        new: Layout,
    ) -> Result<NonNull<[u8]>, AllocError> {
        self.init();

        if new.size() > old.size() {
            return Err(AllocError);
        }

        if (ptr.as_ptr() as usize) % new.align() == 0 {
            return Ok(NonNull::slice_from_raw_parts(ptr, new.size()));
        }

        let allocated = if new.align() <= 16 {
            rs_alloc(new.size(), false)
        } else {
            Self::memalign_non_inline(new.align(), new.size())
        };

        let Some(new_ptr) = NonNull::new(allocated.as_ptr() as *mut u8) else {
            return Err(AllocError);
        };

        copy_nonoverlapping(ptr.as_ptr(), new_ptr.as_ptr(), new.size());
        rs_free(UnsafePointer::new(ptr.as_ptr() as *mut Header));

        Ok(NonNull::slice_from_raw_parts(new_ptr, new.size()))
    }

    fn by_ref(&self) -> &Self
    where
        Self: Sized,
    {
        self
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
