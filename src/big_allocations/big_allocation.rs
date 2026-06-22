use std::{
    os::raw::c_void,
    ptr::{null_mut, write},
};

use rustix::mm::{Advice, MapFlags, ProtFlags, madvise, mmap_anonymous, munmap};

use crate::{
    BIG_MAGIC, BUDDY_INIT, BigAllocMeta, Header, RS_DISABLE_THP, RSMallocError,
    big_allocations::buddy::BIG_BUDDY_ALLOCATOR,
    core_prim::wrappers::UnsafePointer,
    internals::{hashmap::BIG_MAP, l3_main_radix::L3_RADIX},
    utility::align_to,
};

const TWO_MB: usize = 1024 * 1024 * 2;

pub unsafe fn estimate_and_align_2mb(size: usize) -> usize {
    let remainder = size % TWO_MB;

    if remainder > 0 && (TWO_MB - remainder) <= 1024 * 64 && !RS_DISABLE_THP {
        align_to(size, TWO_MB)
    } else {
        align_to(size, 4096)
    }
}

#[cold]
#[inline(never)]
pub unsafe fn big_malloc(size: usize, aligned: bool) -> UnsafePointer<Header> {
    let Some(requested_total) = size.checked_add(Header::SIZE) else {
        return UnsafePointer::NULL;
    };
    let aligned_total = estimate_and_align_2mb(requested_total);
    let mut registered = false;
    let mut mapped_total = aligned_total;
    let mut actual_ptr: *mut u8 = null_mut();

    if size <= 1024 * 1024 * 64 && BUDDY_INIT {
        let buddy = BIG_BUDDY_ALLOCATOR.alloc(aligned_total);

        if let Some((addr, order)) = buddy {
            actual_ptr = addr as *mut u8;
            registered = true;
            mapped_total = 1 << order;
        }
    }

    if actual_ptr.is_null() {
        if let Ok(pointer) = mmap_anonymous(
            null_mut(),
            mapped_total,
            ProtFlags::READ | ProtFlags::WRITE,
            MapFlags::PRIVATE,
        ) {
            actual_ptr = pointer as *mut u8;
        } else {
            return UnsafePointer::NULL;
        }
    }

    if !registered && mapped_total.is_multiple_of(TWO_MB) && !RS_DISABLE_THP {
        let _ = madvise(
            actual_ptr as *mut c_void,
            mapped_total,
            Advice::LinuxHugepage,
        );
    }

    write(
        actual_ptr as *mut Header,
        Header {
            next: null_mut(),
            class: 100,
            magic: BIG_MAGIC,
            life_time: 0,
            canary: 0,
        },
    );

    let header = actual_ptr as *mut Header;
    (*header).compute_canary(header);

    let payload_ptr = actual_ptr.add(Header::SIZE);

    if registered {
    } else if !registered && !aligned {
        L3_RADIX.set_single_big(actual_ptr as usize, true)
    } else {
        L3_RADIX.set_range(actual_ptr as usize, mapped_total, true)
    };

    BIG_MAP.insert(
        payload_ptr as usize,
        BigAllocMeta {
            next: null_mut(),
            size,
            order: mapped_total.next_power_of_two().trailing_zeros() as usize,
            aligned,
        },
    );

    UnsafePointer::new(actual_ptr as *mut Header).walk_header()
}

#[inline(never)]
pub unsafe fn big_free(ptr: usize) {
    let header = BIG_MAP.remove(ptr).unwrap_or_else(|| {
        RSMallocError::MemoryCorruption.log_and_abort(
            null_mut(),
            "missing header for big allocation",
            None,
        )
    });
    let mapping_base = (ptr - Header::SIZE) as *mut u8;
    let payload_size = estimate_and_align_2mb(header.size + Header::SIZE);

    if BUDDY_INIT {
        if BIG_BUDDY_ALLOCATOR.is_in_pool(mapping_base as usize) {
            BIG_BUDDY_ALLOCATOR.free(mapping_base as usize, header.order);
            return;
        }
    }

    if header.aligned {
        L3_RADIX.set_range(mapping_base as usize, payload_size, false);
    } else {
        L3_RADIX.set_single_big(mapping_base as usize, false);
    }

    let _ = munmap(mapping_base as *mut c_void, payload_size);
}
