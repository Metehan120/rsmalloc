use std::{
    mem::size_of,
    os::raw::c_void,
    ptr::null_mut,
    sync::atomic::{AtomicU8, AtomicUsize, Ordering},
};

use rustix::{
    mm::{Advice, MapFlags, ProtFlags, madvise, mmap_anonymous, munmap},
    thread::sched_getcpu,
};

#[cfg(feature = "debug")]
use crate::trim::{TOTAL_TRIM_CALLS, TOTAL_TRIMMED_VA};
use crate::{
    BUDDY_AVERAGE_BLOCK_TIMES, BUDDY_INIT, CURRENT_STAMP, GLOBAL_TRIM_LOCK, add_buddy_cached_va,
    inner::alloc::MAX_REFILL_RETRIES,
    internals::{
        binder::prefer_node, l3_main_radix::RADIX, lock::SpinLock, numa_parser::NumaTopology,
        once::Once,
    },
    record_mmap_call,
    rseq_core::slab_cache::{SLAB_CACHE, SlabCacheInner},
    utility::align_to,
};

pub static BUDDY_TOTAL_CACHED_VA: AtomicUsize = AtomicUsize::new(0);

pub const BIG_BUDDY_MIN_ORDER: usize = 22; // 4 MiB
pub const BIG_BUDDY_MAX_ORDER: usize = 26; // 64 MiB
pub const BUDDY_NUM_ORDERS: usize = BIG_BUDDY_MAX_ORDER - BIG_BUDDY_MIN_ORDER + 1;
const NUM_ORDERS: usize = BUDDY_NUM_ORDERS;
const PAGE_SIZE: usize = 4096;
const BIG_BUDDY_MAX_BLOCK_SIZE: usize = 1 << BIG_BUDDY_MAX_ORDER;

pub const BUDDY_TRIM_NOT_ALLOCATED: u8 = 0;
const BUDDY_TRIM_ALLOCATED: u8 = 1;
pub const BUDDY_TRIM_TRIMMED: u8 = 2;

#[repr(C)]
#[derive(Debug, Clone, Copy)]
struct FreeBlock {
    next: *mut FreeBlock,
    life_time: u32,
    trim_state: u8,
}

impl FreeBlock {
    unsafe fn new(addr: usize, life_time: u32, trim_state: u8) -> *mut FreeBlock {
        let block = addr as *mut FreeBlock;
        core::ptr::write(
            block,
            FreeBlock {
                next: null_mut(),
                life_time,
                trim_state,
            },
        );
        block
    }
}

#[repr(C)]
struct BuddyRegion {
    next: *mut BuddyRegion,
    base: usize,
    total_size: usize,
    order: usize,
    node_id: u16,
    nonempty_mask: AtomicU8,
    free: [*mut FreeBlock; NUM_ORDERS],
    order_locks: [SpinLock; NUM_ORDERS],
}

impl BuddyRegion {
    const fn empty() -> Self {
        Self {
            next: null_mut(),
            base: 0,
            total_size: 0,
            order: BIG_BUDDY_MIN_ORDER,
            node_id: 0,
            nonempty_mask: AtomicU8::new(0),
            free: [null_mut(); NUM_ORDERS],
            order_locks: [const { SpinLock::new() }; NUM_ORDERS],
        }
    }
}

#[cfg(feature = "debug")]
pub struct BuddyBackendReport {
    pub regions: usize,
    pub total_region_bytes: usize,
    pub free_bytes: usize,
    pub free_blocks: [usize; BUDDY_NUM_ORDERS],
    pub never_allocated_blocks: usize,
    pub reused_blocks: usize,
    pub trimmed_blocks: usize,
    pub never_allocated_by_order: [usize; BUDDY_NUM_ORDERS],
    pub reused_by_order: [usize; BUDDY_NUM_ORDERS],
    pub trimmed_by_order: [usize; BUDDY_NUM_ORDERS],
    pub grow_order: usize,
    pub thp: bool,
}

pub struct BuddyAllocator {
    regions: *mut BuddyRegion,
    grow_order: usize,
    thp: bool,
    spin: SpinLock,
    once: Once,
}

impl BuddyAllocator {
    pub const fn new() -> Self {
        Self {
            regions: null_mut(),
            grow_order: BIG_BUDDY_MIN_ORDER,
            thp: false,
            spin: SpinLock::new(),
            once: Once::new(),
        }
    }

    #[inline(always)]
    fn order_for_size(size: usize) -> usize {
        size.max(1).next_power_of_two().trailing_zeros() as usize
    }

    #[inline(always)]
    fn normalize_region_size(size: usize) -> usize {
        size.checked_next_power_of_two()
            .unwrap_or(BIG_BUDDY_MAX_BLOCK_SIZE)
            .max(BIG_BUDDY_MAX_BLOCK_SIZE)
    }

    #[inline(always)]
    fn align_to_page(size: usize) -> usize {
        align_to(size, PAGE_SIZE)
    }

    #[inline(always)]
    fn order_index(order: usize) -> usize {
        order - BIG_BUDDY_MIN_ORDER
    }

    #[inline(always)]
    fn order_bit(order: usize) -> u8 {
        1 << Self::order_index(order)
    }

    #[inline(always)]
    unsafe fn mark_order_nonempty(region: *mut BuddyRegion, order: usize) {
        (*region)
            .nonempty_mask
            .fetch_or(Self::order_bit(order), Ordering::Release);
    }

    #[inline(always)]
    unsafe fn mark_order_empty_if_needed(region: *mut BuddyRegion, order: usize) {
        if (*region).free[Self::order_index(order)].is_null() {
            (*region)
                .nonempty_mask
                .fetch_and(!Self::order_bit(order), Ordering::Release);
        }
    }

    unsafe fn alloc_region_node() -> Option<*mut BuddyRegion> {
        let node_size = Self::align_to_page(size_of::<BuddyRegion>());

        for _ in 0..MAX_REFILL_RETRIES {
            record_mmap_call(node_size);
            if let Ok(region) = mmap_anonymous(
                null_mut(),
                node_size,
                ProtFlags::READ | ProtFlags::WRITE,
                MapFlags::PRIVATE,
            ) {
                add_buddy_cached_va(node_size);
                return Some(region as *mut BuddyRegion);
            }
        }

        None
    }

    #[inline(never)]
    unsafe fn add_region(&mut self, size: usize, node_id: u16, init: bool, is_numa: bool) -> bool {
        let normalized_size = Self::normalize_region_size(size);

        let mut retries = 0;
        let mut base = null_mut();
        while retries < MAX_REFILL_RETRIES {
            record_mmap_call(normalized_size);
            if let Ok(region) = mmap_anonymous(
                base,
                normalized_size,
                ProtFlags::READ | ProtFlags::WRITE,
                MapFlags::PRIVATE,
            ) {
                add_buddy_cached_va(normalized_size);
                if is_numa {
                    prefer_node(region, normalized_size, node_id);
                }
                base = region as *mut c_void;
                break;
            }
            retries += 1;
        }

        if retries == MAX_REFILL_RETRIES {
            return false;
        }
        let base = base as usize;

        let region_ptr = match Self::alloc_region_node() {
            Some(ptr) => ptr,
            None => {
                let _ = munmap(base as *mut c_void, normalized_size);
                return false;
            }
        };

        if self.thp {
            let _ = madvise(base as *mut c_void, normalized_size, Advice::LinuxHugepage);
        }

        RADIX.set_range(base, normalized_size, true);

        core::ptr::write(region_ptr, BuddyRegion::empty());

        (*region_ptr).base = base;
        (*region_ptr).total_size = normalized_size;
        (*region_ptr).order = BIG_BUDDY_MAX_ORDER;
        (*region_ptr).node_id = node_id;

        // A configured region may be larger than the largest allocation order
        // represent it as independent maximum-order blocks so the fixed order
        // tables and masks never need to index above BIG_BUDDY_MAX_ORDER
        let top_index = Self::order_index(BIG_BUDDY_MAX_ORDER);
        let mut offset = 0;
        while offset < normalized_size {
            let block = FreeBlock::new(base + offset, 0, BUDDY_TRIM_NOT_ALLOCATED);
            (*block).next = (*region_ptr).free[top_index];
            (*region_ptr).free[top_index] = block;
            offset += BIG_BUDDY_MAX_BLOCK_SIZE;
        }
        Self::mark_order_nonempty(region_ptr, BIG_BUDDY_MAX_ORDER);

        {
            let _guard = self.spin.lock();
            (*region_ptr).next = self.regions;
            self.regions = region_ptr;
        }

        if init {
            BUDDY_INIT = true;
        }

        true
    }

    #[inline(always)]
    unsafe fn regions_head(&self) -> *mut BuddyRegion {
        // Buddy regions are append-only after publication. Wait only for a writer
        // publishing a new head; traversal is safe while regions are never removed.
        self.spin.spin_until_unlock();
        self.regions
    }

    #[inline(always)]
    unsafe fn find_region(&self, addr: usize) -> *mut BuddyRegion {
        let mut region = self.regions_head();
        while !region.is_null() {
            let start = (*region).base;
            let end = start + (*region).total_size;
            if addr >= start && addr < end {
                return region;
            }
            region = (*region).next;
        }
        null_mut()
    }

    #[inline(always)]
    unsafe fn buddy(region: *mut BuddyRegion, addr: usize, order: usize) -> usize {
        let rel = addr - (*region).base;
        (*region).base + (rel ^ (1 << order))
    }

    #[inline(always)]
    unsafe fn alloc_from_region(
        region: *mut BuddyRegion,
        requested_order: usize,
    ) -> Option<(usize, u8)> {
        if requested_order > (*region).order {
            return None;
        }

        let requested_index = Self::order_index(requested_order);
        let mask = (*region).nonempty_mask.load(Ordering::Acquire) & (!0u8 << requested_index);
        if mask == 0 {
            return None;
        }

        let mut alloc_order = requested_order;
        while alloc_order <= (*region).order {
            let order_bit = Self::order_bit(alloc_order);
            if mask & order_bit == 0 {
                alloc_order += 1;
                continue;
            }

            let order_index = Self::order_index(alloc_order);
            let _guard = (*region).order_locks[order_index].lock();
            let head = &mut (*region).free[order_index];
            if (*head).is_null() {
                Self::mark_order_empty_if_needed(region, alloc_order);
                alloc_order += 1;
                continue;
            }

            let block = (*head as *mut FreeBlock).as_mut().unwrap();
            let block_addr = block as *mut FreeBlock as usize;
            let block_life_time = block.life_time;
            let block_trim_state = block.trim_state;
            *head = block.next;
            Self::mark_order_empty_if_needed(region, alloc_order);
            drop(_guard);

            while alloc_order > requested_order {
                alloc_order -= 1;
                let block_size = 1 << alloc_order;
                let buddy_addr = block_addr + block_size;

                let buddy = FreeBlock::new(buddy_addr, block_life_time, block_trim_state);
                let order_index = Self::order_index(alloc_order);
                let _guard = (*region).order_locks[order_index].lock();
                let head_ptr = &mut (*region).free[order_index];
                let buddy_block = buddy.as_mut().unwrap();
                buddy_block.next = *head_ptr;
                *head_ptr = buddy;
                Self::mark_order_nonempty(region, alloc_order);
            }

            return Some((block_addr, block_trim_state));
        }

        None
    }

    pub unsafe fn init(&mut self, size: usize, thp: bool) {
        let this = self as *mut BuddyAllocator;
        self.once.call_once(|| unsafe {
            let page = &mut *this;
            page.thp = thp;
            let normalized_size = Self::normalize_region_size(size);

            let (_, inner) = SLAB_CACHE.get_numa_and_inner();
            let current_cpu = sched_getcpu();
            let node_id = SLAB_CACHE.node_for_cpu(current_cpu as usize, inner);

            page.grow_order = Self::order_for_size(normalized_size).min(BIG_BUDDY_MAX_ORDER);
            page.add_region(normalized_size, node_id, true, inner.is_numa);
        });
    }

    #[inline(always)]
    pub fn is_in_pool(&self, addr: usize) -> bool {
        unsafe { !self.find_region(addr).is_null() }
    }

    unsafe fn alloc_from_node(
        &self,
        requested_order: usize,
        node_id: u16,
    ) -> Option<(usize, usize, u8)> {
        let mut region = self.regions_head();

        while !region.is_null() {
            let next = (*region).next;

            if (*region).node_id == node_id {
                if let Some((addr, flag)) = Self::alloc_from_region(region, requested_order) {
                    return Some((addr, requested_order, flag));
                }
            }

            region = next;
        }

        None
    }

    pub unsafe fn alloc(
        &mut self,
        size: usize,
        node_id: u16,
        numa_inner: (&NumaTopology, &SlabCacheInner),
    ) -> Option<(usize, usize, u8)> {
        let requested_order = Self::order_for_size(size).max(BIG_BUDDY_MIN_ORDER);
        if requested_order > BIG_BUDDY_MAX_ORDER {
            return None;
        }

        if let Some(block) = self.alloc_from_node(requested_order, node_id) {
            return Some(block);
        }

        let expand_order = self
            .grow_order
            .max(requested_order)
            .min(BIG_BUDDY_MAX_ORDER);

        if self.add_region(1 << expand_order, node_id, false, numa_inner.1.is_numa) {
            if let Some(block) = self.alloc_from_node(requested_order, node_id) {
                return Some(block);
            }
        }

        let (numa, inner) = numa_inner;
        if inner.is_numa {
            for i in 0..numa.nranges {
                let node_id = (i + node_id as usize) % numa.nranges;

                if let Some(block) = self.alloc_from_node(requested_order, node_id as u16) {
                    return Some(block);
                }
            }

            return None;
        }

        None
    }

    pub unsafe fn free(&mut self, addr: usize, order: usize) {
        if order < BIG_BUDDY_MIN_ORDER || order > BIG_BUDDY_MAX_ORDER {
            return;
        }

        let region = self.find_region(addr);
        if region.is_null() {
            return;
        }
        if order > (*region).order {
            return;
        }

        let mut current = addr;
        let mut current_order = order;

        while current_order < (*region).order {
            let buddy = Self::buddy(region, current, current_order);

            if buddy < (*region).base || buddy >= (*region).base + (*region).total_size {
                break;
            }

            let order_index = Self::order_index(current_order);
            let _guard = (*region).order_locks[order_index].lock();
            let head = &mut (*region).free[order_index];
            let mut prev: *mut FreeBlock = null_mut();
            let mut curr = *head;

            let mut found = false;
            while !curr.is_null() {
                let curr_addr = curr as usize;
                if curr_addr == buddy {
                    found = true;
                    let buddy_block = curr.as_ref().unwrap();
                    if prev.is_null() {
                        *head = buddy_block.next;
                    } else {
                        (*prev).next = buddy_block.next;
                    }
                    Self::mark_order_empty_if_needed(region, current_order);
                    break;
                }
                prev = curr;
                curr = curr.as_ref().unwrap().next;
            }

            if !found {
                break;
            }

            current = current.min(buddy);
            current_order += 1;
        }

        let block = FreeBlock::new(
            current,
            CURRENT_STAMP.load(std::sync::atomic::Ordering::Relaxed),
            BUDDY_TRIM_ALLOCATED,
        );
        let order_index = Self::order_index(current_order);
        let _guard = (*region).order_locks[order_index].lock();
        let head = &mut (*region).free[order_index];
        let block_mut = block.as_mut().unwrap();
        block_mut.next = *head;
        *head = block;
        Self::mark_order_nonempty(region, current_order);
    }

    pub unsafe fn try_grow_inplace(
        &mut self,
        addr: usize,
        current_order: usize,
    ) -> Option<(usize, usize)> {
        if self.regions_head().is_null() {
            return None;
        }

        let region = self.find_region(addr);
        if region.is_null() {
            return None;
        }
        if current_order >= (*region).order {
            return None;
        }

        if addr < (*region).base || addr >= (*region).base + (*region).total_size {
            return None;
        }

        let next_order = current_order + 1;
        let buddy_addr = Self::buddy(region, addr, current_order);

        if buddy_addr < addr {
            return None;
        }

        if buddy_addr < (*region).base || buddy_addr >= (*region).base + (*region).total_size {
            return None;
        }

        let order_index = Self::order_index(current_order);
        let _guard = (*region).order_locks[order_index].lock();
        let head = &mut (*region).free[order_index];
        let mut prev: *mut FreeBlock = null_mut();
        let mut curr = *head;

        while !curr.is_null() {
            let curr_addr = curr as usize;
            if curr_addr == buddy_addr {
                let buddy_block = curr.as_ref().unwrap();
                if prev.is_null() {
                    *head = buddy_block.next;
                } else {
                    (*prev).next = buddy_block.next;
                }
                Self::mark_order_empty_if_needed(region, current_order);

                return Some((addr, next_order));
            }
            prev = curr;
            curr = curr.as_ref().unwrap().next;
        }

        None
    }

    pub unsafe fn trim(&mut self, requested_size: usize) -> usize {
        self.trim_inner(requested_size, true)
    }

    pub unsafe fn trim_old(&mut self, requested_size: usize) -> usize {
        self.trim_inner(requested_size, false)
    }

    #[cfg(feature = "debug")]
    pub unsafe fn report(&self) -> BuddyBackendReport {
        let mut report = BuddyBackendReport {
            regions: 0,
            total_region_bytes: 0,
            free_bytes: 0,
            free_blocks: [0; BUDDY_NUM_ORDERS],
            never_allocated_blocks: 0,
            reused_blocks: 0,
            trimmed_blocks: 0,
            never_allocated_by_order: [0; BUDDY_NUM_ORDERS],
            reused_by_order: [0; BUDDY_NUM_ORDERS],
            trimmed_by_order: [0; BUDDY_NUM_ORDERS],
            grow_order: self.grow_order,
            thp: self.thp,
        };

        let mut region = self.regions_head();
        while !region.is_null() {
            report.regions += 1;
            report.total_region_bytes = report
                .total_region_bytes
                .saturating_add((*region).total_size);

            let mut order = BIG_BUDDY_MIN_ORDER;
            while order <= (*region).order {
                let index = Self::order_index(order);
                let _guard = (*region).order_locks[index].lock();
                let block_size = 1usize << order;
                let mut curr = (*region).free[index];

                while !curr.is_null() {
                    report.free_blocks[index] += 1;
                    report.free_bytes = report.free_bytes.saturating_add(block_size);

                    match (*curr).trim_state {
                        BUDDY_TRIM_NOT_ALLOCATED => {
                            report.never_allocated_blocks += 1;
                            report.never_allocated_by_order[index] += 1;
                        }
                        BUDDY_TRIM_TRIMMED => {
                            report.trimmed_blocks += 1;
                            report.trimmed_by_order[index] += 1;
                        }
                        _ => {
                            report.reused_blocks += 1;
                            report.reused_by_order[index] += 1;
                        }
                    }

                    curr = (*curr).next;
                }

                order += 1;
            }

            region = (*region).next;
        }

        report
    }

    #[cfg(feature = "preload")]
    pub unsafe fn reset_locks_on_fork(&self) {
        self.spin.reset_at_fork();

        let mut region = self.regions;
        while !region.is_null() {
            let mut index = 0;
            while index < NUM_ORDERS {
                (*region).order_locks[index].reset_at_fork();
                index += 1;
            }
            region = (*region).next;
        }
    }

    unsafe fn trim_inner(&mut self, requested_size: usize, force_trim: bool) -> usize {
        let Some(_global_trim_guard) = GLOBAL_TRIM_LOCK.try_lock() else {
            return 0;
        };

        #[cfg(feature = "debug")]
        TOTAL_TRIM_CALLS.fetch_add(1, Ordering::Relaxed);

        let mut trimmed = 0usize;
        let mut avg: u32 = 0;
        let mut total = 0u32;
        let stamp = CURRENT_STAMP.load(std::sync::atomic::Ordering::Relaxed);
        let avg_life = BUDDY_AVERAGE_BLOCK_TIMES.load(std::sync::atomic::Ordering::Relaxed);

        let mut region = self.regions_head();
        while !region.is_null() {
            let next_region = (*region).next;
            {
                let mut order = BIG_BUDDY_MIN_ORDER;
                while order <= (*region).order {
                    let order_index = Self::order_index(order);
                    let _order_guard = (*region).order_locks[order_index].lock();
                    let block_size = 1 << order;
                    let mut curr = (*region).free[order_index];

                    while !curr.is_null() {
                        let next_block = (*curr).next;
                        if (*curr).trim_state == BUDDY_TRIM_NOT_ALLOCATED
                            || (*curr).trim_state == BUDDY_TRIM_TRIMMED
                        {
                            curr = next_block;
                            continue;
                        }

                        let life_time = (*curr).life_time;
                        let age = stamp.saturating_sub(life_time);

                        avg = avg.saturating_add(age);
                        total = total.saturating_add(1);

                        if (force_trim || age > avg_life) && block_size > PAGE_SIZE {
                            let trim_addr = (curr as usize) + PAGE_SIZE;
                            let trim_size = block_size - PAGE_SIZE;

                            #[cfg(feature = "lazy-page-trim")]
                            let advice = Advice::LinuxFree;

                            #[cfg(not(feature = "lazy-page-trim"))]
                            let advice = Advice::LinuxDontNeed;

                            if madvise(trim_addr as *mut c_void, trim_size, advice).is_ok() {
                                #[cfg(feature = "debug")]
                                TOTAL_TRIMMED_VA.fetch_add(block_size, Ordering::Relaxed);
                                (*curr).trim_state = BUDDY_TRIM_TRIMMED;
                                trimmed = trimmed.saturating_add(trim_size);
                            }

                            (*curr).life_time = stamp;
                        }

                        if force_trim && requested_size != 0 && trimmed >= requested_size {
                            if total > 0 {
                                let avg = (avg / total).clamp(1000, 60000);
                                BUDDY_AVERAGE_BLOCK_TIMES
                                    .store(avg, std::sync::atomic::Ordering::Relaxed);
                            }
                            return trimmed;
                        }
                        curr = next_block;
                    }

                    order += 1;
                }
            }

            region = next_region;
        }

        if total > 0 {
            let avg = (avg / total).clamp(1000, 60000);
            BUDDY_AVERAGE_BLOCK_TIMES.store(avg, std::sync::atomic::Ordering::Relaxed);
        }

        trimmed
    }
}

pub static mut BUDDY_BACKEND: BuddyAllocator = BuddyAllocator::new();

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn region_size_preserves_supported_values_above_max_order() {
        assert_eq!(
            BuddyAllocator::normalize_region_size(256 * 1024 * 1024),
            256 * 1024 * 1024
        );
        assert_eq!(
            BuddyAllocator::normalize_region_size(65 * 1024 * 1024),
            128 * 1024 * 1024
        );
    }

    #[test]
    fn region_size_keeps_a_max_order_minimum_and_handles_overflow() {
        assert_eq!(
            BuddyAllocator::normalize_region_size(1),
            BIG_BUDDY_MAX_BLOCK_SIZE
        );
        assert_eq!(
            BuddyAllocator::normalize_region_size(usize::MAX),
            BIG_BUDDY_MAX_BLOCK_SIZE
        );
    }
}
