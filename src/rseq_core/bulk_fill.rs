use std::ptr::{null_mut, write};

use rustix::mm::{MapFlags, ProtFlags, mmap_anonymous};

use crate::{
    Err, FREED_MAGIC, Header, MetaData,
    internals::l3_main_radix::L3_RADIX,
    rseq_core::{bulk_core::FreeList, rseq_cache::RSEQ_CACHE, rseq_main::get_rseq},
    utility::{ITERATIONS, SIZE_CLASSES, align_to},
};

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
                _padding: 0,
            },
        );

        if head.is_null() {
            tail = current_header;
        }
        head = current_header;
    }

    (*metadata).next = base + (count * block_size);

    (head, tail, count)
}

// TODO: Wire up time stamping
pub unsafe fn bulk_fill(
    thread: &mut FreeList,
    class: usize,
    max_init: usize,
) -> Result<(*mut Header, *mut Header, usize), Err> {
    let payload_size = SIZE_CLASSES[class];
    let block_size = align_to(payload_size + Header::SIZE, 16);

    let current_stamp = 0;

    let pending = thread.free[class];
    if !pending.is_null() {
        let (head, tail, count) =
            init_blocks(class as u8, pending, block_size, max_init, current_stamp);
        if count > 0 {
            if remaining_blocks(pending, block_size) == 0 {
                thread.free[class] = null_mut();
            }
            return Ok((head, tail, count));
        }
        thread.free[class] = null_mut();
    }

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

    let (head, tail, count) =
        init_blocks(class as u8, metadata, block_size, max_init, current_stamp);
    if count == 0 {
        return Err(Err::OutOfMemory);
    }
    if remaining_blocks(metadata, block_size) > 0 {
        thread.free[class] = metadata;
    }

    Ok((head, tail, count))
}

pub unsafe fn drain_pending(thread: &mut FreeList, class: usize) {
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
            RSEQ_CACHE.mail_push_batch(class, head, tail, count, get_rseq().cpu_id as usize);
        }
    }

    thread.free[class] = null_mut();
}
