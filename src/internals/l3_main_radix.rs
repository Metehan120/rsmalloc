// This file is partially assisted by CODEX & Gemini
//
// TODO: Verify safety of the RadixTree implementation

use crate::{RSMallocError, core_prim::wrappers::UnsafePointer, internals::lock::SerialLock};
use rustix::mm::{MapFlags, ProtFlags, mmap_anonymous};
use std::{hint::unlikely, ptr::null_mut};

pub const CHUNK_SIZE: usize = 4096;

const L1_BITS: usize = 12;
const L2_BITS: usize = 12;
const L3_BITS: usize = 12;

const L1_SIZE: usize = 1 << L1_BITS;
const L2_SIZE: usize = 1 << L2_BITS;
const L3_SIZE: usize = 1 << L3_BITS;
const L3_WORD_BITS: usize = u64::BITS as usize;
const L3_BITMAP_WORDS: usize = (L3_SIZE + L3_WORD_BITS - 1) / L3_WORD_BITS;

const MAX_ADDR: usize = 1 << 48;
const MAX_ADDR_IDX: usize = (MAX_ADDR) / 4096;
const RADIX_MAX_CHUNKS: usize = 1 << (L1_BITS + L2_BITS + L3_BITS);

pub struct Radix {
    pub l1: UnsafePointer<usize>,
}

impl Radix {
    unsafe fn map_memory(size: usize) -> *mut u8 {
        match mmap_anonymous(
            null_mut(),
            size,
            ProtFlags::READ | ProtFlags::WRITE,
            MapFlags::PRIVATE,
        ) {
            Ok(ptr) => ptr as *mut u8,
            Err(err) => RSMallocError::VAIinitFailed.log_and_abort(
                null_mut(),
                "Cannot allocate memory for RadixTree (L3)",
                Some(err.raw_os_error()),
            ),
        }
    }

    #[inline(always)]
    unsafe fn alloc_l3_bitmap_leaf() -> *mut u64 {
        Self::map_memory(L3_BITMAP_WORDS * size_of::<u64>()) as *mut u64
    }

    pub unsafe fn new() -> Self {
        let ptr = Self::map_memory(L1_SIZE * size_of::<usize>()) as *mut usize;
        Self {
            l1: UnsafePointer::new(ptr),
        }
    }

    #[inline(always)]
    fn split(idx: usize) -> (usize, usize, usize) {
        let l1 = (idx >> (L2_BITS + L3_BITS)) & (L1_SIZE - 1);
        let l2 = (idx >> L3_BITS) & (L2_SIZE - 1);
        let l3 = idx & (L3_SIZE - 1);
        (l1, l2, l3)
    }

    #[inline(always)]
    unsafe fn get_or_alloc(ptr: *mut usize, idx: usize, level_size: usize) -> *mut usize {
        let entry = ptr.add(idx);
        let val = *entry as *mut usize;
        if val.is_null() {
            let new = Self::map_memory(level_size * size_of::<usize>()) as *mut usize;
            *entry = new as usize;
            new
        } else {
            val
        }
    }

    #[inline(always)]
    unsafe fn get_or_alloc_l3(l2: *mut usize, idx: usize) -> *mut u64 {
        let entry = l2.add(idx);
        let val = *entry as *mut u64;
        if val.is_null() {
            let new = Self::alloc_l3_bitmap_leaf();
            *entry = new as usize;
            new
        } else {
            val
        }
    }

    #[inline(always)]
    pub unsafe fn set(&self, chunk_idx: usize, val: bool) {
        if unlikely(chunk_idx >= RADIX_MAX_CHUNKS) {
            return;
        }
        let (i1, i2, i3) = Self::split(chunk_idx);
        let l2 = Self::get_or_alloc(self.l1.as_ptr(), i1, L2_SIZE);
        let l3 = Self::get_or_alloc_l3(l2, i2);

        let word_idx = i3 / L3_WORD_BITS;
        let bit_idx = i3 % L3_WORD_BITS;
        let mask = 1u64 << bit_idx;
        let word = l3.add(word_idx);

        if val {
            *word |= mask;
        } else {
            *word &= !mask;
        }
    }

    #[inline(always)]
    pub unsafe fn get(&self, chunk_idx: usize) -> bool {
        if unlikely(chunk_idx >= RADIX_MAX_CHUNKS) {
            return false;
        }
        let (i1, i2, i3) = Self::split(chunk_idx);

        let l2 = *self.l1.as_ptr().add(i1) as *mut usize;
        if l2.is_null() {
            return false;
        }
        let l3 = *l2.add(i2) as *mut u64;
        if l3.is_null() {
            return false;
        }

        let word_idx = i3 / L3_WORD_BITS;
        let bit_idx = i3 % L3_WORD_BITS;
        let mask = 1u64 << bit_idx;

        (*l3.add(word_idx) & mask) != 0
    }
}

pub struct RadixTree {
    pub nodes: Radix,
    lock: SerialLock,
}

unsafe impl Sync for RadixTree {}

impl RadixTree {
    pub const unsafe fn new_const() -> Self {
        Self {
            nodes: Radix {
                l1: UnsafePointer::new(null_mut()),
            },
            lock: SerialLock::new(),
        }
    }

    pub unsafe fn new() -> Self {
        Self {
            nodes: Radix::new(),
            lock: SerialLock::new(),
        }
    }

    #[inline(always)]
    pub unsafe fn set_single_big(&mut self, addr: usize, val: bool) {
        let _guard = self.lock.lock();
        let start_idx = (addr / CHUNK_SIZE) % MAX_ADDR_IDX;

        self.nodes.set(start_idx, val);
    }

    #[inline(always)]
    pub unsafe fn set_range(&self, addr: usize, size: usize, val: bool) {
        let _guard = self.lock.lock();
        let start_idx = (addr / CHUNK_SIZE) % MAX_ADDR_IDX;
        let end_idx = (addr.saturating_add(size.saturating_sub(1)) / CHUNK_SIZE) % MAX_ADDR_IDX;
        for i in start_idx..=end_idx {
            self.nodes.set(i, val);
        }
    }

    #[inline(always)]
    pub unsafe fn is_owned(&self, addr: usize) -> bool {
        self.lock.spin_until_unlock();
        if unlikely(self.nodes.l1.is_null()) {
            return false;
        }
        self.nodes.get((addr / CHUNK_SIZE) % MAX_ADDR_IDX)
    }

    pub unsafe fn check_collision(&self, start: usize, size: usize) -> bool {
        let _guard = self.lock.lock();
        let start_idx = (start / CHUNK_SIZE) % MAX_ADDR_IDX;
        let end_idx = (start.saturating_add(size.saturating_sub(1)) / CHUNK_SIZE) % MAX_ADDR_IDX;
        if unlikely(end_idx >= RADIX_MAX_CHUNKS) {
            return true;
        }
        for i in start_idx..=end_idx {
            if self.nodes.get(i) {
                return true;
            }
        }
        false
    }
}

pub static mut L3_RADIX: RadixTree = unsafe { RadixTree::new_const() };

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn bitset_boundary_and_clear_correctness() {
        unsafe {
            let tree = RadixTree::new();

            let base = (5usize << (L2_BITS + L3_BITS)) | (7usize << L3_BITS);
            let idx_a = base + 63;
            let idx_b = base + 64;
            let idx_c = base + 127;

            tree.nodes.set(idx_a, true);
            tree.nodes.set(idx_b, true);

            assert!(tree.nodes.get(idx_a));
            assert!(tree.nodes.get(idx_b));
            assert!(!tree.nodes.get(idx_c));

            tree.nodes.set(idx_b, false);
            assert!(tree.nodes.get(idx_a));
            assert!(!tree.nodes.get(idx_b));
        }
    }

    #[test]
    fn range_set_clear_and_collision() {
        unsafe {
            let tree = RadixTree::new();

            let start = 0x1200_0000usize;
            let size = CHUNK_SIZE * 4;

            tree.set_range(start, size, true);

            assert!(tree.is_owned(start));
            assert!(tree.is_owned(start + CHUNK_SIZE));
            assert!(tree.is_owned(start + CHUNK_SIZE * 3));
            assert!(!tree.is_owned(start + CHUNK_SIZE * 4));
            assert!(tree.check_collision(start + CHUNK_SIZE, CHUNK_SIZE * 2));
            assert!(!tree.check_collision(start + CHUNK_SIZE * 5, CHUNK_SIZE));

            tree.set_range(start, size, false);

            assert!(!tree.is_owned(start));
            assert!(!tree.is_owned(start + CHUNK_SIZE * 3));
            assert!(!tree.check_collision(start, size));
        }
    }

    #[test]
    fn out_of_bounds_index_is_safe() {
        unsafe {
            let tree = RadixTree::new();

            assert!(!tree.nodes.get(RADIX_MAX_CHUNKS));
            tree.nodes.set(RADIX_MAX_CHUNKS, true);

            assert!(!tree.nodes.get(0));
        }
    }
}
