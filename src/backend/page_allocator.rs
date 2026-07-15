use std::{
    cell::UnsafeCell,
    mem::size_of,
    os::raw::c_void,
    ptr::{null_mut, write},
};

#[cfg(feature = "page-backend-no-huge-page")]
use rustix::mm::{Advice, madvise};
use rustix::mm::{MapFlags, ProtFlags, mmap_anonymous};

use crate::{
    internals::{binder::prefer_node, lock::SpinLock, once::Once},
    record_mmap_call,
    utility::align_to,
};

const PAGE_SIZE: usize = 4096;
const ARENA_SIZE: usize = 64 * 1024 * 1024;

#[repr(C, align(64))]
struct NodeArena {
    lock: SpinLock,
    current: usize,
    end: usize,
    node_id: u16,
}

struct InnerTable {
    arenas: *mut NodeArena,
    node_count: usize,
}

pub struct PageAllocator {
    inner: UnsafeCell<InnerTable>,
    once: Once,
}

unsafe impl Sync for PageAllocator {}
unsafe impl Send for PageAllocator {}

impl PageAllocator {
    pub const fn new() -> Self {
        Self {
            inner: UnsafeCell::new(InnerTable {
                arenas: null_mut(),
                node_count: 0,
            }),
            once: Once::new(),
        }
    }

    #[cold]
    #[inline(never)]
    pub unsafe fn init(&self, node_count: usize) {
        self.once.call_once(|| {
            let node_count = node_count.max(1);
            let bytes = size_of::<NodeArena>() * node_count;

            record_mmap_call(bytes);
            let Ok(mem) = mmap_anonymous(
                null_mut(),
                bytes,
                ProtFlags::READ | ProtFlags::WRITE,
                MapFlags::PRIVATE,
            ) else {
                return;
            };

            let arenas = mem as *mut NodeArena;
            for node in 0..node_count {
                unsafe {
                    write(
                        arenas.add(node),
                        NodeArena {
                            lock: SpinLock::new(),
                            current: 0,
                            end: 0,
                            node_id: node as u16,
                        },
                    );
                }
            }

            let inner = unsafe { &mut *self.inner.get() };
            inner.arenas = arenas;
            inner.node_count = node_count;
        });
    }

    #[inline(always)]
    pub unsafe fn alloc(&self, node_id: u16, size: usize) -> Option<*mut c_void> {
        let size = align_to(size.max(1), PAGE_SIZE);
        let inner = &mut *self.inner.get();

        if inner.arenas.is_null() || inner.node_count == 0 {
            return None;
        }

        let node = if node_id as usize >= inner.node_count {
            0
        } else {
            node_id as usize
        };
        let arena = &mut *inner.arenas.add(node);
        let _guard = arena.lock.lock();

        if let Some(ptr) = Self::try_alloc_locked(arena, size) {
            return Some(ptr);
        }

        Self::new_arena_locked(arena, size)?;
        Self::try_alloc_locked(arena, size)
    }

    #[inline(always)]
    fn try_alloc_locked(arena: &mut NodeArena, size: usize) -> Option<*mut c_void> {
        let next = arena.current.checked_add(size)?;
        if next <= arena.end {
            let ptr = arena.current as *mut c_void;
            arena.current = next;
            Some(ptr)
        } else {
            None
        }
    }

    #[cold]
    #[inline(never)]
    unsafe fn new_arena_locked(arena: &mut NodeArena, requested: usize) -> Option<()> {
        let map_size = align_to(requested.max(ARENA_SIZE), PAGE_SIZE);

        record_mmap_call(map_size);
        let mem = mmap_anonymous(
            null_mut(),
            map_size,
            ProtFlags::READ | ProtFlags::WRITE,
            MapFlags::PRIVATE,
        )
        .ok()?;

        #[cfg(feature = "page-backend-no-huge-page")]
        let _ = madvise(mem, map_size, Advice::LinuxNoHugepage);

        prefer_node(mem, map_size, arena.node_id);

        arena.current = mem as usize;
        arena.end = arena.current.checked_add(map_size)?;

        Some(())
    }
}

pub static PAGE_ALLOCATOR: PageAllocator = PageAllocator::new();
