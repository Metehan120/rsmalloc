//! General-purpose allocation interface used by rsmalloc's native API.
//!
//! [`AllocationAPI`] deliberately models malloc-style allocation: allocations
//! carry their own metadata, deallocation does not require the original layout,
//! and reallocation preserves the existing alignment. This interface is
//! independent of Rust's `GlobalAlloc` and unstable `Allocator` traits.

use std::{error::Error, fmt, io, ptr::NonNull};

/// Error returned by a fallible [`AllocationAPI`] operation.
#[non_exhaustive]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AllocationError {
    /// The allocator could not satisfy the allocation request.
    OutOfMemory,
    /// The requested alignment was zero, not a power of two, or unsupported.
    InvalidAlignment,
    /// Computing the requested allocation size overflowed `usize`.
    SizeOverflow,
    /// The supplied pointer is not owned by the allocator.
    NotOwned,
    /// The allocator does not implement the requested operation.
    NotSupported,
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
/// Implementations may reject zero-sized requests with an error. If a
/// zero-sized request succeeds, it must still return a non-null pointer that can
/// later be passed to [`AllocationAPI::deallocate`].
///
/// All methods returning [`AllocationError::NotSupported`] leave existing
/// allocations untouched.
///
/// # Safety
///
/// Implementors must return non-null, pairwise-disjoint live allocations and
/// keep them valid until a successful `deallocate` or `reallocate` invalidates
/// them. Safe allocation methods must never expose overlapping storage, and
/// every failure from `reallocate` must leave the original allocation live and
/// unmodified.
pub unsafe trait AllocationAPI {
    /// Size token accepted by this allocator.
    type Size: AllocationSizeAPI<Out = Self::Size> + Copy;

    /// Allocates a block containing at least `size.bytes()` accessible bytes.
    fn allocate(&self, size: Self::Size) -> Result<NonNull<u8>, AllocationError>;

    /// Allocates a block with an explicit alignment.
    ///
    /// `alignment` is measured in bytes and must be a supported, nonzero power
    /// of two. The contents are uninitialized.
    fn allocate_aligned(
        &self,
        size: Self::Size,
        alignment: usize,
    ) -> Result<NonNull<u8>, AllocationError>;

    /// Allocates a block whose requested bytes are initialized to zero.
    ///
    /// Any additional usable capacity reported by [`AllocationAPI::usable_size`]
    /// is not guaranteed to be initialized.
    fn allocate_zeroed(&self, size: Self::Size) -> Result<NonNull<u8>, AllocationError>;

    /// Returns the usable payload size of a live allocation.
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
    /// On success, `pointer` is invalidated and must not be used again.
    ///
    /// # Safety
    ///
    /// `pointer` must identify a currently live allocation returned by an
    /// equivalent instance of this allocator. Passing an arbitrary or already
    /// freed pointer can cause undefined behavior.
    unsafe fn deallocate(&self, pointer: NonNull<u8>);

    /// Resizes an allocation while preserving its existing alignment.
    ///
    /// On success, the old pointer is invalidated even when the returned address
    /// is unchanged. Bytes through the smaller of the old and new requested
    /// sizes are preserved. On every error—including
    /// [`AllocationError::NotSupported`]—the original allocation remains live
    /// and unmodified.
    ///
    /// A zero-sized `new_size` follows the implementation's documented
    /// zero-sized allocation policy; it must not silently invalidate `pointer`
    /// while returning an error.
    ///
    /// # Safety
    ///
    /// `pointer` must identify a currently live allocation returned by an
    /// equivalent instance of this allocator.
    unsafe fn reallocate(
        &self,
        pointer: NonNull<u8>,
        new_size: Self::Size,
    ) -> Result<NonNull<u8>, AllocationError>;
}
