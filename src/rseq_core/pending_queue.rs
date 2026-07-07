use std::{cell::UnsafeCell, ptr::null_mut};

use crate::{MetaData, internals::lock::SpinLock, utility::NUM_SIZE_CLASSES};

pub struct Inner {
    pages: [*mut MetaData; NUM_SIZE_CLASSES],
    lock: [SpinLock; NUM_SIZE_CLASSES],
}

pub struct ThreadQueue {
    inner: UnsafeCell<Inner>,
}

unsafe impl Sync for ThreadQueue {}

impl ThreadQueue {
    pub const fn new() -> Self {
        Self {
            inner: UnsafeCell::new(Inner {
                pages: [const { null_mut() }; NUM_SIZE_CLASSES],
                lock: [const { SpinLock::new() }; NUM_SIZE_CLASSES],
            }),
        }
    }

    #[cold]
    pub unsafe fn insert(&self, class: usize, node: *mut MetaData) {
        let inner = self.inner.get();
        let inner = &mut (*inner);
        let _guard = inner.lock[class].lock();

        (*node).next_page = inner.pages[class];
        inner.pages[class] = node;
    }

    #[inline(always)]
    pub unsafe fn pop(&self, class: usize) -> *mut MetaData {
        let inner = self.inner.get();
        let inner = &mut (*inner);
        let Some(_guard) = inner.lock[class].try_lock() else {
            return null_mut();
        };

        let node = inner.pages[class];
        if node.is_null() {
            return node;
        }

        inner.pages[class] = (*node).next_page;
        (*node).next_page = null_mut();
        node
    }
}

pub static PENDING_QUEUE: ThreadQueue = ThreadQueue::new();
