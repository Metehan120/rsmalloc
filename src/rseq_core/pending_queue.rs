use std::{
    cell::UnsafeCell,
    hint::spin_loop,
    mem::size_of,
    ptr::null_mut,
    sync::atomic::{
        AtomicBool, AtomicUsize,
        Ordering::{self, Relaxed},
    },
};

use rustix::mm::{MapFlags, ProtFlags, mmap_anonymous};

use crate::{MetaData, internals::once::Once, utility::NUM_SIZE_CLASSES};

const PAGE_SIZE: usize = 4096;
const TAG_MASK: usize = PAGE_SIZE - 1;
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
    nodes: UnsafeCell<*mut [AtomicUsize; NUM_SIZE_CLASSES]>,
    node_count: AtomicUsize,
    once: Once,
    is_numa: AtomicBool,
}

unsafe impl Sync for ThreadQueue {}

impl ThreadQueue {
    pub const fn new() -> Self {
        Self {
            nodes: UnsafeCell::new(null_mut()),
            node_count: AtomicUsize::new(0),
            once: Once::new(),
            is_numa: AtomicBool::new(false),
        }
    }

    #[cold]
    #[inline(never)]
    pub unsafe fn init(&self, node_count: usize, is_numa: bool) {
        self.once.call_once(|| {
            let node_count = node_count.max(1);
            let bytes = size_of::<[AtomicUsize; NUM_SIZE_CLASSES]>() * node_count;

            if let Ok(mem) = mmap_anonymous(
                null_mut(),
                bytes,
                ProtFlags::READ | ProtFlags::WRITE,
                MapFlags::PRIVATE,
            ) {
                *self.nodes.get() = mem as *mut [AtomicUsize; NUM_SIZE_CLASSES];
                self.node_count.store(node_count, Ordering::Release);
                self.is_numa.store(is_numa, Relaxed);
            }
        });
    }

    #[inline(always)]
    unsafe fn head(&self, node_id: u16, class: usize) -> Option<&AtomicUsize> {
        let nodes = *self.nodes.get();
        if nodes.is_null() {
            return None;
        }

        if !self.is_numa.load(Relaxed) {
            return Some(&(*nodes)[class]);
        }

        let node_id = node_id as usize;
        if node_id >= self.node_count.load(Ordering::Acquire) {
            return None;
        }

        Some(&(*nodes.add(node_id))[class])
    }

    #[cold]
    #[inline(never)]
    pub unsafe fn insert(&self, class: usize, node: *mut MetaData) {
        let Some(head) = self.head((*node).node_id, class) else {
            return;
        };

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
    pub unsafe fn pop(&self, node_id: u16, class: usize) -> *mut MetaData {
        let Some(head) = self.head(node_id, class) else {
            return null_mut();
        };

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
