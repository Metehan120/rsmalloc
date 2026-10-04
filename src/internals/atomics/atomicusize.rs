use std::sync::atomic;

use crate::internals::atomics::AtomicOrdering;

#[repr(transparent)]
pub struct AtomicUsize {
    inner: atomic::AtomicUsize,
}

impl AtomicUsize {
    #[inline(always)]
    pub const fn new(value: usize) -> Self {
        Self {
            inner: atomic::AtomicUsize::new(value),
        }
    }

    #[inline(always)]
    pub fn load(&self, order: AtomicOrdering) -> usize {
        self.inner.load(order.to_atomic_ordering::<false>())
    }

    #[inline(always)]
    pub fn store(&self, value: usize, order: AtomicOrdering) {
        self.inner.store(value, order.to_atomic_ordering::<false>());
    }

    #[inline(always)]
    pub fn fetch_add(&self, value: usize, order: AtomicOrdering) -> usize {
        self.inner
            .fetch_add(value, order.to_atomic_ordering::<true>())
    }

    #[inline(always)]
    pub fn fetch_sub(&self, value: usize, order: AtomicOrdering) -> usize {
        self.inner
            .fetch_sub(value, order.to_atomic_ordering::<true>())
    }

    #[inline(always)]
    pub const fn as_ptr(&self) -> *mut usize {
        self.inner.as_ptr()
    }
}
