use std::{
    ptr::{null_mut, write},
    sync::atomic::{AtomicPtr, Ordering},
};

use crate::{CURRENT_STAMP, Flags, backend::page_allocator::PAGE_ALLOCATOR};
use crate::{
    FREED_MAGIC, Header, MetaData, add_slab_cached_va,
    internals::radix_tree::RADIX,
    utility::{ITERATIONS, SIZE_CLASSES},
};

use crate::{
    rseq_core::{pending_queue::PENDING_QUEUE, slab_cache::SLAB_CACHE},
    utility::Alignment,
};

pub(crate) enum Err {
    OutOfMemory,
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
                flags: Flags::NotAllocated,
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

unsafe fn alloc_metadata(
    class: usize,
    block_size: usize,
    cpu_id: usize,
) -> Result<*mut MetaData, Err> {
    let inner = SLAB_CACHE.get_inner();
    let node_id = SLAB_CACHE.node_for_cpu(cpu_id, inner);

    let pending = PENDING_QUEUE.pop(node_id, class, cpu_id);
    if !pending.is_null() {
        return Ok(pending);
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

    let mem = PAGE_ALLOCATOR
        .alloc(Some(node_id), total)
        .ok_or(Err::OutOfMemory)?;

    add_slab_cached_va(total);

    RADIX.set(mem as usize, total, true);

    let metadata = mem as *mut MetaData;
    write(
        metadata,
        MetaData {
            next_page: AtomicPtr::new(null_mut()),
            start: mem as usize,
            end: (mem as usize) + total,
            next: (mem as usize) + size_of::<MetaData>(),
            node_id,
        },
    );

    Ok(metadata)
}

// TODO: Wire up time stamping
pub unsafe fn bulk_fill(
    class: usize,
    cpu_id: usize,
    max_init: usize,
) -> Result<(*mut Header, *mut Header, usize), Err> {
    let payload_size = SIZE_CLASSES[class];
    let block_size = (payload_size + Header::SIZE).align_to(16);
    let current_stamp = CURRENT_STAMP.load(Ordering::Relaxed);

    let pending_slot = SLAB_CACHE.pending_refill(cpu_id, class);
    // Removing the pointer from the slot gives this refill exclusive metadata ownership.
    let pending = pending_slot.swap(null_mut(), Ordering::Acquire);
    if !pending.is_null() {
        let (head, tail, count) =
            init_blocks(class as u8, pending, block_size, max_init, current_stamp);
        if count > 0 {
            if remaining_blocks(pending, block_size) > 0
                && pending_slot
                    .compare_exchange(null_mut(), pending, Ordering::Release, Ordering::Relaxed)
                    .is_err()
            {
                PENDING_QUEUE.insert(class, cpu_id, pending);
            }
            return Ok((head, tail, count));
        }
    }

    let metadata = alloc_metadata(class, block_size, cpu_id)?;
    let (head, tail, count) =
        init_blocks(class as u8, metadata, block_size, max_init, current_stamp);
    if count == 0 {
        return Err(Err::OutOfMemory);
    }
    if remaining_blocks(metadata, block_size) > 0
        && pending_slot
            .compare_exchange(null_mut(), metadata, Ordering::Release, Ordering::Relaxed)
            .is_err()
    {
        PENDING_QUEUE.insert(class, cpu_id, metadata);
    }

    Ok((head, tail, count))
}
