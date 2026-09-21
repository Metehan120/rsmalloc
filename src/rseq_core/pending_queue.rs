use std::{
    cell::UnsafeCell,
    mem::size_of,
    ptr::null_mut,
    sync::atomic::{AtomicBool, AtomicUsize, Ordering, Ordering::Relaxed},
};

use portable_atomic::AtomicU128;
use rustix::mm::{MapFlags, ProtFlags, mmap_anonymous};

use crate::{MetaData, internals::once::Once, record_mmap_call, utility::NUM_SIZE_CLASSES};

#[cfg(feature = "debug")]
pub static GLOBAL_QUEUE_REPORTS: AtomicUsize = AtomicUsize::new(0);

const TAG_SHIFT: u32 = 64;
const PTR_MASK: u128 = u64::MAX as u128;

struct Slot {
    head: AtomicU128,
}

impl Slot {
    #[inline(always)]
    unsafe fn push(&self, node: *mut MetaData) {
        let mut old = self.head.load(Ordering::Relaxed);
        loop {
            (*node).next_page.store(unpack_ptr(old), Ordering::Relaxed);
            match self.head.compare_exchange_weak(
                old,
                repack_ptr(node, old),
                Ordering::Release,
                Ordering::Relaxed,
            ) {
                Ok(_) => return,
                Err(observed) => old = observed,
            }
        }
    }

    #[inline(always)]
    unsafe fn pop(&self) -> *mut MetaData {
        let mut old = self.head.load(Ordering::Acquire);
        loop {
            let node = unpack_ptr(old);
            if node.is_null() {
                return null_mut();
            }

            let next = (*node).next_page.load(Ordering::Relaxed);
            match self.head.compare_exchange_weak(
                old,
                repack_ptr(next, old),
                Ordering::Acquire,
                Ordering::Acquire,
            ) {
                Ok(_) => {
                    (*node).next_page.store(null_mut(), Ordering::Relaxed);
                    return node;
                }
                Err(observed) => old = observed,
            }
        }
    }
}

#[inline(always)]
fn unpack_ptr(word: u128) -> *mut MetaData {
    (word & PTR_MASK) as usize as *mut MetaData
}

#[inline(always)]
fn repack_ptr(ptr: *mut MetaData, old: u128) -> u128 {
    let tag = ((old >> TAG_SHIFT) as u64).wrapping_add(1);
    ((ptr as usize as u128) & PTR_MASK) | ((tag as u128) << TAG_SHIFT)
}

pub struct BulkFillQueue {
    nodes: UnsafeCell<*mut [Slot; NUM_SIZE_CLASSES]>,
    node_count: AtomicUsize,
    once: Once,
    is_numa: AtomicBool,
}

unsafe impl Sync for BulkFillQueue {}

impl BulkFillQueue {
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

        slot.push(node);
    }

    #[inline(always)]
    pub unsafe fn pop(&self, node_id: u16, class: usize) -> *mut MetaData {
        let Some(slot) = self.slot(node_id, class) else {
            return null_mut();
        };

        slot.pop()
    }
}

pub static PENDING_QUEUE: BulkFillQueue = BulkFillQueue::new();
