use std::{
    cell::UnsafeCell,
    mem::size_of,
    ptr::null_mut,
    sync::atomic::{AtomicBool, AtomicUsize, Ordering, Ordering::Relaxed},
};

use rustix::mm::{MapFlags, ProtFlags, mmap_anonymous};

use crate::{
    MetaData,
    internals::{lock::SpinLock, once::Once},
    record_mmap_call,
    traits::Lock,
    utility::NUM_SIZE_CLASSES,
};

#[cfg(feature = "debug")]
pub static GLOBAL_QUEUE_REPORTS: AtomicUsize = AtomicUsize::new(0);

struct Slot {
    lock: SpinLock<*mut MetaData>,
}

pub struct ThreadQueue {
    nodes: UnsafeCell<*mut [Slot; NUM_SIZE_CLASSES]>,
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
            let bytes = size_of::<[Slot; NUM_SIZE_CLASSES]>() * node_count;
            record_mmap_call(bytes);
            if let Ok(mem) = mmap_anonymous(
                null_mut(),
                bytes,
                ProtFlags::READ | ProtFlags::WRITE,
                MapFlags::PRIVATE,
            ) {
                *self.nodes.get() = mem as *mut [Slot; NUM_SIZE_CLASSES];
                self.node_count.store(node_count, Ordering::Release);
                self.is_numa.store(is_numa, Relaxed);
            }
        });
    }

    #[inline(always)]
    unsafe fn slot(&self, node_id: u16, class: usize) -> Option<&Slot> {
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
        let Some(slot) = self.slot((*node).node_id, class) else {
            return;
        };

        #[cfg(feature = "debug")]
        GLOBAL_QUEUE_REPORTS.fetch_add(1, Ordering::Relaxed);

        let head = &mut *slot.lock.lock();
        (*node).next_page = *head;
        *head = node;
    }

    #[cfg(feature = "preload")]
    pub unsafe fn lock_all_for_fork(&self) {
        let nodes = *self.nodes.get();
        if nodes.is_null() {
            return;
        }

        let node_count = self.node_count.load(Ordering::Acquire);
        for node in 0..node_count {
            for class in 0..NUM_SIZE_CLASSES {
                core::mem::forget((*nodes.add(node))[class].lock.lock());
            }
        }
    }

    #[cfg(feature = "preload")]
    pub unsafe fn reset_locks_on_fork(&self) {
        let nodes = *self.nodes.get();
        if nodes.is_null() {
            return;
        }

        let node_count = self.node_count.load(Ordering::Acquire);
        for node in 0..node_count {
            for class in 0..NUM_SIZE_CLASSES {
                (*nodes.add(node))[class].lock.reset_at_fork();
            }
        }
    }

    #[inline(always)]
    pub unsafe fn pop(&self, node_id: u16, class: usize) -> *mut MetaData {
        let Some(slot) = self.slot(node_id, class) else {
            return null_mut();
        };

        let head = &mut *slot.lock.lock();
        let node = *head;
        if node.is_null() {
            return null_mut();
        }
        *head = (*node).next_page;
        (*node).next_page = null_mut();
        node
    }
}

pub static PENDING_QUEUE: ThreadQueue = ThreadQueue::new();
