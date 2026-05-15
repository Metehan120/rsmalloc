use rustix::mm::{MapFlags, ProtFlags, mmap_anonymous, munmap};

use crate::{BigAllocMeta, RSMallocError, internals::lock::SerialLock};
use std::{
    mem::size_of,
    os::raw::c_void,
    ptr::null_mut,
    sync::atomic::{AtomicPtr, AtomicUsize, Ordering},
};

const STATE_EMPTY: u8 = 0;
const STATE_OCCUPIED: u8 = 1;
const STATE_TOMBSTONE: u8 = 2;

const LOAD_NUM: usize = 7;
const LOAD_DEN: usize = 10;
const INITIAL_CAP: usize = 16 * 1024;

#[repr(C)]
struct Entry {
    key: usize,
    meta: BigAllocMeta,
    state: u8,
    _pad: [u8; 7],
}

pub struct BigAllocMap {
    ptr: AtomicPtr<Entry>,
    cap: AtomicUsize,
    len: AtomicUsize,
    tombstones: AtomicUsize,
    lock: SerialLock,
}

pub static BIG_ALLOC_MAP: BigAllocMap = BigAllocMap::new();
pub use BIG_ALLOC_MAP as BIG_MAP;

impl BigAllocMap {
    pub const fn new() -> Self {
        BigAllocMap {
            ptr: AtomicPtr::new(null_mut()),
            cap: AtomicUsize::new(0),
            len: AtomicUsize::new(0),
            tombstones: AtomicUsize::new(0),
            lock: SerialLock::new(),
        }
    }

    pub unsafe fn insert(&self, key: usize, meta: BigAllocMeta) {
        let _guard = self.lock.lock();
        self.ensure_capacity();
        let table = self.ptr.load(Ordering::Relaxed);
        let cap = self.cap.load(Ordering::Relaxed);
        let mut idx = hash_key(key) & (cap - 1);
        let mut first_tomb: isize = -1;

        for _ in 0..cap {
            let entry = table.add(idx);
            let state = (*entry).state;
            if state == STATE_EMPTY {
                let target = if first_tomb >= 0 {
                    table.add(first_tomb as usize)
                } else {
                    entry
                };
                (*target).key = key;
                (*target).meta = meta;
                (*target).state = STATE_OCCUPIED;
                self.len.fetch_add(1, Ordering::Relaxed);
                if first_tomb >= 0 {
                    self.tombstones.fetch_sub(1, Ordering::Relaxed);
                }
                return;
            }
            if state == STATE_TOMBSTONE && first_tomb < 0 {
                first_tomb = idx as isize;
            } else if state == STATE_OCCUPIED && (*entry).key == key {
                (*entry).meta = meta;
                return;
            }
            idx = (idx + 1) & (cap - 1);
        }

        if first_tomb >= 0 {
            let target = table.add(first_tomb as usize);
            (*target).key = key;
            (*target).meta = meta;
            (*target).state = STATE_OCCUPIED;
            self.len.fetch_add(1, Ordering::Relaxed);
            self.tombstones.fetch_sub(1, Ordering::Relaxed);
            return;
        }

        RSMallocError::OutOfMemory.log_and_abort(
            null_mut() as *mut c_void,
            "BigAllocMap insert failed (table full)",
            None,
        );
    }

    pub unsafe fn get(&self, key: usize) -> Option<BigAllocMeta> {
        let _guard = self.lock.lock();
        let table = self.ptr.load(Ordering::Relaxed);
        let cap = self.cap.load(Ordering::Relaxed);
        if table.is_null() || cap == 0 {
            return None;
        }

        let mut idx = hash_key(key) & (cap - 1);
        for _ in 0..cap {
            let entry = table.add(idx);
            let state = (*entry).state;
            if state == STATE_EMPTY {
                return None;
            }
            if state == STATE_OCCUPIED && (*entry).key == key {
                return Some((*entry).meta);
            }
            idx = (idx + 1) & (cap - 1);
        }
        None
    }

    pub unsafe fn replace(&self, key: usize, meta: BigAllocMeta) -> Option<BigAllocMeta> {
        let _guard = self.lock.lock();
        let table = self.ptr.load(Ordering::Relaxed);
        let cap = self.cap.load(Ordering::Relaxed);
        if table.is_null() || cap == 0 {
            return None;
        }

        let mut idx = hash_key(key) & (cap - 1);
        for _ in 0..cap {
            let entry = table.add(idx);
            let state = (*entry).state;
            if state == STATE_EMPTY {
                return None;
            }
            if state == STATE_OCCUPIED && (*entry).key == key {
                let old = (*entry).meta;
                (*entry).meta = meta;
                return Some(old);
            }
            idx = (idx + 1) & (cap - 1);
        }

        None
    }

    pub unsafe fn remove(&self, key: usize) -> Option<BigAllocMeta> {
        let _guard = self.lock.lock();
        let table = self.ptr.load(Ordering::Relaxed);
        let cap = self.cap.load(Ordering::Relaxed);
        if table.is_null() || cap == 0 {
            return None;
        }

        let mut idx = hash_key(key) & (cap - 1);
        for _ in 0..cap {
            let entry = table.add(idx);
            let state = (*entry).state;
            if state == STATE_EMPTY {
                return None;
            }
            if state == STATE_OCCUPIED && (*entry).key == key {
                (*entry).state = STATE_TOMBSTONE;
                self.len.fetch_sub(1, Ordering::Relaxed);
                self.tombstones.fetch_add(1, Ordering::Relaxed);
                return Some((*entry).meta);
            }
            idx = (idx + 1) & (cap - 1);
        }
        None
    }

    unsafe fn ensure_capacity(&self) {
        let cap = self.cap.load(Ordering::Relaxed);
        if cap == 0 {
            self.alloc_table(INITIAL_CAP);
            return;
        }

        let len = self.len.load(Ordering::Relaxed);
        let tombs = self.tombstones.load(Ordering::Relaxed);
        if (len + 1) * LOAD_DEN <= cap * LOAD_NUM && tombs * 2 <= cap {
            return;
        }
        self.grow();
    }

    unsafe fn grow(&self) {
        let old_ptr = self.ptr.load(Ordering::Relaxed);
        let old_cap = self.cap.load(Ordering::Relaxed);
        let new_cap = old_cap.saturating_mul(2).max(INITIAL_CAP);

        let new_ptr = self.alloc_raw(new_cap);
        for i in 0..old_cap {
            let entry = old_ptr.add(i);
            if (*entry).state == STATE_OCCUPIED {
                self.reinsert(new_ptr, new_cap, (*entry).key, (*entry).meta);
            }
        }

        self.ptr.store(new_ptr, Ordering::Relaxed);
        self.cap.store(new_cap, Ordering::Relaxed);
        self.tombstones.store(0, Ordering::Relaxed);

        let old_size = old_cap * size_of::<Entry>();
        let _ = munmap(old_ptr as *mut c_void, old_size);
    }

    unsafe fn alloc_table(&self, cap: usize) {
        let ptr = self.alloc_raw(cap);
        self.ptr.store(ptr, Ordering::Relaxed);
        self.cap.store(cap, Ordering::Relaxed);
        self.len.store(0, Ordering::Relaxed);
        self.tombstones.store(0, Ordering::Relaxed);
    }

    unsafe fn alloc_raw(&self, cap: usize) -> *mut Entry {
        let size = cap * size_of::<Entry>();
        let ptr = mmap_anonymous(
            null_mut(),
            size,
            ProtFlags::READ | ProtFlags::WRITE,
            MapFlags::PRIVATE | MapFlags::NORESERVE,
        )
        .unwrap_or_else(|_| {
            RSMallocError::OutOfMemory.log_and_abort(
                null_mut(),
                "Cannot allocate BigAllocMap",
                None,
            )
        });

        ptr as *mut Entry
    }

    unsafe fn reinsert(&self, table: *mut Entry, cap: usize, key: usize, meta: BigAllocMeta) {
        let mut idx = hash_key(key) & (cap - 1);
        for _ in 0..cap {
            let entry = table.add(idx);
            if (*entry).state != STATE_OCCUPIED {
                (*entry).key = key;
                (*entry).meta = meta;
                (*entry).state = STATE_OCCUPIED;
                return;
            }
            idx = (idx + 1) & (cap - 1);
        }

        RSMallocError::OutOfMemory.log_and_abort(
            null_mut() as *mut c_void,
            "BigAllocMap reinsert failed (table full)",
            None,
        );
    }
}

#[inline(always)]
fn hash_key(key: usize) -> usize {
    let mut x = key as u64;
    x ^= x >> 33;
    x = x.wrapping_mul(0xff51afd7ed558ccd);
    x ^= x >> 33;
    x = x.wrapping_mul(0xc4ceb9fe1a85ec53);
    x ^= x >> 33;
    x as usize
}
