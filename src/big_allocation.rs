use std::{
    os::raw::c_void,
    ptr::{null_mut, write},
};

use rustix::mm::{Advice, MapFlags, ProtFlags, madvise, mmap_anonymous, munmap};

use crate::{
    BIG_MAGIC, BigAllocMeta, Header, RSMallocError,
    core_prim::wrappers::UnsafePointer,
    internals::{hashmap::BIG_MAP, l3_main_radix::L3_RADIX, lock::SerialLock},
    utility::align_to,
};

#[derive(Copy, Clone)]
struct BigFreeBlock {
    ptr: *mut u8,
    total_size: usize,
}

struct BigFreeCache {
    lock: SerialLock,
    data: [Option<BigFreeBlock>; 16],
}

impl BigFreeCache {
    const fn new() -> Self {
        Self {
            lock: SerialLock::new(),
            data: [None; 16],
        }
    }
}

static mut BIG_FREE_CACHE: BigFreeCache = BigFreeCache::new();
const TWO_MB: usize = 1024 * 1024 * 2;

fn estimate_and_align_2mb(size: usize) -> usize {
    let remainder = size % TWO_MB;

    if remainder > 0 && (TWO_MB - remainder) <= 1024 * 64 {
        align_to(size, TWO_MB)
    } else {
        align_to(size, 4096)
    }
}

#[inline(never)]
pub unsafe fn big_malloc(size: usize) -> UnsafePointer<Header> {
    let aligned_total = estimate_and_align_2mb(size + Header::SIZE);

    let mut registered = false;
    let mut actual_ptr: *mut u8 = null_mut();
    {
        let _guard = BIG_FREE_CACHE.lock.lock();
        let cache = &mut BIG_FREE_CACHE.data;
        for i in 0..cache.len() {
            if let Some(block) = cache[i] {
                if block.total_size >= aligned_total {
                    registered = true;
                    actual_ptr = block.ptr;
                    let block_total = block.total_size;
                    cache[i] = None;

                    if block_total >= aligned_total + 4096 {
                        let remainder_ptr = actual_ptr.add(aligned_total);
                        let remainder_size = block_total - aligned_total;

                        let mut pushed = false;
                        for j in 0..cache.len() {
                            if cache[j].is_none() {
                                cache[j] = Some(BigFreeBlock {
                                    ptr: remainder_ptr,
                                    total_size: remainder_size,
                                });
                                pushed = true;
                                break;
                            }
                        }

                        if !pushed {
                            L3_RADIX.set_range(remainder_ptr as usize, remainder_size, false);
                            let _ = munmap(remainder_ptr as *mut c_void, remainder_size);
                        }
                    }
                    break;
                }
            }
        }
    }

    let mut aligned = false;
    if actual_ptr.is_null() {
        match mmap_anonymous(
            null_mut(),
            aligned_total,
            ProtFlags::READ | ProtFlags::WRITE,
            MapFlags::PRIVATE,
        ) {
            Ok(ptr) => {
                aligned = false;
                actual_ptr = ptr as *mut u8;
            }
            Err(_) => return UnsafePointer::NULL,
        }
    }

    if !aligned && !registered {
        let _ = madvise(
            actual_ptr as *mut c_void,
            aligned_total,
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
            _padding: 0,
        },
    );

    let payload_ptr = actual_ptr.add(Header::SIZE);

    if !registered {
        L3_RADIX.set_range(actual_ptr as usize, aligned_total, true)
    };

    BIG_MAP.insert(
        payload_ptr as usize,
        BigAllocMeta {
            next: null_mut(),
            size,
        },
    );

    UnsafePointer::new(actual_ptr as *mut Header).walk_header()
}

#[inline(never)]
pub unsafe fn big_free(ptr: usize) {
    const BIG_CACHE_LIMIT: usize = 1024 * 1024 * 256;

    let header = BIG_MAP.remove(ptr).unwrap_or_else(|| {
        RSMallocError::MemoryCorruption.log_and_abort(
            null_mut(),
            "missing header for big allocation",
            None,
        )
    });
    let payload_size = header.size;
    let total_size = estimate_and_align_2mb(payload_size + Header::SIZE);
    let mapping_base = (ptr - Header::SIZE) as *mut u8;

    if total_size >= BIG_CACHE_LIMIT {
        L3_RADIX.set_range(mapping_base as usize, total_size, false);
        if munmap(mapping_base as *mut c_void, total_size).is_err() {
            let _ = madvise(
                mapping_base as *mut c_void,
                total_size,
                Advice::LinuxDontNeed,
            );
        }
        return;
    }

    let mut pushed = false;
    {
        let _guard = BIG_FREE_CACHE.lock.lock();
        let cache = &mut BIG_FREE_CACHE.data;

        for i in 0..cache.len() {
            if cache[i].is_none() {
                cache[i] = Some(BigFreeBlock {
                    ptr: mapping_base,
                    total_size,
                });
                pushed = true;
                break;
            }
        }

        if !pushed {
            if let Some(to_trim) = cache[0] {
                L3_RADIX.set_range(to_trim.ptr as usize, to_trim.total_size, false);
                if munmap(to_trim.ptr as *mut c_void, to_trim.total_size).is_err() {
                    let _ = madvise(
                        to_trim.ptr as *mut c_void,
                        to_trim.total_size,
                        Advice::LinuxDontNeed,
                    );
                }
            }

            for i in 0..cache.len() - 1 {
                cache[i] = cache[i + 1];
            }
            cache[cache.len() - 1] = Some(BigFreeBlock {
                ptr: mapping_base,
                total_size,
            });
        }
    }
}

pub unsafe fn trim_big_allocations() -> usize {
    let _guard = BIG_FREE_CACHE.lock.lock();
    let cache = &mut BIG_FREE_CACHE.data;
    let mut freed = 0;
    for i in 0..cache.len() {
        if let Some(block) = cache[i] {
            L3_RADIX.set_range(block.ptr as usize, block.total_size, false);
            if munmap(block.ptr as *mut c_void, block.total_size).is_err() {
                let _ = madvise(
                    block.ptr as *mut c_void,
                    block.total_size,
                    Advice::LinuxDontNeed,
                );
            }
            freed += block.total_size;
            cache[i] = None;
        }
    }
    freed
}
