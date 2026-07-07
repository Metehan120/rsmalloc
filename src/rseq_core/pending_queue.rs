use std::{
    hint::spin_loop,
    ptr::null_mut,
    sync::atomic::{AtomicUsize, Ordering},
};

use crate::{MetaData, utility::NUM_SIZE_CLASSES};

const TAG_MASK: usize = 4095;
const PTR_MASK: usize = !TAG_MASK;

#[inline(always)]
fn pack(ptr: *mut MetaData, old_word: usize) -> usize {
    let tag = old_word.wrapping_add(1) & TAG_MASK;
    ((ptr as usize) & PTR_MASK) | tag
}

#[inline(always)]
fn unpack_ptr(word: usize) -> *mut MetaData {
    (word & PTR_MASK) as *mut MetaData
}

pub struct ThreadQueue {
    pages: [AtomicUsize; NUM_SIZE_CLASSES],
}

impl ThreadQueue {
    pub const fn new() -> Self {
        Self {
            pages: [const { AtomicUsize::new(0) }; NUM_SIZE_CLASSES],
        }
    }

    #[cold]
    #[inline(never)]
    pub unsafe fn insert(&self, class: usize, node: *mut MetaData) {
        let head = &self.pages[class];

        loop {
            let old = head.load(Ordering::Relaxed);
            (*node).next_page = unpack_ptr(old);
            let new = pack(node, old);

            if head
                .compare_exchange_weak(old, new, Ordering::Release, Ordering::Relaxed)
                .is_ok()
            {
                return;
            }

            spin_loop();
        }
    }

    #[inline(always)]
    pub unsafe fn pop(&self, class: usize) -> *mut MetaData {
        let head = &self.pages[class];

        loop {
            let old = head.load(Ordering::Acquire);
            let node = unpack_ptr(old);

            if node.is_null() {
                return null_mut();
            }

            let next = (*node).next_page;
            let new = pack(next, old);

            if head
                .compare_exchange_weak(old, new, Ordering::Acquire, Ordering::Relaxed)
                .is_ok()
            {
                (*node).next_page = null_mut();
                return node;
            }

            spin_loop();
        }
    }
}

pub static PENDING_QUEUE: ThreadQueue = ThreadQueue::new();
