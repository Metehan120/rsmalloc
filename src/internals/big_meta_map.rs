use rustix::mm::{MapFlags, ProtFlags, mmap_anonymous};

use crate::{
    BigAllocMeta, RSMallocError,
    backend::page_allocator::{ARENA_SIZE, PAGE_ALLOCATOR},
    internals::lock::SpinLock,
    record_mmap_call,
    traits::Lock,
};
use std::{mem::size_of, os::raw::c_void, ptr::null_mut};

const SHARD_COUNT: usize = 64;
const SHARD_MASK: usize = SHARD_COUNT - 1;
const BUCKETS_PER_SHARD: usize = 64;
const BUCKET_MASK: usize = BUCKETS_PER_SHARD - 1;
const NODE_CHUNK: usize = 64;

struct Node {
    key: usize,
    meta: BigAllocMeta,
    next: *mut Node,
}

const _: () = assert!(size_of::<Node>() <= 64);

struct ShardInner {
    buckets: [*mut Node; BUCKETS_PER_SHARD],
    free_list: *mut Node,
}

impl ShardInner {
    const fn new() -> Self {
        Self {
            buckets: [null_mut(); BUCKETS_PER_SHARD],
            free_list: null_mut(),
        }
    }
}

#[repr(align(64))]
struct Shard {
    inner: SpinLock<ShardInner>,
}

impl Shard {
    const fn new() -> Self {
        Self {
            inner: SpinLock::new(ShardInner::new()),
        }
    }
}

pub struct BigMetaMap {
    shards: [Shard; SHARD_COUNT],
}

pub static BIG_META_MAP: BigMetaMap = BigMetaMap::new();
pub use BIG_META_MAP as BIG_MAP;

impl BigMetaMap {
    pub const fn new() -> Self {
        Self {
            shards: [const { Shard::new() }; SHARD_COUNT],
        }
    }

    #[inline(always)]
    fn location(key: usize) -> (usize, usize) {
        let mut hash = key >> 4;
        hash ^= hash >> 33;
        hash = hash.wrapping_mul(0xff51afd7ed558ccdusize);
        hash ^= hash >> 33;
        hash = hash.wrapping_mul(0xc4ceb9fe1a85ec53usize);
        hash ^= hash >> 33;

        (hash & SHARD_MASK, (hash >> 6) & BUCKET_MASK)
    }

    pub unsafe fn insert(&self, key: usize, meta: BigAllocMeta) {
        let (shard_index, bucket_index) = Self::location(key);
        let mut inner = self.shards[shard_index].inner.lock();
        let mut node = inner.buckets[bucket_index];

        while !node.is_null() {
            if (*node).key == key {
                (*node).meta = meta;
                return;
            }
            node = (*node).next;
        }

        let node = self.alloc_node(&mut inner, key, meta);
        (*node).next = inner.buckets[bucket_index];
        inner.buckets[bucket_index] = node;
    }

    pub unsafe fn get(&self, key: usize) -> Option<BigAllocMeta> {
        let (shard_index, bucket_index) = Self::location(key);
        let inner = self.shards[shard_index].inner.lock();
        let mut node = inner.buckets[bucket_index];

        while !node.is_null() {
            if (*node).key == key {
                return Some((*node).meta);
            }
            node = (*node).next;
        }

        None
    }

    pub unsafe fn replace(&self, key: usize, meta: BigAllocMeta) -> Option<BigAllocMeta> {
        let (shard_index, bucket_index) = Self::location(key);
        let inner = self.shards[shard_index].inner.lock();
        let mut node = inner.buckets[bucket_index];

        while !node.is_null() {
            if (*node).key == key {
                let old = (*node).meta;
                (*node).meta = meta;
                return Some(old);
            }
            node = (*node).next;
        }

        None
    }

    pub unsafe fn remove(&self, key: usize) -> Option<BigAllocMeta> {
        let (shard_index, bucket_index) = Self::location(key);
        let mut inner = self.shards[shard_index].inner.lock();
        let mut link = &mut inner.buckets[bucket_index] as *mut *mut Node;

        while !(*link).is_null() {
            let node = *link;
            if (*node).key == key {
                *link = (*node).next;
                let meta = (*node).meta;
                (*node).next = inner.free_list;
                inner.free_list = node;
                return Some(meta);
            }
            link = &mut (*node).next;
        }

        None
    }

    #[cfg(feature = "preload")]
    pub fn lock_for_fork(&self) {
        for shard in &self.shards {
            core::mem::forget(shard.inner.lock());
        }
    }

    #[cfg(feature = "preload")]
    pub fn reset_lock_on_fork(&self) {
        for shard in &self.shards {
            shard.inner.reset_at_fork();
        }
    }

    unsafe fn map_mem(&self, size: usize) -> Result<*mut c_void, RSMallocError> {
        record_mmap_call(size);
        mmap_anonymous(
            null_mut(),
            size,
            ProtFlags::READ | ProtFlags::WRITE,
            MapFlags::PRIVATE | MapFlags::NORESERVE,
        )
        .map_err(|e| RSMallocError::OutOfMemory {
            subsystem: "big_meta_map.rs alloc_chunk",
            size,
            errno: Some(e.raw_os_error()),
        })
    }

    unsafe fn alloc_chunk(&self, inner: &mut ShardInner) {
        let size = NODE_CHUNK * size_of::<Node>();
        let ptr = if size < ARENA_SIZE {
            PAGE_ALLOCATOR
                .alloc(None, size)
                .or_else(|| self.map_mem(size).ok())
                .ok_or(RSMallocError::OutOfMemory {
                    subsystem: "big_meta_map.rs alloc_chunk",
                    size,
                    errno: None,
                })
        } else {
            self.map_mem(size)
        }
        .unwrap_or_else(|e| e.log_and_abort()) as *mut Node;

        for index in 0..NODE_CHUNK {
            let node = ptr.add(index);
            (*node).next = if index + 1 < NODE_CHUNK {
                ptr.add(index + 1)
            } else {
                inner.free_list
            };
        }

        inner.free_list = ptr;
    }

    unsafe fn alloc_node(
        &self,
        inner: &mut ShardInner,
        key: usize,
        meta: BigAllocMeta,
    ) -> *mut Node {
        if inner.free_list.is_null() {
            self.alloc_chunk(inner);
        }

        let node = inner.free_list;
        inner.free_list = (*node).next;
        (*node).key = key;
        (*node).meta = meta;
        (*node).next = null_mut();
        node
    }
}

unsafe impl Sync for BigMetaMap {}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashMap as StdHashMap;

    fn meta(size: usize) -> BigAllocMeta {
        BigAllocMeta {
            size,
            order: 0,
            segmented_bitmap_region: 0,
            aligned: false,
        }
    }

    #[test]
    fn insert_get_replace_remove_matches_reference_hashmap() {
        let map = BigMetaMap::new();
        let mut reference: StdHashMap<usize, usize> = StdHashMap::new();

        let mut state: u64 = 0x2545F4914F6CDD1D;
        let mut next_rand = || {
            state ^= state << 13;
            state ^= state >> 7;
            state ^= state << 17;
            state
        };

        unsafe {
            for i in 0..5000usize {
                let op = next_rand() % 4;
                let key = ((next_rand() % 500) as usize) * 16 + 0x1000;
                match op {
                    0 | 1 => {
                        map.insert(key, meta(i));
                        reference.insert(key, i);
                    }
                    2 => {
                        let got = map.get(key).map(|m| m.size);
                        assert_eq!(got, reference.get(&key).copied());
                    }
                    _ => {
                        let got = map.remove(key).map(|m| m.size);
                        assert_eq!(got, reference.remove(&key));
                    }
                }
            }

            for (key, size) in &reference {
                assert_eq!(map.get(*key).map(|m| m.size), Some(*size));
            }
        }
    }

    #[test]
    fn replace_returns_old_value_and_leaves_key_present() {
        let map = BigMetaMap::new();
        unsafe {
            assert!(map.replace(42, meta(1)).is_none());
            map.insert(42, meta(1));
            let old = map.replace(42, meta(2));
            assert_eq!(old.map(|m| m.size), Some(1));
            assert_eq!(map.get(42).map(|m| m.size), Some(2));
        }
    }

    #[test]
    fn keys_are_distributed_across_shards() {
        let mut seen = [false; SHARD_COUNT];
        for index in 0..4096usize {
            let key = 0x1000_0000usize + index * 4 * 1024 * 1024;
            seen[BigMetaMap::location(key).0] = true;
        }

        assert!(seen.into_iter().all(|value| value));
    }
}
