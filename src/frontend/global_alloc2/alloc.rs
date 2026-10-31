#[cfg(feature = "allocator-api")]
use std::alloc::AllocError;
use std::{
    alloc::{GlobalAlloc, Layout},
    num::NonZero,
    ptr::NonNull,
};

/// Acknowledgement of a background trim message, not a reclamation result.
#[must_use]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RSTrimStatus {
    /// A message was already pending; no additional pass is queued.
    AlreadyRequested,
    /// A new message was posted for the existing background worker.
    Requested,
}

pub(crate) fn request_trim(size: usize) -> RSTrimStatus {
    if crate::frontend::post_trim_request(size) {
        RSTrimStatus::Requested
    } else {
        RSTrimStatus::AlreadyRequested
    }
}

#[cfg(any(feature = "debug", doc))]
pub use crate::frontend::global_alloc2::debug::*;
pub use crate::frontend::global_alloc2::raw::*;
#[cfg(any(feature = "native-allocation-api", doc))]
use crate::v2::allocation_api::{AllocationAPI, AllocationError, AllocationSize};
use crate::{
    GLOBAL_ALLOC_ONCE, Header,
    backend::bootstrap::main_bootstrap,
    core_prim::wrappers::MaybeNull,
    inner::{
        align::memalign_inner,
        alloc::{rs_alloc, usable_size},
        calloc::{rs_calloc, zero},
        free::rs_free,
        realloc::rs_realloc,
    },
    utility::likely,
    v2::config::Config,
};

/// Allocator-specific maintenance and inspection operations.
///
/// These methods are separate from [`AllocationAPI`]: they do not allocate or
/// transfer ownership of a block. `RSMalloc` implements this trait whether or
/// not the optional native allocation interface is enabled.
pub trait RSMallocCoreAPI {
    type TrimIn;
    type TrimOut;

    /// Requests a best-effort trim of eligible cached pages.
    ///
    /// For `RSMalloc`, returns [`RSTrimStatus::Requested`] or
    /// [`RSTrimStatus::AlreadyRequested`], not a reclaimed byte count. Requests
    /// are coalesced and processed asynchronously by the existing background
    /// worker; this call does not start or wake it or wait for completion.
    /// Live allocations remain valid. Reclamation is best-effort, subject to
    /// worker eligibility gates, and may not immediately reduce RSS.
    fn trim(&self, size: Self::TrimIn) -> Self::TrimOut;
    /// Returns the usable payload size of a live rsmalloc allocation.
    ///
    /// The result can exceed the originally requested size. Only bytes the
    /// caller initialized may be read, regardless of the reported capacity.
    /// `None` means the allocator did not report a usable size.
    unsafe fn rs_usable_size(&self, pointer: NonNull<u8>) -> Option<usize>;
    /// Initializes allocator-wide state ahead of the first allocation.
    ///
    /// Ordinary allocation calls initialize it automatically.
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
    unsafe fn memalign_non_inline(align: usize, size: usize) -> MaybeNull<Header> {
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
            MaybeNull::new(pointer.as_ptr()).cast(),
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
        rs_free(MaybeNull::new(ptr as *mut Header));
    }

    #[inline]
    unsafe fn realloc(&self, ptr: *mut u8, _: Layout, new_size: usize) -> *mut u8 {
        unsafe { self.init() };
        rs_realloc(MaybeNull::new(ptr as *mut Header), new_size, None).cast_as_ptr()
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
        rs_free(MaybeNull::new(pointer.as_ptr()).cast());
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

#[cfg(any(feature = "native-allocation-api", doc))]
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

    fn allocate_zeroed_nmem(
        &self,
        nmem: Self::Size,
        zero_size: Self::Size,
    ) -> Result<NonNull<u8>, AllocationError> {
        unsafe { self.init() };
        let pointer = unsafe { rs_calloc(nmem.bytes(), zero_size.bytes()) };

        NonNull::new(pointer.cast_as_ptr()).ok_or(AllocationError::SomethingWentWrong)
    }

    #[inline]
    fn allocate_aligned(
        &self,
        size: Self::Size,
        alignment: NonZero<usize>,
    ) -> Result<NonNull<u8>, AllocationError> {
        unsafe { self.init() };
        if !alignment.is_power_of_two() {
            return Err(AllocationError::InvalidAlignment);
        }

        let pointer = unsafe { memalign_inner(alignment.get(), size.bytes(), true) };

        NonNull::new(pointer.cast_as_ptr()).ok_or(AllocationError::OutOfMemory)
    }

    #[inline]
    unsafe fn deallocate(&self, pointer: *mut u8) {
        rs_free(MaybeNull::new(pointer).cast());
    }

    #[inline]
    unsafe fn usable_size(&self, pointer: NonNull<u8>) -> Result<usize, AllocationError> {
        self.init();
        let size = usable_size(MaybeNull::new(pointer.as_ptr()).cast());
        if size != 0 {
            return Ok(size);
        }
        Err(AllocationError::NotOwned)
    }

    #[inline]
    unsafe fn reallocate(
        &self,
        pointer: *mut u8,
        new_size: Self::Size,
    ) -> Result<*mut u8, AllocationError> {
        self.init();
        if new_size.bytes() == 0 && pointer.is_null() {
            return Err(AllocationError::NotSupported);
        }

        let size = new_size.bytes();
        let pointer = rs_realloc(MaybeNull::new(pointer).cast(), size, None);
        if size == 0 || !pointer.is_null() {
            return Ok(pointer.cast_as_ptr());
        }
        Err(AllocationError::OutOfMemory)
    }

    unsafe fn aligned_reallocate(
        &self,
        pointer: *mut u8,
        new_size: Self::Size,
        new_alignment: NonZero<usize>,
    ) -> Result<*mut u8, AllocationError> {
        self.init();
        if !new_alignment.is_power_of_two() {
            return Err(AllocationError::InvalidAlignment);
        }
        if new_size.bytes() == 0 && pointer.is_null() {
            return Err(AllocationError::NotSupported);
        }

        let size = new_size.bytes();
        let pointer = rs_realloc(
            MaybeNull::new(pointer).cast(),
            size,
            Some(new_alignment.get()),
        );
        if size == 0 || !pointer.is_null() {
            return Ok(pointer.cast_as_ptr());
        }
        Err(AllocationError::OutOfMemory)
    }
}

/// Trim request used by [`RSMallocCoreAPI`] on [`RSMalloc`].
pub enum SimpleTrimSize {
    /// Requests an asynchronous, unbounded pass over eligible segmented-bitmap
    /// blocks and slab caches. Not all cached pages are necessarily reclaimable.
    All,
    /// Requests this best-effort byte target. The worker scans segmented-bitmap
    /// blocks first and slab caches if the target has not been met.
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
    type TrimOut = RSTrimStatus;

    unsafe fn rs_usable_size(&self, pointer: NonNull<u8>) -> Option<usize> {
        unsafe { self.init() };

        let usable = unsafe { usable_size(MaybeNull::new(pointer.as_ptr()).cast()) };
        (usable != 0).then_some(usable)
    }

    #[inline(never)]
    fn trim(&self, size: Self::TrimIn) -> Self::TrimOut {
        unsafe { self.init() };

        request_trim(size.get_size())
    }

    /// Initializes the allocator manually.
    ///
    /// You can ignore this, rsmalloc will automatically initialize itself on first use.
    #[inline(never)]
    fn manual_init(&self) {
        unsafe { self.init() };
    }
}
