use std::{
    cell::UnsafeCell,
    mem::size_of,
    os::raw::c_void,
    ptr::{null_mut, write},
    sync::atomic::{AtomicPtr, AtomicUsize, Ordering},
};

use rustix::mm::{MapFlags, ProtFlags, mmap_anonymous};
#[cfg(feature = "guard-pages-thp")]
use rustix::mm::{MprotectFlags, mprotect};

use crate::{
    internals::{binder::NumaBind, lock::SpinLock, once::Once},
    record_mmap_call,
    rseq_core::{rseq_offsets::get_rseq, slab_cache::SLAB_CACHE},
    traits::Lock,
    utility::{Alignment, MIN_REFILL_BYTES},
};

const PAGE_SIZE: usize = 4096;
pub static mut ARENA_SIZE: usize = 1024 * 1024 * 256;

#[cfg(all(feature = "guard-pages-thp", not(feature = "guard-pages-ignore-thp")))]
const GUARD_ALIGN: usize = 2 * 1024 * 1024;
#[cfg(all(feature = "guard-pages-thp", feature = "guard-pages-ignore-thp"))]
const GUARD_ALIGN: usize = 1024 * 64;

#[cfg(feature = "guard-pages-thp")]
const GUARD_OFFSET: usize = GUARD_ALIGN - PAGE_SIZE;

#[cfg(feature = "guard-pages-thp")]
#[inline(always)]
unsafe fn skip_guard_page(addr: usize, end: usize) -> usize {
    if addr % GUARD_ALIGN == GUARD_OFFSET && addr < end {
        let _ = mprotect(addr as *mut c_void, PAGE_SIZE, MprotectFlags::empty());
        return addr + PAGE_SIZE;
    }
    addr
}

#[cfg(feature = "guard-pages-thp")]
#[inline(always)]
fn guard_page_in_range(start: usize, size: usize) -> Option<usize> {
    let end = start.checked_add(size)?;
    let block_base = start - (start % GUARD_ALIGN);
    let mut guard = block_base + GUARD_OFFSET;
    if guard < start {
        guard += GUARD_ALIGN;
    }
    if guard < end { Some(guard) } else { None }
}

#[cfg(feature = "guard-pages-thp")]
#[inline(always)]
fn fits_within_guard_segment(size: usize) -> bool {
    size <= GUARD_OFFSET
}

#[cfg(feature = "debug")]
pub static TOTAL_REMOVED: AtomicUsize = AtomicUsize::new(0);
#[cfg(feature = "debug")]
pub static TOTAL_LIVED: AtomicUsize = AtomicUsize::new(0);

struct PageArena {
    next: *mut PageArena,
    prev: *mut PageArena,
    base: usize,
    end: usize,
    current: AtomicUsize,
}

struct NodeArenaState {
    arenas: *mut PageArena,
}

#[repr(C, align(64))]
struct NodeArena {
    lock: SpinLock<NodeArenaState>,
    current: AtomicPtr<PageArena>,
    node_id: u16,
}

unsafe impl Sync for NodeArena {}

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
                            lock: SpinLock::new(NodeArenaState { arenas: null_mut() }),
                            current: AtomicPtr::new(null_mut()),
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

    #[cfg(feature = "preload")]
    pub unsafe fn lock_all_for_fork(&self) {
        let inner = &*self.inner.get();
        if inner.arenas.is_null() {
            return;
        }

        for node in 0..inner.node_count {
            core::mem::forget((*inner.arenas.add(node)).lock.lock());
        }
    }

    #[cfg(feature = "preload")]
    pub unsafe fn reset_locks_on_fork(&self) {
        let inner = &*self.inner.get();
        if inner.arenas.is_null() {
            return;
        }

        for node in 0..inner.node_count {
            (*inner.arenas.add(node)).lock.reset_at_fork();
        }
    }

    #[cfg(feature = "debug")]
    pub unsafe fn arena_counts(&self) -> Vec<usize> {
        let inner = &*self.inner.get();
        if inner.arenas.is_null() {
            return Vec::new();
        }

        let mut counts = Vec::with_capacity(inner.node_count);
        for node in 0..inner.node_count {
            let node_ref = &*inner.arenas.add(node);
            let state = node_ref.lock.lock();

            let mut count = 0usize;
            let mut arena = state.arenas;
            while !arena.is_null() {
                count += 1;
                arena = (*arena).next;
            }

            counts.push(count);
        }

        counts
    }

    #[inline(always)]
    pub unsafe fn alloc(&self, node_id: Option<u16>, size: usize) -> Option<*mut c_void> {
        let size = (size.max(1)).checked_align_to(PAGE_SIZE)?;
        let inner = &*self.inner.get();

        if inner.arenas.is_null() || inner.node_count == 0 {
            return None;
        }

        let node_id = node_id.unwrap_or_else(|| {
            let inner = SLAB_CACHE.get_inner();
            let cpu_id = get_rseq().cpu_id as usize;
            SLAB_CACHE.node_for_cpu(cpu_id, inner)
        });

        let node = if node_id as usize >= inner.node_count {
            0
        } else {
            node_id as usize
        };

        Self::alloc_inner(&*inner.arenas.add(node), size)
    }

    unsafe fn maybe_remove(state: &mut NodeArenaState, node: &NodeArena, current: *mut PageArena) {
        if !current.is_null()
            && (*current).end - (*current).current.load(Ordering::Relaxed) < MIN_REFILL_BYTES
        {
            Self::remove_arena(state, node, current);
        }
    }

    #[inline(always)]
    unsafe fn alloc_inner(node: &NodeArena, size: usize) -> Option<*mut c_void> {
        let current = node.current.load(Ordering::Acquire);
        if let Some(ptr) = Self::allocate_from_arena(current, size) {
            return Some(ptr);
        }
        Self::alloc_slow(current, node, size)
    }

    #[inline(never)]
    unsafe fn alloc_slow(
        old_arena: *mut PageArena,
        node: &NodeArena,
        size: usize,
    ) -> Option<*mut c_void> {
        let state = &mut *node.lock.lock();

        let current = node.current.load(Ordering::Acquire);
        if current != old_arena {
            if let Some(ptr) = Self::allocate_from_arena(current, size) {
                return Some(ptr);
            }
        }
        Self::maybe_remove(state, node, current);

        if let Some(ptr) = Self::allocate_search(state, node, size) {
            return Some(ptr);
        }

        let arena = Self::new_arena_locked(state, node.node_id, size)?;
        node.current.store(arena, Ordering::Release);
        Self::allocate_from_arena(arena, size)
    }

    #[inline(always)]
    unsafe fn allocate_search(
        state: &mut NodeArenaState,
        node: &NodeArena,
        size: usize,
    ) -> Option<*mut c_void> {
        let mut arena = state.arenas;

        while !arena.is_null() {
            let next_arena = (*arena).next;

            if let Some(ptr) = Self::allocate_from_arena(arena, size) {
                node.current.store(arena, Ordering::Release);
                return Some(ptr);
            }

            Self::maybe_remove(state, node, arena);

            arena = next_arena;
        }

        None
    }

    #[inline(always)]
    unsafe fn allocate_from_arena(arena: *mut PageArena, size: usize) -> Option<*mut c_void> {
        if arena.is_null() {
            return None;
        }
        let end = (*arena).end;
        let mut observed = (*arena).current.load(Ordering::Acquire);

        loop {
            #[cfg(not(feature = "guard-pages-thp"))]
            let start = observed;

            #[cfg(feature = "guard-pages-thp")]
            let start = {
                let mut start = skip_guard_page(observed, end);
                if let Some(guard) = guard_page_in_range(start, size) {
                    start = skip_guard_page(guard, end);
                }
                start
            };

            let next = start.checked_add(size)?;
            if next > end {
                return None;
            }

            #[cfg(feature = "guard-pages-thp")]
            if fits_within_guard_segment(size) && guard_page_in_range(start, size).is_some() {
                return None;
            }

            match (*arena).current.compare_exchange_weak(
                observed,
                next,
                Ordering::Release,
                Ordering::Relaxed,
            ) {
                Ok(_) => return Some(start as *mut c_void),
                Err(current) => observed = current,
            }
        }
    }

    pub unsafe fn try_grow_inplace(
        &self,
        node_id: u16,
        ptr: *mut c_void,
        old_size: usize,
        new_size: usize,
    ) -> bool {
        if ptr.is_null() || new_size <= old_size {
            return false;
        }

        let old_size = old_size.max(1).align_to(PAGE_SIZE);
        let Some(new_size) = new_size.max(1).checked_align_to(PAGE_SIZE) else {
            return false;
        };
        if new_size <= old_size {
            return true;
        }

        let inner = &*self.inner.get();
        if inner.arenas.is_null() || inner.node_count == 0 {
            return false;
        }

        let node = if node_id as usize >= inner.node_count {
            0
        } else {
            node_id as usize
        };

        let node = &*inner.arenas.add(node);
        let state = &mut *node.lock.lock();

        let addr = ptr as usize;
        let mut arena = state.arenas;

        while !arena.is_null() {
            let arena_ref = &mut *arena;
            if addr >= arena_ref.base && addr < arena_ref.end {
                if (addr - arena_ref.base) % PAGE_SIZE != 0 {
                    return false;
                }

                let Some(old_end) = addr.checked_add(old_size) else {
                    return false;
                };

                let Some(new_end) = addr.checked_add(new_size) else {
                    return false;
                };

                if new_end > arena_ref.end {
                    return false;
                }

                #[cfg(feature = "guard-pages-thp")]
                if fits_within_guard_segment(new_size - old_size)
                    && guard_page_in_range(old_end, new_size - old_size).is_some()
                {
                    return false;
                }

                if arena_ref
                    .current
                    .compare_exchange(old_end, new_end, Ordering::Release, Ordering::Relaxed)
                    .is_err()
                {
                    return false;
                }

                if arena_ref.end - new_end < MIN_REFILL_BYTES {
                    Self::remove_arena(state, node, arena);
                }

                return true;
            }

            arena = arena_ref.next;
        }

        false
    }

    unsafe fn remove_arena(
        state: &mut NodeArenaState,
        node: &NodeArena,
        arena_base: *mut PageArena,
    ) {
        if arena_base.is_null() {
            return;
        }

        let arena = &mut *arena_base;

        let next = arena.next;
        let prev = arena.prev;

        if !prev.is_null() {
            (*prev).next = next;
        } else {
            state.arenas = next;
        }

        if !next.is_null() {
            (*next).prev = prev;
        }

        let _ =
            node.current
                .compare_exchange(arena_base, next, Ordering::Release, Ordering::Relaxed);

        #[cfg(feature = "debug")]
        TOTAL_REMOVED.fetch_add(1, Ordering::Relaxed);
    }

    #[cold]
    #[inline(never)]
    unsafe fn new_arena_locked(
        state: &mut NodeArenaState,
        node_id: u16,
        requested: usize,
    ) -> Option<*mut PageArena> {
        let data_size = requested.max(ARENA_SIZE).align_to(PAGE_SIZE);

        #[cfg(feature = "guard-pages-thp")]
        let data_size = data_size.checked_add(PAGE_SIZE)?;
        let metadata_size = size_of::<PageArena>().align_to(PAGE_SIZE);
        let map_size = metadata_size.checked_add(data_size)?;

        record_mmap_call(map_size);
        let mem = mmap_anonymous(
            null_mut(),
            map_size,
            ProtFlags::READ | ProtFlags::WRITE,
            MapFlags::PRIVATE,
        )
        .ok()?;

        #[cfg(all(
            feature = "page-backend-no-huge-page",
            not(feature = "page-backend-huge-page")
        ))]
        let _ = rustix::mm::madvise(mem, map_size, rustix::mm::Advice::LinuxNoHugepage);

        #[cfg(all(
            feature = "page-backend-huge-page",
            not(feature = "page-backend-no-huge-page")
        ))]
        let _ = rustix::mm::madvise(mem, map_size, rustix::mm::Advice::LinuxHugepage);

        NumaBind.prefer_node(mem, map_size, node_id);

        let arena = mem as *mut PageArena;
        let base = mem as usize + metadata_size;

        write(
            arena,
            PageArena {
                next: state.arenas,
                prev: null_mut(),
                base,
                end: base.checked_add(data_size)?,
                current: AtomicUsize::new(base),
            },
        );

        if !state.arenas.is_null() {
            (*state.arenas).prev = arena;
        }

        state.arenas = arena;

        #[cfg(feature = "debug")]
        TOTAL_LIVED.fetch_add(1, Ordering::Relaxed);

        Some(arena)
    }
}

pub static PAGE_ALLOCATOR: PageAllocator = PageAllocator::new();
