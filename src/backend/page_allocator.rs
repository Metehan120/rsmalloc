// Audit PageAllocator later before Alpha-3 release
//
// - Metehan

use std::{
    cell::UnsafeCell,
    mem::size_of,
    os::raw::c_void,
    ptr::{null_mut, write},
};

#[cfg(any(
    all(
        feature = "page-backend-no-huge-page",
        not(feature = "page-backend-huge-page")
    ),
    all(
        feature = "page-backend-huge-page",
        not(feature = "page-backend-no-huge-page")
    )
))]
use rustix::mm::{Advice, madvise};
use rustix::mm::{MapFlags, ProtFlags, mmap_anonymous};

use crate::{
    internals::{binder::prefer_node, lock::SpinLock, once::Once},
    record_mmap_call,
    utility::align_to,
};

const PAGE_SIZE: usize = 4096;
pub static mut ARENA_SIZE: usize = 1024 * 1024 * 256;
const BITS_PER_WORD: usize = 64;

struct PageArena {
    next: *mut PageArena,
    base: usize,
    end: usize,
    current: usize,
    page_count: usize,
    search_hint: usize,
    bitmap: *mut u64,
}

impl PageArena {
    #[inline(always)]
    unsafe fn page_ptr(&self, page: usize) -> *mut c_void {
        (self.base + page * PAGE_SIZE) as *mut c_void
    }

    #[inline(always)]
    unsafe fn is_used(&self, page: usize) -> bool {
        let word = page / BITS_PER_WORD;
        let bit = 1u64 << (page & (BITS_PER_WORD - 1));
        *self.bitmap.add(word) & bit != 0
    }

    #[inline(always)]
    unsafe fn mark_range(&mut self, start: usize, pages: usize) {
        for page in start..start + pages {
            let word = page / BITS_PER_WORD;
            let bit = 1u64 << (page & (BITS_PER_WORD - 1));
            *self.bitmap.add(word) |= bit;
        }
    }

    #[inline(always)]
    unsafe fn clear_range(&mut self, start: usize, pages: usize) {
        for page in start..start + pages {
            let word = page / BITS_PER_WORD;
            let bit = 1u64 << (page & (BITS_PER_WORD - 1));
            *self.bitmap.add(word) &= !bit;
        }
    }

    #[inline(always)]
    unsafe fn range_is_free(&self, start: usize, pages: usize) -> bool {
        if start
            .checked_add(pages)
            .is_none_or(|end| end > self.page_count)
        {
            return false;
        }

        for page in start..start + pages {
            if self.is_used(page) {
                return false;
            }
        }

        true
    }

    #[inline(always)]
    unsafe fn find_free_run_in(&self, start: usize, end: usize, pages: usize) -> Option<usize> {
        if pages == 0 || pages > self.page_count || start >= end {
            return None;
        }

        let mut page = start;
        let limit = end.saturating_sub(pages).saturating_add(1);

        while page < limit {
            if self.range_is_free(page, pages) {
                return Some(page);
            }
            page += 1;
        }

        None
    }

    #[inline(always)]
    unsafe fn find_free_run(&self, pages: usize) -> Option<usize> {
        let hint = self.search_hint.min(self.page_count);

        if let Some(page) = self.find_free_run_in(hint, self.page_count, pages) {
            return Some(page);
        }

        self.find_free_run_in(0, hint, pages)
    }
}

#[repr(C, align(64))]
struct NodeArena {
    lock: SpinLock,
    current: *mut PageArena,
    arenas: *mut PageArena,
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
                            current: null_mut(),
                            arenas: null_mut(),
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
        self.allocate(node_id, size)
    }

    #[inline(always)]
    unsafe fn allocate(&self, node_id: u16, size: usize) -> Option<*mut c_void> {
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
        let node = &mut *inner.arenas.add(node);
        let _guard = node.lock.lock();

        if let Some(ptr) = Self::allocate_bump(node, pages) {
            return Some(ptr);
        }

        if let Some(ptr) = Self::allocate_bit(node, pages) {
            return Some(ptr);
        }

        Self::new_arena_locked(node, size)?;
        Self::allocate_bump(node, pages)
    }

    #[inline(always)]
    unsafe fn allocate_bump(node: &mut NodeArena, pages: usize) -> Option<*mut c_void> {
        if node.current.is_null() {
            return None;
        }

        let arena = &mut *node.current;
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

        Some(ptr)
    }

    #[inline(always)]
    unsafe fn allocate_bit(node: &mut NodeArena, pages: usize) -> Option<*mut c_void> {
        let mut arena = node.arenas;

        while !arena.is_null() {
            let arena_ref = &mut *arena;
            if let Some(start_page) = arena_ref.find_free_run(pages) {
                arena_ref.mark_range(start_page, pages);
                arena_ref.search_hint = start_page.saturating_add(pages).min(arena_ref.page_count);
                return Some(arena_ref.page_ptr(start_page));
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
        let node = &mut *inner.arenas.add(node);
        let _guard = node.lock.lock();
        let addr = ptr as usize;
        let mut arena = node.arenas;

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

                for page in start_page..old_end {
                    if !arena_ref.is_used(page) {
                        return false;
                    }
                }

                for page in old_end..new_end {
                    if arena_ref.is_used(page) {
                        return false;
                    }
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

    #[allow(dead_code)]
    pub unsafe fn release(&self, node_id: u16, ptr: *mut c_void, size: usize) -> bool {
        if ptr.is_null() {
            return false;
        }

        let size = align_to(size.max(1), PAGE_SIZE);
        let pages = size / PAGE_SIZE;
        let inner = &mut *self.inner.get();

        if inner.arenas.is_null() || inner.node_count == 0 {
            return false;
        }

        let node = if node_id as usize >= inner.node_count {
            0
        } else {
            node_id as usize
        };
        let node = &mut *inner.arenas.add(node);
        let _guard = node.lock.lock();
        let addr = ptr as usize;
        let mut arena = node.arenas;

        while !arena.is_null() {
            let arena_ref = &mut *arena;
            if addr >= arena_ref.base && addr < arena_ref.end {
                if (addr - arena_ref.base) & (PAGE_SIZE - 1) != 0 {
                    return false;
                }

                let start_page = (addr - arena_ref.base) / PAGE_SIZE;
                if start_page
                    .checked_add(pages)
                    .is_none_or(|end| end > arena_ref.page_count)
                {
                    return false;
                }

                arena_ref.clear_range(start_page, pages);
                arena_ref.search_hint = arena_ref.search_hint.min(start_page);
                return true;
            }

            arena = arena_ref.next;
        }

        false
    }

    #[cold]
    #[inline(never)]
    unsafe fn new_arena_locked(node: &mut NodeArena, requested: usize) -> Option<()> {
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

        prefer_node(mem, map_size, node.node_id);

        let arena = mem as *mut PageArena;
        let base = mem as usize + metadata_size;
        let bitmap = (mem as usize + size_of::<PageArena>()) as *mut u64;

        write(
            arena,
            PageArena {
                next: node.arenas,
                base,
                end: base.checked_add(data_size)?,
                current: base,
                page_count,
                search_hint: 0,
                bitmap,
            },
        );

        node.arenas = arena;
        node.current = arena;

        Some(())
    }
}

pub static PAGE_ALLOCATOR: PageAllocator = PageAllocator::new();
