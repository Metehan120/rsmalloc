#[cfg(all(
    not(feature = "cpu-refill-paths"),
    not(feature = "disable-thread-pending")
))]
use std::cell::UnsafeCell;
use std::{
    ptr::{null_mut, write},
    sync::atomic::Ordering,
};

use rustix::mm::{MapFlags, ProtFlags, mmap_anonymous};

#[cfg(all(
    not(feature = "cpu-refill-paths"),
    not(feature = "disable-thread-pending")
))]
use crate::GenericCache;
#[cfg(any(
    feature = "cpu-refill-paths",
    all(
        not(feature = "cpu-refill-paths"),
        not(feature = "disable-thread-pending")
    )
))]
use crate::rseq_core::rseq_cache::RSEQ_CACHE;

#[cfg(not(feature = "cpu-refill-paths"))]
use crate::utility::NUM_SIZE_CLASSES;
use crate::{
    Err, FREED_MAGIC, Header, MetaData, TOTAL_CACHED_VA,
    internals::l3_main_radix::L3_RADIX,
    utility::{ITERATIONS, SIZE_CLASSES, align_to},
};

#[cfg(all(
    not(feature = "cpu-refill-paths"),
    not(feature = "disable-thread-pending")
))]
pub struct Destructor;

#[cfg(all(
    not(feature = "cpu-refill-paths"),
    not(feature = "disable-thread-pending")
))]
impl Drop for Destructor {
    fn drop(&mut self) {
        for i in 0..NUM_SIZE_CLASSES {
            unsafe { drain_pending(&mut THREAD_BULK, i) };
        }
    }
}

#[cfg(not(feature = "cpu-refill-paths"))]
struct ThreadBulk {
    free: [*mut MetaData; NUM_SIZE_CLASSES],
    #[cfg(all(
        not(feature = "cpu-refill-paths"),
        not(feature = "disable-thread-pending")
    ))]
    destructor: UnsafeCell<Option<Destructor>>,
    #[cfg(all(
        not(feature = "cpu-refill-paths"),
        not(feature = "disable-thread-pending")
    ))]
    init: bool,
}

#[cfg(not(feature = "cpu-refill-paths"))]
impl ThreadBulk {
    const fn new() -> Self {
        Self {
            free: [const { null_mut() }; NUM_SIZE_CLASSES],
            #[cfg(all(
                not(feature = "cpu-refill-paths"),
                not(feature = "disable-thread-pending")
            ))]
            destructor: UnsafeCell::new(None),
            #[cfg(all(
                not(feature = "cpu-refill-paths"),
                not(feature = "disable-thread-pending")
            ))]
            init: false,
        }
    }
}

#[cfg(not(feature = "cpu-refill-paths"))]
#[thread_local]
static mut THREAD_BULK: ThreadBulk = ThreadBulk::new();

#[cfg(all(
    not(feature = "cpu-refill-paths"),
    not(feature = "disable-thread-pending")
))]
#[inline(always)]
unsafe fn touch_tls() {
    if !THREAD_BULK.init {
        let slot = THREAD_BULK.destructor.get();
        core::ptr::write_volatile(slot, Some(Destructor));
        THREAD_BULK.init = true;
    }
}

#[inline(always)]
unsafe fn remaining_blocks(metadata: *mut MetaData, block_size: usize) -> usize {
    let remaining_bytes = (*metadata).end.saturating_sub((*metadata).next);
    remaining_bytes / block_size
}

#[allow(unsafe_op_in_unsafe_fn)]
unsafe fn init_blocks(
    class: u8,
    metadata: *mut MetaData,
    block_size: usize,
    max_blocks: usize,
    current_stamp: u32,
) -> (*mut Header, *mut Header, usize) {
    let remaining = remaining_blocks(metadata, block_size);
    if remaining == 0 {
        return (null_mut(), null_mut(), 0);
    }

    let count = remaining.min(max_blocks);
    let base = (*metadata).next;
    let mut head = null_mut();
    let mut tail = null_mut();

    for i in (0..count).rev() {
        let current_header = (base + i * block_size) as *mut Header;

        write(
            current_header,
            Header {
                next: head,
                class,
                magic: FREED_MAGIC,
                life_time: current_stamp,
                canary: 0,
            },
        );

        (*current_header).compute_canary(current_header);

        if head.is_null() {
            tail = current_header;
        }
        head = current_header;
    }

    (*metadata).next = base + (count * block_size);

    (head, tail, count)
}

unsafe fn alloc_metadata(class: usize, block_size: usize) -> Result<*mut MetaData, Err> {
    let mut num_blocks = ITERATIONS[class];
    let mut total = size_of::<MetaData>() + (block_size * num_blocks);

    let pages = (total + 4095) / 4096;
    let available_bytes = pages * 4096 - size_of::<MetaData>();
    let max_blocks_in_pages = available_bytes / block_size;

    if max_blocks_in_pages > num_blocks {
        num_blocks = max_blocks_in_pages;
        total = size_of::<MetaData>() + (block_size * num_blocks);
    }

    let mem = mmap_anonymous(
        null_mut(),
        total,
        ProtFlags::READ | ProtFlags::WRITE,
        MapFlags::PRIVATE,
    )
    .map_err(|_| Err::OutOfMemory)?;

    TOTAL_CACHED_VA.fetch_add(total, Ordering::Relaxed);

    L3_RADIX.set_range(mem as usize, total, true);

    let metadata = mem as *mut MetaData;
    write(
        metadata,
        MetaData {
            start: mem as usize,
            end: (mem as usize) + total,
            next: (mem as usize) + size_of::<MetaData>(),
        },
    );

    Ok(metadata)
}

// TODO: Wire up time stamping
#[cfg(feature = "cpu-refill-paths")]
pub unsafe fn bulk_fill(
    class: usize,
    cpu_id: usize,
    max_init: usize,
) -> Result<(*mut Header, *mut Header, usize), Err> {
    let payload_size = SIZE_CLASSES[class];
    let block_size = align_to(payload_size + Header::SIZE, 16);
    let current_stamp = 0;

    let global = RSEQ_CACHE.get_bulk_fill(class, cpu_id);
    let _guard = global.lock().lock();

    let pending = global.get_metadata();
    if !pending.is_null() {
        let (head, tail, count) =
            init_blocks(class as u8, pending, block_size, max_init, current_stamp);
        if count > 0 {
            if remaining_blocks(pending, block_size) == 0 {
                global.set_metadata(null_mut());
            }
            return Ok((head, tail, count));
        }
        global.set_metadata(null_mut());
    }

    let metadata = alloc_metadata(class, block_size)?;
    let (head, tail, count) =
        init_blocks(class as u8, metadata, block_size, max_init, current_stamp);
    if count == 0 {
        return Err(Err::OutOfMemory);
    }
    if remaining_blocks(metadata, block_size) > 0 {
        global.set_metadata(metadata);
    }

    Ok((head, tail, count))
}

// TODO: Wire up time stamping
#[cfg(not(feature = "cpu-refill-paths"))]
pub unsafe fn bulk_fill(
    class: usize,
    cpu_id: usize,
    max_init: usize,
) -> Result<(*mut Header, *mut Header, usize), Err> {
    #[cfg(all(
        not(feature = "cpu-refill-paths"),
        not(feature = "disable-thread-pending")
    ))]
    touch_tls();

    let _ = cpu_id;
    let payload_size = SIZE_CLASSES[class];
    let block_size = align_to(payload_size + Header::SIZE, 16);
    let current_stamp = 0;

    let pending = THREAD_BULK.free[class];
    if !pending.is_null() {
        let (head, tail, count) =
            init_blocks(class as u8, pending, block_size, max_init, current_stamp);
        if count > 0 {
            if remaining_blocks(pending, block_size) == 0 {
                THREAD_BULK.free[class] = null_mut();
            }
            return Ok((head, tail, count));
        }
        THREAD_BULK.free[class] = null_mut();
    }

    let metadata = alloc_metadata(class, block_size)?;
    let (head, tail, count) =
        init_blocks(class as u8, metadata, block_size, max_init, current_stamp);
    if count == 0 {
        return Err(Err::OutOfMemory);
    }
    if remaining_blocks(metadata, block_size) > 0 {
        THREAD_BULK.free[class] = metadata;
    }

    Ok((head, tail, count))
}

#[cfg(all(
    not(feature = "cpu-refill-paths"),
    not(feature = "disable-thread-pending")
))]
unsafe fn drain_pending(thread: &mut ThreadBulk, class: usize) {
    let pending = thread.free[class];
    if pending.is_null() {
        return;
    }

    let payload_size = SIZE_CLASSES[class];
    let block_size = align_to(payload_size + Header::SIZE, 16);
    let current_stamp = 0;
    let remaining = remaining_blocks(pending, block_size);

    if remaining > 0 {
        let (head, tail, count) =
            init_blocks(class as u8, pending, block_size, remaining, current_stamp);
        if count > 0 {
            RSEQ_CACHE.push_tailed(class, head, tail, count);
        }
    }

    thread.free[class] = null_mut();
}
