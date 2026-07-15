use std::{
    os::raw::c_void,
    ptr::{null_mut, write},
    sync::atomic::Ordering::Relaxed,
};

use rustix::mm::{Advice, MapFlags, ProtFlags, madvise, mmap_anonymous, munmap};

use crate::{
    ALLOCATED_FLAG, BIG_MAGIC, BUDDY_INIT, BigAllocMeta, Header, RS_DISABLE_THP, RSMallocError,
    TRIMMED_FLAG, ZERO_FLAG,
    big_allocations::buddy::{BUDDY_BACKEND, BUDDY_TRIM_NOT_ALLOCATED, BUDDY_TRIM_TRIMMED},
    core_prim::wrappers::UnsafePointer,
    internals::{binder::prefer_node, hashmap::BIG_MAP, l3_main_radix::RADIX},
    record_mmap_call,
    rseq_core::{rseq_offsets::get_rseq, slab_cache::SLAB_CACHE},
    trim::DISABLE_BUDDY,
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
    let mut flag = 100;
    let cpu_id = get_rseq().cpu_id as usize;
    let (numa, inner) = SLAB_CACHE.get_numa_and_inner();
    let node_id = SLAB_CACHE.node_for_cpu(cpu_id, inner);

    if size <= 1024 * 1024 * 64 && BUDDY_INIT && !DISABLE_BUDDY.load(Relaxed) {
        let buddy = BUDDY_BACKEND.alloc(aligned_total, node_id, (numa, inner));

        if let Some((addr, order, trim_state)) = buddy {
            actual_ptr = addr as *mut u8;
            registered = true;
            mapped_total = 1 << order;

            flag = match trim_state {
                BUDDY_TRIM_NOT_ALLOCATED => ZERO_FLAG,
                BUDDY_TRIM_TRIMMED => TRIMMED_FLAG,
                _ => ALLOCATED_FLAG,
            };
        }
    }

    if actual_ptr.is_null() {
        record_mmap_call(mapped_total);
        if let Ok(pointer) = mmap_anonymous(
            null_mut(),
            mapped_total,
            ProtFlags::READ | ProtFlags::WRITE,
            MapFlags::PRIVATE,
        ) {
            if inner.is_numa {
                prefer_node(pointer, mapped_total, node_id);
            }

            flag = ZERO_FLAG;
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
            flags: flag,
        },
    );

    let payload_ptr = actual_ptr.add(Header::SIZE);

    if registered {
    } else if !registered && !aligned {
        RADIX.set_single_big(actual_ptr as usize, true)
    } else {
        RADIX.set_range(actual_ptr as usize, mapped_total, true)
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
        if BUDDY_BACKEND.is_in_pool(mapping_base as usize) {
            BUDDY_BACKEND.free(mapping_base as usize, header.order);
            return;
        }
    }

    if header.aligned {
        RADIX.set_range(mapping_base as usize, payload_size, false);
    } else {
        RADIX.set_single_big(mapping_base as usize, false);
    }

    let _ = munmap(mapping_base as *mut c_void, payload_size);
}
