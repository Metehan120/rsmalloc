// Audit this file in near future
//
// TODO: Use pointer wrappers

use std::{mem::size_of, os::raw::c_void, ptr::null_mut};

use rustix::mm::{Advice, MapFlags, ProtFlags, madvise, mmap_anonymous};

use crate::{
    BUDDY_INIT, RSMallocError,
    internals::{l3_main_radix::L3_RADIX, lock::SerialLock, once::Once},
    utility::align_to,
};

pub const BIG_BUDDY_MIN_ORDER: usize = 22; // 4 MiB
pub const BIG_BUDDY_MAX_ORDER: usize = 26; // 64 MiB
pub const BIG_BUDDY_MAX_SIZE: usize = 1 << BIG_BUDDY_MAX_ORDER;
const NUM_ORDERS: usize = BIG_BUDDY_MAX_ORDER - BIG_BUDDY_MIN_ORDER + 1;
const PAGE_SIZE: usize = 4096;

#[repr(C)]
#[derive(Debug, Clone, Copy)]
struct FreeBlock {
    next: *mut FreeBlock,
}

impl FreeBlock {
    unsafe fn new(addr: usize) -> *mut FreeBlock {
        let block = addr as *mut FreeBlock;
        core::ptr::write(block, FreeBlock { next: null_mut() });
        block
    }
}

#[repr(C)]
struct BuddyRegion {
    next: *mut BuddyRegion,
    base: usize,
    total_size: usize,
    order: usize,
    free: [*mut FreeBlock; NUM_ORDERS],
}

impl BuddyRegion {
    const fn empty() -> Self {
        Self {
            next: null_mut(),
            base: 0,
            total_size: 0,
            order: BIG_BUDDY_MIN_ORDER,
            free: [null_mut(); NUM_ORDERS],
        }
    }
}

pub struct BuddyAllocator {
    regions: *mut BuddyRegion,
    grow_order: usize,
    thp: bool,
    spin: SerialLock,
    trim_lock: SerialLock,
    once: Once,
}

impl BuddyAllocator {
    pub const fn new() -> Self {
        Self {
            regions: null_mut(),
            grow_order: BIG_BUDDY_MIN_ORDER,
            thp: false,
            spin: SerialLock::new(),
            trim_lock: SerialLock::new(),
            once: Once::new(),
        }
    }

    #[inline(always)]
    fn order_for_size(size: usize) -> usize {
        size.max(1).next_power_of_two().trailing_zeros() as usize
    }

    #[inline(always)]
    fn align_to_page(size: usize) -> usize {
        align_to(size, PAGE_SIZE)
    }

    unsafe fn alloc_region_node() -> *mut BuddyRegion {
        let node_size = Self::align_to_page(size_of::<BuddyRegion>());
        mmap_anonymous(
            null_mut(),
            node_size,
            ProtFlags::READ | ProtFlags::WRITE,
            MapFlags::PRIVATE,
        )
        .unwrap_or_else(|no| {
            RSMallocError::VAIinitFailed.log_and_abort(
                null_mut(),
                "cannot allocate buddy region metadata",
                Some(no.raw_os_error()),
            )
        }) as *mut BuddyRegion
    }

    unsafe fn add_region(&mut self, size: usize, init: bool) {
        let normalized_size = size
            .next_power_of_two()
            .clamp(1 << BIG_BUDDY_MIN_ORDER, 1 << BIG_BUDDY_MAX_ORDER);

        let base = match mmap_anonymous(
            null_mut(),
            normalized_size,
            ProtFlags::READ | ProtFlags::WRITE,
            MapFlags::PRIVATE,
        ) {
            Ok(ptr) => ptr,
            Err(no) => {
                if init {
                    BUDDY_INIT = false;
                    return;
                }
                RSMallocError::VAIinitFailed.log_and_abort(
                    null_mut(),
                    "cannot expand buddy allocator",
                    Some(no.raw_os_error()),
                )
            }
        } as usize;

        if self.thp {
            let _ = madvise(base as *mut c_void, normalized_size, Advice::LinuxHugepage);
        }

        L3_RADIX.set_range(base, normalized_size, true);

        let region_ptr = Self::alloc_region_node();
        core::ptr::write(region_ptr, BuddyRegion::empty());

        let top_order = Self::order_for_size(normalized_size);
        (*region_ptr).base = base;
        (*region_ptr).total_size = normalized_size;
        (*region_ptr).order = top_order;

        let block = FreeBlock::new(base);
        (*region_ptr).free[top_order - BIG_BUDDY_MIN_ORDER] = block;

        (*region_ptr).next = self.regions;
        self.regions = region_ptr;

        if init {
            BUDDY_INIT = true;
        }
    }

    #[inline(always)]
    unsafe fn find_region(&self, addr: usize) -> *mut BuddyRegion {
        let mut region = self.regions;
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
    unsafe fn alloc_from_region(region: *mut BuddyRegion, requested_order: usize) -> Option<usize> {
        if requested_order > (*region).order {
            return None;
        }

        let mut alloc_order = requested_order;
        while alloc_order <= (*region).order
            && (*region).free[alloc_order - BIG_BUDDY_MIN_ORDER] == null_mut()
        {
            alloc_order += 1;
        }

        if alloc_order > (*region).order {
            return None;
        }

        let head = &mut (*region).free[alloc_order - BIG_BUDDY_MIN_ORDER];
        let block = (*head as *mut FreeBlock).as_mut().unwrap();
        let block_addr = block as *mut FreeBlock as usize;
        *head = block.next;

        while alloc_order > requested_order {
            alloc_order -= 1;
            let block_size = 1 << alloc_order;
            let buddy_addr = block_addr + block_size;

            let buddy = FreeBlock::new(buddy_addr);
            let head_ptr = &mut (*region).free[alloc_order - BIG_BUDDY_MIN_ORDER];
            let buddy_block = buddy.as_mut().unwrap();
            buddy_block.next = *head_ptr;
            *head_ptr = buddy;
        }

        Some(block_addr)
    }

    pub unsafe fn init(&mut self, size: usize, thp: bool) {
        let this = self as *mut BuddyAllocator;
        self.once.call_once(|| unsafe {
            let page = &mut *this;
            page.thp = thp;
            let normalized_size = size
                .next_power_of_two()
                .clamp(1 << BIG_BUDDY_MIN_ORDER, 1 << BIG_BUDDY_MAX_ORDER);

            page.grow_order = Self::order_for_size(normalized_size);
            page.add_region(normalized_size, true);
        });
    }

    #[inline(always)]
    pub fn is_in_pool(&self, addr: usize) -> bool {
        unsafe { !self.find_region(addr).is_null() }
    }

    pub unsafe fn alloc(&mut self, size: usize) -> Option<(usize, usize)> {
        if self.trim_lock.get_lock() {
            return None;
        }
        let _guard = self.spin.lock();

        let requested_order = Self::order_for_size(size).max(BIG_BUDDY_MIN_ORDER);
        if requested_order > BIG_BUDDY_MAX_ORDER {
            return None;
        }

        let mut region = self.regions;
        while !region.is_null() {
            if let Some(addr) = Self::alloc_from_region(region, requested_order) {
                return Some((addr, requested_order));
            }
            region = (*region).next;
        }

        let expand_order = self
            .grow_order
            .max(requested_order)
            .min(BIG_BUDDY_MAX_ORDER);

        self.add_region(1 << expand_order, false);

        let region = self.regions;
        if region.is_null() {
            return None;
        }

        Self::alloc_from_region(region, requested_order).map(|addr| (addr, requested_order))
    }

    pub unsafe fn free(&mut self, addr: usize, order: usize) {
        let _guard = self.spin.lock();

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

            let head = &mut (*region).free[current_order - BIG_BUDDY_MIN_ORDER];
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

        let block = FreeBlock::new(current);
        let head = &mut (*region).free[current_order - BIG_BUDDY_MIN_ORDER];
        let block_mut = block.as_mut().unwrap();
        block_mut.next = *head;
        *head = block;
    }

    pub unsafe fn try_grow_inplace(
        &mut self,
        addr: usize,
        current_order: usize,
    ) -> Option<(usize, usize)> {
        if self.trim_lock.get_lock() {
            return None;
        }
        let _guard = self.spin.lock();

        if self.regions.is_null() {
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

        let head = &mut (*region).free[current_order - BIG_BUDDY_MIN_ORDER];
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

                return Some((addr, next_order));
            }
            prev = curr;
            curr = curr.as_ref().unwrap().next;
        }

        None
    }

    /*
     * Just a concept written by AI
    pub unsafe fn trim(&mut self) -> usize {
        let _guard = self.spin.lock();
        let _trim_guard = self.trim_lock.lock();

        let mut trimmed = 0usize;

        let mut region = self.regions;
        while !region.is_null() {
            let mut order = BIG_BUDDY_MIN_ORDER;
            while order <= (*region).top_order {
                let block_size = 1 << order;
                let mut curr = (*region).free_heads[order - BIG_BUDDY_MIN_ORDER];

                while !curr.is_null() {
                    let next_block = (*curr).next;
                    let _ = madvise(curr as *mut c_void, block_size, Advice::DontNeed);
                    trimmed = trimmed.saturating_add(block_size);
                    curr = next_block;
                }

                order += 1;
            }

            region = (*region).next;
        }

        trimmed
    } */
}

pub static mut BIG_BUDDY_ALLOCATOR: BuddyAllocator = BuddyAllocator::new();
