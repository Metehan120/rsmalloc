// Readers use acquire loads while writers publish new radix nodes with CAS and
// update bitmap words with release RMWs. A reader racing with a writer may
// observe either the old or new state; the allocator only requires eventual
// visibility, not a perfectly up-to-date view of the radix.

#[cfg(feature = "preload")]
use crate::internals::lock::SerialLock;
use crate::{RSMallocError, core_prim::wrappers::UnsafePointer};
use rustix::mm::{MapFlags, ProtFlags, mmap_anonymous, munmap};
use std::{
    hint::unlikely,
    os::raw::c_void,
    ptr::null_mut,
    sync::atomic::{
        AtomicU64, AtomicUsize,
        Ordering::{Acquire, Release},
    },
};

pub const CHUNK_SIZE: usize = 4096;

const L0_BITS: usize = 8;
const L1_BITS: usize = 12;
const L2_BITS: usize = 12;
const L3_BITS: usize = 12;

const L0_SIZE: usize = 1 << L0_BITS;
const L1_SIZE: usize = 1 << L1_BITS;
const L2_SIZE: usize = 1 << L2_BITS;
const L3_SIZE: usize = 1 << L3_BITS;
const L3_WORD_BITS: usize = u64::BITS as usize;
const L3_BITMAP_WORDS: usize = (L3_SIZE + L3_WORD_BITS - 1) / L3_WORD_BITS;

const MAX_ADDR: usize = 1usize << 56;
const RADIX_MAX_CHUNKS: usize = 1 << (L0_BITS + L1_BITS + L2_BITS + L3_BITS);

pub struct Radix {
    pub l0: UnsafePointer<AtomicUsize>,
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
    unsafe fn unmap_memory(ptr: *mut u8, size: usize) {
        let _ = munmap(ptr as *mut c_void, size);
    }

    #[inline(always)]
    unsafe fn alloc_l3_bitmap_leaf() -> *mut AtomicU64 {
        Self::map_memory(L3_BITMAP_WORDS * size_of::<AtomicU64>()) as *mut AtomicU64
    }

    pub unsafe fn new() -> Self {
        let ptr = Self::map_memory(L0_SIZE * size_of::<AtomicUsize>()) as *mut AtomicUsize;
        Self {
            l0: UnsafePointer::new(ptr),
        }
    }

    #[inline(always)]
    fn split(idx: usize) -> (usize, usize, usize, usize) {
        let l0 = (idx >> (L1_BITS + L2_BITS + L3_BITS)) & (L0_SIZE - 1);
        let l1 = (idx >> (L2_BITS + L3_BITS)) & (L1_SIZE - 1);
        let l2 = (idx >> L3_BITS) & (L2_SIZE - 1);
        let l3 = idx & (L3_SIZE - 1);
        (l0, l1, l2, l3)
    }

    #[inline(always)]
    unsafe fn get_or_alloc(
        ptr: *mut AtomicUsize,
        idx: usize,
        level_size: usize,
    ) -> *mut AtomicUsize {
        let entry = ptr.add(idx);
        let val = (*entry).load(Acquire) as *mut AtomicUsize;
        if !val.is_null() {
            return val;
        }

        let size = level_size * size_of::<AtomicUsize>();
        let new = Self::map_memory(size) as *mut AtomicUsize;
        match (*entry).compare_exchange(0, new as usize, Release, Acquire) {
            Ok(_) => new,
            Err(existing) => {
                Self::unmap_memory(new as *mut u8, size);
                existing as *mut AtomicUsize
            }
        }
    }

    #[inline(always)]
    unsafe fn get_or_alloc_l3(l2: *mut AtomicUsize, idx: usize) -> *mut AtomicU64 {
        let entry = l2.add(idx);
        let val = (*entry).load(Acquire) as *mut AtomicU64;
        if !val.is_null() {
            return val;
        }

        let size = L3_BITMAP_WORDS * size_of::<AtomicU64>();
        let new = Self::alloc_l3_bitmap_leaf();
        match (*entry).compare_exchange(0, new as usize, Release, Acquire) {
            Ok(_) => new,
            Err(existing) => {
                Self::unmap_memory(new as *mut u8, size);
                existing as *mut AtomicU64
            }
        }
    }

    #[inline(always)]
    pub unsafe fn set(&self, chunk_idx: usize, val: bool) {
        if unlikely(chunk_idx >= RADIX_MAX_CHUNKS) {
            return;
        }
        let (i0, i1, i2, i3) = Self::split(chunk_idx);
        let l1 = Self::get_or_alloc(self.l0.as_ptr(), i0, L1_SIZE);
        let l2 = Self::get_or_alloc(l1, i1, L2_SIZE);
        let l3 = Self::get_or_alloc_l3(l2, i2);

        let word_idx = i3 / L3_WORD_BITS;
        let bit_idx = i3 % L3_WORD_BITS;
        let mask = 1u64 << bit_idx;
        let word = l3.add(word_idx);

        if val {
            (*word).fetch_or(mask, Release);
        } else {
            (*word).fetch_and(!mask, Release);
        }
    }

    #[inline(always)]
    pub unsafe fn get(&self, chunk_idx: usize) -> bool {
        if unlikely(chunk_idx >= RADIX_MAX_CHUNKS) {
            return false;
        }
        let (i0, i1, i2, i3) = Self::split(chunk_idx);

        let l1 = (*self.l0.as_ptr().add(i0)).load(Acquire) as *mut AtomicUsize;
        if l1.is_null() {
            return false;
        }
        let l2 = (*l1.add(i1)).load(Acquire) as *mut AtomicUsize;
        if l2.is_null() {
            return false;
        }
        let l3 = (*l2.add(i2)).load(Acquire) as *mut AtomicU64;
        if l3.is_null() {
            return false;
        }

        let word_idx = i3 / L3_WORD_BITS;
        let bit_idx = i3 % L3_WORD_BITS;
        let mask = 1u64 << bit_idx;

        ((*l3.add(word_idx)).load(Acquire) & mask) != 0
    }
}

pub struct RadixTree {
    pub nodes: Radix,
    #[cfg(feature = "preload")]
    pub lock: SerialLock,
}

unsafe impl Sync for RadixTree {}

impl RadixTree {
    pub const unsafe fn new_const() -> Self {
        Self {
            nodes: Radix {
                l0: UnsafePointer::new(null_mut()),
            },
            #[cfg(feature = "preload")]
            lock: SerialLock::new(),
        }
    }

    pub unsafe fn new() -> Self {
        Self {
            nodes: Radix::new(),
            #[cfg(feature = "preload")]
            lock: SerialLock::new(),
        }
    }

    #[inline(always)]
    pub unsafe fn set_single_big(&mut self, addr: usize, val: bool) {
        if unlikely(!Self::valid_user_addr(addr)) {
            return;
        }
        self.nodes.set(addr / CHUNK_SIZE, val);
    }

    #[inline(always)]
    pub unsafe fn set_range(&self, addr: usize, size: usize, val: bool) {
        if unlikely(size == 0 || !Self::valid_user_addr(addr)) {
            return;
        }

        let Some(end_addr) = addr.checked_add(size - 1) else {
            return;
        };

        if unlikely(!Self::valid_user_addr(end_addr)) {
            return;
        }

        let start_idx = addr / CHUNK_SIZE;
        let end_idx = end_addr / CHUNK_SIZE;

        for i in start_idx..=end_idx {
            self.nodes.set(i, val);
        }
    }

    #[inline(always)]
    pub unsafe fn is_owned(&self, addr: usize) -> bool {
        if unlikely(self.nodes.l0.is_null() || !Self::valid_user_addr(addr)) {
            return false;
        }

        self.nodes.get(addr / CHUNK_SIZE)
    }

    #[inline(always)]
    const fn valid_user_addr(addr: usize) -> bool {
        addr < MAX_ADDR
    }
}

pub static mut L3_RADIX: RadixTree = unsafe { RadixTree::new_const() };
