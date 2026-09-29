use std::num::NonZero;
use thiserror::Error;

#[derive(Debug, Error)]
pub enum HelperErrors {
    #[error("{alignment} is not a nonzero power-of-two alignment")]
    NotALignOfTwo { alignment: usize },
}

/// Default alignment of allocations.
pub const DEFAULT_ALIGNMENT: usize = 16;
/// A 32-byte alignment request, useful for 256-bit SIMD data.
///
/// This controls pointer alignment, not CPU instruction availability.
pub const SIMD256_ALIGNMENT: NonZero<usize> = unsafe { NonZero::new_unchecked(32) };
/// A 4 KiB alignment request for page-oriented buffers on x86-64 Linux.
///
/// Alignment alone does not allocate a full page or change page protection.
pub const X86_PAGE_ALIGNMENT: NonZero<usize> = unsafe { NonZero::new_unchecked(4096) };

/// Helpers for constructing nonzero power-of-two alignments.
pub struct Alignment;

impl Alignment {
    /// Returns the alignment of `T` as a nonzero alignment request.
    pub const fn align_of<T>() -> Result<NonZero<usize>, HelperErrors> {
        let alignment = align_of::<T>();
        Self::new_align(alignment)
    }

    /// Accepts a nonzero power-of-two alignment, rejecting other values.
    pub const fn new_align(alignment: usize) -> Result<NonZero<usize>, HelperErrors> {
        if let Some(nonzero) = NonZero::new(alignment)
            && alignment.is_power_of_two()
        {
            return Ok(nonzero);
        }
        Err(HelperErrors::NotALignOfTwo { alignment })
    }
}
