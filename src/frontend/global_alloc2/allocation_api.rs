//! General-purpose allocation interface used by rsmalloc's native API.
//!
//! [`AllocationAPI`] deliberately models malloc-style allocation: allocations
//! carry their own metadata, deallocation does not require the original layout,
//! and reallocation preserves the existing alignment. This interface is
//! independent of Rust's `GlobalAlloc` and unstable `Allocator` traits.
//!
//! Enable the `native-allocation-api` Cargo feature to use this module. A
//! [`AllocationSize`] is a byte count, not a Rust `Layout`: it carries no
//! alignment or element type. Use [`AllocationAPI::allocate_aligned`] when the
//! result will be used as a type with alignment greater than the allocator's
//! default.
//!
//! Successful allocations remain live until they are passed to
//! [`AllocationAPI::deallocate`] or successfully reallocated. The caller must
//! keep track of the returned pointer; dropping a raw pointer does not free
//! its allocation. Do not mix this interface's pointers with a different
//! allocator's deallocation functions.
//!
//! ```rust
//! use rsmalloc::v2::{
//!     alloc::RSMalloc,
//!     allocation_api::{AllocationAPI, AllocationError, AllocationSize},
//! };
//!
//! static ALLOCATOR: RSMalloc = RSMalloc::new_default();
//!
//! fn example() -> Result<(), AllocationError> {
//!     let pointer = ALLOCATOR.allocate(AllocationSize::from_bytes(128))?;
//!     // Use the first 128 bytes through `pointer.as_ptr()` here.
//!     unsafe { ALLOCATOR.deallocate(pointer.as_ptr()) };
//!     Ok(())
//! }
//! ```

use std::{error::Error, fmt, io, num::NonZero, ptr::NonNull};

/// Error returned by a fallible [`AllocationAPI`] operation.
///
/// The set of errors depends on the operation. For example, size-token
/// multiplication reports [`Self::SizeOverflow`], invalid aligned requests
/// report [`Self::InvalidAlignment`], and a failed
/// [`AllocationAPI::usable_size`] query can report [`Self::NotOwned`].
#[non_exhaustive]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AllocationError {
    /// The allocator could not satisfy the allocation request.
    OutOfMemory,
    /// The requested alignment was not a power of two or is unsupported.
    InvalidAlignment,
    /// Computing the requested allocation size overflowed `usize`.
    SizeOverflow,
    /// The supplied pointer is not owned by the allocator.
    NotOwned,
    /// The allocator does not implement the requested operation.
    NotSupported,
    /// The allocator could not determine the precise cause of an allocation failure.
    SomethingWentWrong,
    /// The operating system rejected the operation with this raw error code.
    OsError(i32),
}

impl fmt::Display for AllocationError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::OutOfMemory => f.write_str("allocator could not satisfy the allocation request"),
            Self::InvalidAlignment => f.write_str("invalid or unsupported allocation alignment"),
            Self::SizeOverflow => f.write_str("allocation size overflowed usize"),
            Self::NotOwned => f.write_str("pointer is not owned by the allocator"),
            Self::NotSupported => f.write_str("allocation operation is not supported"),
            Self::SomethingWentWrong => f.write_str("allocation failed for an unspecified reason"),
            Self::OsError(error_num) => write!(
                f,
                "operating system error {error_num}: {}",
                io::Error::from_raw_os_error(*error_num)
            ),
        }
    }
}

impl Error for AllocationError {}

/// A byte-count token accepted by rsmalloc's native allocation interface.
///
/// This type intentionally contains no alignment. Use
/// [`AllocationAPI::allocate_aligned`] when a specific alignment is required.
/// `array_bytes::<T>(count)` checks multiplication, but it still does not
/// make the returned allocation suitable for storing `T` without an
/// appropriate alignment request.
#[repr(transparent)]
#[must_use]
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct AllocationSize(usize);

impl AllocationSize {
    /// Creates a request for exactly `bytes` bytes.
    pub const fn from_bytes(bytes: usize) -> Self {
        Self(bytes)
    }

    /// Computes the byte count occupied by `count` consecutive values of `T`.
    ///
    /// This records only the resulting byte count; it does not record
    /// `align_of::<T>()`.
    pub const fn array_bytes<T>(count: usize) -> Result<Self, AllocationError> {
        match size_of::<T>().checked_mul(count) {
            Some(bytes) => Ok(Self(bytes)),
            None => Err(AllocationError::SizeOverflow),
        }
    }

    /// Returns the requested number of bytes.
    pub const fn bytes(&self) -> usize {
        self.0
    }
}

/// Factory and inspection interface for an allocator-specific size token.
///
/// The separate [`AllocationSizeAPI::Out`] type permits an implementation to
/// use a factory type while guaranteeing through [`AllocationAPI::Size`] that
/// the produced value is exactly the type accepted by the allocator.
pub trait AllocationSizeAPI {
    /// Concrete size token produced by this factory.
    type Out;

    /// Creates a request for an explicit byte count.
    fn from_bytes(bytes: usize) -> Self::Out;

    /// Computes the bytes needed for `count` consecutive values of `T`.
    ///
    /// The result does not imply that the allocation will satisfy
    /// `align_of::<T>()`; callers requiring typed alignment must use
    /// [`AllocationAPI::allocate_aligned`].
    fn array_bytes<T>(count: usize) -> Result<Self::Out, AllocationError>;

    /// Returns the requested byte count.
    fn bytes(&self) -> usize;
}

impl AllocationSizeAPI for AllocationSize {
    type Out = AllocationSize;

    #[inline(always)]
    fn from_bytes(bytes: usize) -> Self::Out {
        AllocationSize::from_bytes(bytes)
    }

    #[inline(always)]
    fn array_bytes<T>(count: usize) -> Result<Self::Out, AllocationError> {
        AllocationSize::array_bytes::<T>(count)
    }

    #[inline(always)]
    fn bytes(&self) -> usize {
        self.0
    }
}

/// General-purpose, metadata-owning allocation interface.
///
/// Allocation returns a [`NonNull<u8>`] and must be paired with this
/// allocator's [`Self::deallocate`] or a successful reallocation. Unlike
/// [`std::alloc::GlobalAlloc`], deallocation does not take a `Layout`.
/// [`Self::usable_size`] can report more bytes than originally requested, but
/// extra capacity is not automatically initialized.
///
/// Allocation methods may reject zero-sized requests with an error. If a
/// zero-sized allocation succeeds, it must return a non-null pointer that can
/// later be passed to [`AllocationAPI::deallocate`]. Reallocation of a non-null
/// pointer to zero instead frees the old allocation and returns `Ok(null_mut())`.
///
/// A successful reallocation invalidates the old pointer even when its
/// numeric address is unchanged. For a nonzero resize, an error leaves the
/// original allocation live; callers must retain that pointer to free or
/// retry it. All methods returning [`AllocationError::NotSupported`] leave
/// existing allocations untouched.
///
/// RSMalloc rejects a null-pointer, zero-size reallocation request with
/// [`AllocationError::NotSupported`]. `Ok(null_mut())` is returned only when
/// a non-null allocation is freed by resizing it to zero.
///
/// # Safety
///
/// Implementors must return non-null, pairwise-disjoint live allocations and
/// keep them valid until a successful `deallocate` or `reallocate` invalidates
/// them. Safe allocation methods must never expose overlapping storage, and
/// every reallocation error must leave the original allocation live and
/// unmodified. Deallocating a null pointer must be a no-op; reallocating a null
/// pointer must behave as allocation for supported requests. Implementations
/// may reject a null-pointer, zero-size resize with an error.
pub unsafe trait AllocationAPI {
    /// Size token accepted by this allocator.
    type Size: AllocationSizeAPI<Out = Self::Size> + Copy;

    /// Allocates a block containing at least `size.bytes()` accessible bytes.
    ///
    /// The bytes are uninitialized. The returned pointer must eventually be
    /// deallocated through [`Self::deallocate`] or successfully reallocated.
    /// A zero-byte request may succeed with a deallocatable non-null pointer
    /// or return an error, according to the implementation.
    fn allocate(&self, size: Self::Size) -> Result<NonNull<u8>, AllocationError>;

    /// Allocates a block with an explicit alignment.
    ///
    /// `alignment` is a nonzero byte count and must be a supported power of two.
    /// The contents are uninitialized. The returned pointer satisfies at
    /// least the requested alignment. Its byte count still comes from `size`.
    fn allocate_aligned(
        &self,
        size: Self::Size,
        alignment: NonZero<usize>,
    ) -> Result<NonNull<u8>, AllocationError>;

    /// Allocates a block whose requested bytes are initialized to zero.
    ///
    /// Any additional usable capacity reported by [`AllocationAPI::usable_size`]
    /// is not guaranteed to be initialized. This operation uses the allocator's
    /// default alignment; use [`Self::allocate_aligned`] when a stronger
    /// alignment is required.
    fn allocate_zeroed(&self, size: Self::Size) -> Result<NonNull<u8>, AllocationError>;

    /// Allocates `nmem.bytes()` elements of `zero_size.bytes()` bytes each.
    ///
    /// The requested `nmem.bytes() * zero_size.bytes()` bytes are initialized to
    /// zero; additional usable capacity is not guaranteed to be zeroed. The
    /// multiplication is checked by the allocator, so it cannot wrap into a
    /// smaller allocation. RSMalloc reports any failure from this operation,
    /// including multiplication overflow or allocation failure, as
    /// [`AllocationError::SomethingWentWrong`]. Neither argument specifies an
    /// alignment beyond the allocator's default.
    fn allocate_zeroed_nmem(
        &self,
        nmem: Self::Size,
        zero_size: Self::Size,
    ) -> Result<NonNull<u8>, AllocationError>;

    /// Returns the usable payload size of a live allocation.
    ///
    /// The result can exceed the requested size. It describes accessible
    /// capacity, not which bytes contain initialized values. An aligned
    /// allocation's result excludes any offset before the returned pointer.
    ///
    /// Implementations that cannot provide this information return
    /// [`AllocationError::NotSupported`].
    ///
    /// # Safety
    ///
    /// `pointer` must identify a currently live allocation returned by an
    /// equivalent instance of this allocator. Passing an arbitrary pointer is
    /// not made safe merely because an implementation can sometimes return
    /// [`AllocationError::NotOwned`].
    unsafe fn usable_size(&self, pointer: NonNull<u8>) -> Result<usize, AllocationError>;

    /// Deallocates a live allocation without requiring its original size.
    ///
    /// A null `pointer` is accepted and does nothing. Otherwise, on success,
    /// `pointer` is invalidated and must not be used again. The original size
    /// and alignment are not needed.
    ///
    /// # Safety
    ///
    /// A non-null `pointer` must identify a currently live allocation returned
    /// by an equivalent instance of this allocator. Passing an arbitrary or
    /// already freed pointer can cause undefined behavior.
    unsafe fn deallocate(&self, pointer: *mut u8);

    /// Resizes an allocation while preserving its existing alignment.
    ///
    /// A null `pointer` requests a new allocation instead. On success, a
    /// non-null old pointer is invalidated even when the returned address
    /// is unchanged. Bytes through the smaller of the old and new requested
    /// sizes are preserved. For a nonzero `new_size`, every error—including
    /// [`AllocationError::NotSupported`]—leaves the original allocation live
    /// and unmodified.
    ///
    /// A zero-sized `new_size` with a non-null `pointer` frees it and returns
    /// `Ok(null_mut())`. A null `pointer` requests allocation for a nonzero
    /// `new_size`; RSMalloc returns [`AllocationError::NotSupported`] when both
    /// the pointer and size are zero. For a nonzero `new_size`, `Ok` contains a
    /// non-null pointer. The return type is a raw pointer because a successful
    /// non-null-to-zero resize deliberately returns null.
    ///
    /// # Safety
    ///
    /// A non-null `pointer` must identify a currently live allocation returned
    /// by an equivalent instance of this allocator.
    unsafe fn reallocate(
        &self,
        pointer: *mut u8,
        new_size: Self::Size,
    ) -> Result<*mut u8, AllocationError>;

    /// Resizes an allocation with a requested alignment for the result.
    ///
    /// A null `pointer` requests a new allocation with `new_alignment`.
    /// `new_alignment` is nonzero and must be a power of two supported by the
    /// allocator. The returned pointer satisfies at least this alignment; it
    /// may retain a stronger alignment when the block can be reused. Unlike
    /// [`AllocationAPI::reallocate`], this operation may move a block solely
    /// to satisfy a stronger alignment.
    ///
    /// On success, a non-null old pointer is invalidated even if its address is
    /// unchanged, and the existing contents are preserved through the smaller
    /// of the old and new requested sizes. For a nonzero `new_size`, errors leave
    /// the original allocation live and unmodified. A zero-sized `new_size`
    /// with a non-null `pointer` frees it and returns `Ok(null_mut())`, as with
    /// [`AllocationAPI::reallocate`]. A null `pointer` requests allocation for
    /// a nonzero `new_size`; with a valid alignment, RSMalloc returns
    /// [`AllocationError::NotSupported`] when both the pointer and size are
    /// zero. For a nonzero `new_size`, `Ok` contains a non-null pointer.
    ///
    /// # Safety
    ///
    /// A non-null `pointer` must identify a currently live allocation returned
    /// by an equivalent instance of this allocator.
    unsafe fn aligned_reallocate(
        &self,
        pointer: *mut u8,
        new_size: Self::Size,
        new_alignment: NonZero<usize>,
    ) -> Result<*mut u8, AllocationError>;
}
