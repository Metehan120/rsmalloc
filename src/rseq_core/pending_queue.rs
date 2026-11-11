// Assisted by CODEX Audited by me
//
// - Metehan

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
const PENDING_LANES: usize = 4;
const PENDING_LANE_MASK: usize = PENDING_LANES - 1;

#[repr(align(64))]
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
                Ok(_) => return node,
                Err(observed) => old = observed,
            }
        }
    }
}

struct ClassSlots {
    lanes: [Slot; PENDING_LANES],
}

impl ClassSlots {
    #[inline(always)]
    unsafe fn push(&self, cpu_id: usize, node: *mut MetaData) {
        self.lanes[cpu_id & PENDING_LANE_MASK].push(node);
    }

    #[inline(always)]
    unsafe fn pop(&self, cpu_id: usize) -> *mut MetaData {
        let preferred = cpu_id & PENDING_LANE_MASK;
        for offset in 0..PENDING_LANES {
            let node = self.lanes[(preferred + offset) & PENDING_LANE_MASK].pop();
            if !node.is_null() {
                return node;
            }
        }
        null_mut()
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
    nodes: UnsafeCell<*mut [ClassSlots; NUM_SIZE_CLASSES]>,
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
            let bytes = size_of::<[ClassSlots; NUM_SIZE_CLASSES]>() * node_count;
            record_mmap_call(bytes);
            if let Ok(mem) = mmap_anonymous(
                null_mut(),
                bytes,
                ProtFlags::READ | ProtFlags::WRITE,
                MapFlags::PRIVATE,
            ) {
                *self.nodes.get() = mem as *mut [ClassSlots; NUM_SIZE_CLASSES];
                self.node_count.store(node_count, Ordering::Release);
                self.is_numa.store(is_numa, Relaxed);
            }
        });
    }

    #[inline(always)]
    unsafe fn class_slots(&self, node_id: u16, class: usize) -> Option<&ClassSlots> {
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
    pub unsafe fn insert(&self, class: usize, cpu_id: usize, node: *mut MetaData) {
        let Some(slots) = self.class_slots((*node).node_id, class) else {
            return;
        };

        #[cfg(feature = "debug")]
        GLOBAL_QUEUE_REPORTS.fetch_add(1, Ordering::Relaxed);

        slots.push(cpu_id, node);
    }

    #[inline(always)]
    pub unsafe fn pop(&self, node_id: u16, class: usize, cpu_id: usize) -> *mut MetaData {
        let Some(slots) = self.class_slots(node_id, class) else {
            return null_mut();
        };

        slots.pop(cpu_id)
    }
}

pub static PENDING_QUEUE: BulkFillQueue = BulkFillQueue::new();
