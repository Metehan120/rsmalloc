use std::{
    cell::UnsafeCell,
    mem::size_of,
    os::raw::c_void,
    ptr::{null_mut, write},
    sync::Mutex,
};

#[cfg(feature = "debug")]
use std::sync::atomic::{AtomicUsize, Ordering};

use rustix::mm::{Advice, MapFlags, ProtFlags, madvise, mmap_anonymous};

use crate::{
    internals::{binder::prefer_node, once::Once},
    record_mmap_call,
    utility::align_to,
};

const PAGE_SIZE: usize = 4096;
pub static mut ARENA_SIZE: usize = 1024 * 1024 * 256;
const BITS_PER_WORD: usize = 64;
#[cfg(feature = "debug")]
pub static TOTAL_REMOVED: AtomicUsize = AtomicUsize::new(0);
#[cfg(feature = "debug")]
pub static TOTAL_LIVED: AtomicUsize = AtomicUsize::new(0);

struct PageArena {
    next: *mut PageArena,
    prev: *mut PageArena,
    base: usize,
    end: usize,
    current: usize,
    page_count: usize,
    search_hint: usize,
    bitmap: *mut u64,
}

impl PageArena {
    #[inline(always)]
    unsafe fn all_used(&self, start: usize, pages: usize) -> bool {
        let mut page = start;
        let mut remaining = pages;

        while remaining != 0 {
            let word = page / BITS_PER_WORD;
            let first_bit = page & (BITS_PER_WORD - 1);
            let bits = remaining.min(BITS_PER_WORD - first_bit);
            let bitmap_word = *self.bitmap.add(word);

            let mask = if bits == BITS_PER_WORD {
                u64::MAX
            } else {
                ((1u64 << bits) - 1) << first_bit
            };

            if bitmap_word & mask != mask {
                return false;
            }

            page += bits;
            remaining -= bits;
        }

        true
    }

    #[inline(always)]
    unsafe fn all_free(&self, start: usize, pages: usize) -> bool {
        let mut page = start;
        let mut remaining = pages;

        while remaining != 0 {
            let word = page / BITS_PER_WORD;
            let first_bit = page & (BITS_PER_WORD - 1);
            let bits = remaining.min(BITS_PER_WORD - first_bit);
            let bitmap_word = *self.bitmap.add(word);

            let mask = if bits == BITS_PER_WORD {
                u64::MAX
            } else {
                ((1u64 << bits) - 1) << first_bit
            };

            if bitmap_word & mask != 0 {
                return false;
            }

            page += bits;
            remaining -= bits;
        }

        true
    }

    #[inline(always)]
    unsafe fn mark_range(&mut self, start: usize, pages: usize) {
        let mut page = start;
        let mut remaining = pages;

        while remaining != 0 {
            let word = page / BITS_PER_WORD;
            let first_bit = page & (BITS_PER_WORD - 1);
            let bits = remaining.min(BITS_PER_WORD - first_bit);
            let bitmap_word = self.bitmap.add(word);

            if bits == BITS_PER_WORD {
                *bitmap_word = u64::MAX;
            } else {
                let mask = ((1u64 << bits) - 1) << first_bit;
                *bitmap_word |= mask;
            }

            page += bits;
            remaining -= bits;
        }
    }
}

struct NodeArenaState {
    current: *mut PageArena,
    arenas: *mut PageArena,
}

#[repr(C, align(64))]
struct NodeArena {
    state: Mutex<NodeArenaState>,
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
                            state: Mutex::new(NodeArenaState {
                                current: null_mut(),
                                arenas: null_mut(),
                            }),
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

    #[cfg(feature = "debug")]
    pub unsafe fn arena_counts(&self) -> Vec<usize> {
        let inner = &*self.inner.get();
        if inner.arenas.is_null() {
            return Vec::new();
        }

        let mut counts = Vec::with_capacity(inner.node_count);
        for node in 0..inner.node_count {
            let node_ref = &*inner.arenas.add(node);
            let state = node_ref.state.lock().unwrap_or_else(|e| e.into_inner());

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
    pub unsafe fn alloc(&self, node_id: u16, size: usize) -> Option<*mut c_void> {
        let size = align_to(size.max(1), PAGE_SIZE);
        let pages = size / PAGE_SIZE;
        let inner = &mut *self.inner.get();

        if inner.arenas.is_null() || inner.node_count == 0 {
            return None;
        }

        let node = if node_id as usize >= inner.node_count {
            0
        } else {
            node_id as usize
        };

        let node = &*inner.arenas.add(node);
        let mut state = node.state.lock().unwrap_or_else(|e| e.into_inner());

        if let Some(ptr) = Self::allocate_bump(&mut state, pages) {
            return Some(ptr);
        }

        if let Some(ptr) = Self::allocate_bit(&mut state, pages, size) {
            return Some(ptr);
        }

        Self::new_arena_locked(&mut state, node.node_id, size)?;
        Self::allocate_bump(&mut state, pages)
    }

    #[inline(always)]
    unsafe fn allocate_bump(state: &mut NodeArenaState, pages: usize) -> Option<*mut c_void> {
        if state.current.is_null() {
            return None;
        }

        let arena = &mut *state.current;
        let bytes = pages.checked_mul(PAGE_SIZE)?;
        let next = arena.current.checked_add(bytes)?;
        if next > arena.end {
            return None;
        }

        let start_page = (arena.current - arena.base) / PAGE_SIZE;
        let ptr = arena.current as *mut c_void;

        arena.mark_range(start_page, pages);
        arena.current = next;
        arena.search_hint = start_page.saturating_add(pages).min(arena.page_count);

        if arena.current == arena.end {
            Self::remove_arena(state, arena);
        }

        Some(ptr)
    }

    #[inline(always)]
    unsafe fn allocate_bit(
        state: &mut NodeArenaState,
        pages: usize,
        bytes: usize,
    ) -> Option<*mut c_void> {
        let mut arena = state.arenas;

        while !arena.is_null() {
            let arena_ref = &mut *arena;

            if arena != state.current {
                if let Some(next) = arena_ref.current.checked_add(bytes)
                    && next <= arena_ref.end
                {
                    let start_page = (arena_ref.current - arena_ref.base) / PAGE_SIZE;
                    let ptr = arena_ref.current as *mut c_void;

                    arena_ref.mark_range(start_page, pages);
                    arena_ref.current = next;
                    arena_ref.search_hint =
                        start_page.saturating_add(pages).min(arena_ref.page_count);

                    if arena_ref.current == arena_ref.end {
                        Self::remove_arena(state, arena);
                    }

                    return Some(ptr);
                }
            }

            arena = arena_ref.next;
        }

        None
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

        let old_size = align_to(old_size.max(1), PAGE_SIZE);
        let new_size = align_to(new_size.max(1), PAGE_SIZE);
        if new_size <= old_size {
            return true;
        }

        let old_pages = old_size / PAGE_SIZE;
        let new_pages = new_size / PAGE_SIZE;
        let inner = &mut *self.inner.get();

        if old_pages == 0
            || new_pages <= old_pages
            || inner.arenas.is_null()
            || inner.node_count == 0
        {
            return false;
        }

        let node = if node_id as usize >= inner.node_count {
            0
        } else {
            node_id as usize
        };

        let node = &*inner.arenas.add(node);
        let state = node.state.lock().unwrap_or_else(|e| e.into_inner());
        let addr = ptr as usize;
        let mut arena = state.arenas;

        while !arena.is_null() {
            let arena_ref = &mut *arena;
            if addr >= arena_ref.base && addr < arena_ref.end {
                if (addr - arena_ref.base) & (PAGE_SIZE - 1) != 0 {
                    return false;
                }

                let start_page = (addr - arena_ref.base) / PAGE_SIZE;
                let Some(old_end) = start_page.checked_add(old_pages) else {
                    return false;
                };

                let Some(new_end) = start_page.checked_add(new_pages) else {
                    return false;
                };

                if new_end > arena_ref.page_count {
                    return false;
                }

                if !arena_ref.all_used(start_page, old_end - start_page) {
                    return false;
                }

                if !arena_ref.all_free(old_end, new_end - old_end) {
                    return false;
                }

                arena_ref.mark_range(old_end, new_end - old_end);
                let new_current = arena_ref.base + new_end * PAGE_SIZE;
                if new_current > arena_ref.current {
                    arena_ref.current = new_current;
                }
                arena_ref.search_hint = new_end.min(arena_ref.page_count);
                return true;
            }

            arena = arena_ref.next;
        }

        false
    }

    unsafe fn remove_arena(state: &mut NodeArenaState, arena_base: *mut PageArena) {
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

        if state.current == arena_base {
            state.current = next;
        }

        #[cfg(feature = "debug")]
        TOTAL_REMOVED.fetch_add(1, Ordering::Relaxed);

        // avoid too much madvise calls on small arena sizes
        // important for overall performance of the allocator;
        // we shouldnt stall too much even in the slowest path
        if ARENA_SIZE >= 1024 * 1024 * 16 {
            let size = arena.end - arena.base;
            let page_count = size / PAGE_SIZE;
            let bitmap_words = (page_count + BITS_PER_WORD - 1) / BITS_PER_WORD;
            let metadata_size = align_to(
                size_of::<PageArena>() + bitmap_words * size_of::<u64>(),
                PAGE_SIZE,
            );

            let _ = madvise(arena_base as *mut c_void, metadata_size, Advice::DontNeed);
        }
    }

    #[cold]
    #[inline(never)]
    unsafe fn new_arena_locked(
        state: &mut NodeArenaState,
        node_id: u16,
        requested: usize,
    ) -> Option<()> {
        let data_size = align_to(requested.max(ARENA_SIZE), PAGE_SIZE);
        let page_count = data_size / PAGE_SIZE;
        let bitmap_words = (page_count + BITS_PER_WORD - 1) / BITS_PER_WORD;
        let metadata_size = align_to(
            size_of::<PageArena>() + bitmap_words * size_of::<u64>(),
            PAGE_SIZE,
        );
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
        let _ = madvise(mem, map_size, Advice::LinuxNoHugepage);

        #[cfg(all(
            feature = "page-backend-huge-page",
            not(feature = "page-backend-no-huge-page")
        ))]
        let _ = madvise(mem, map_size, Advice::LinuxHugepage);

        prefer_node(mem, map_size, node_id);

        let arena = mem as *mut PageArena;
        let base = mem as usize + metadata_size;
        let bitmap = (mem as usize + size_of::<PageArena>()) as *mut u64;

        write(
            arena,
            PageArena {
                next: state.arenas,
                prev: null_mut(),
                base,
                end: base.checked_add(data_size)?,
                current: base,
                page_count,
                search_hint: 0,
                bitmap,
            },
        );

        if !state.arenas.is_null() {
            (*state.current).prev = arena;
        }

        state.arenas = arena;
        state.current = arena;

        #[cfg(feature = "debug")]
        TOTAL_LIVED.fetch_add(1, Ordering::Relaxed);

        Some(())
    }
}

pub static PAGE_ALLOCATOR: PageAllocator = PageAllocator::new();

#[cfg(test)]
mod tests {
    use super::*;

    fn test_arena(bitmap: &mut [u64; 3]) -> PageArena {
        PageArena {
            next: null_mut(),
            prev: null_mut(),
            base: 0,
            end: PAGE_SIZE * BITS_PER_WORD * bitmap.len(),
            current: 0,
            page_count: BITS_PER_WORD * bitmap.len(),
            search_hint: 0,
            bitmap: bitmap.as_mut_ptr(),
        }
    }

    #[test]
    fn mark_range_batches_full_words_and_preserves_boundaries() {
        let mut bitmap = [0u64; 3];
        let mut arena = test_arena(&mut bitmap);

        unsafe { arena.mark_range(63, 66) };

        assert_eq!(bitmap, [1u64 << 63, u64::MAX, 1]);
    }
}
