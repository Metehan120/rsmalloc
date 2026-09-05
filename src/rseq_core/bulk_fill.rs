// After wrestling with TLS destructor for hours I gave up and use libc.
//
// Leave it, good enough doesnt effect main paths just destructor

use std::{
    cell::UnsafeCell,
    os::raw::c_void,
    ptr::{null_mut, write},
    sync::atomic::Ordering,
};

use crate::{CURRENT_STAMP, Flags, backend::page_allocator::PAGE_ALLOCATOR};
use crate::{
    FREED_MAGIC, Header, MetaData, add_slab_cached_va,
    internals::radix_tree::RADIX,
    utility::{ITERATIONS, NUM_SIZE_CLASSES, SIZE_CLASSES},
};

use crate::{
    rseq_core::{pending_queue::PENDING_QUEUE, slab_cache::SLAB_CACHE},
    utility::Alignment,
};

pub(crate) enum Err {
    OutOfMemory,
}

pub struct Destructor(*mut ThreadBulk);

impl Drop for Destructor {
    fn drop(&mut self) {
        unsafe { cleanup_thread_bulk(self.0) };
    }
}

struct ThreadBulk {
    free: [*mut MetaData; NUM_SIZE_CLASSES],
    init: bool,
}

impl ThreadBulk {
    const fn new() -> Self {
        Self {
            free: [const { null_mut() }; NUM_SIZE_CLASSES],
            init: false,
        }
    }

    pub unsafe fn get_or_init(&mut self, class: usize) -> *mut MetaData {
        if !self.init {
            self.init = true;
            touch_tls();
        }

        self.free[class]
    }
}

#[thread_local]
static mut THREAD_BULK: ThreadBulk = ThreadBulk::new();
#[thread_local]
static TLS_DESTRUCTOR: UnsafeCell<Option<Destructor>> = UnsafeCell::new(None);

unsafe extern "C" {
    static __dso_handle: u8;

    fn __cxa_thread_atexit_impl(
        destructor: unsafe extern "C" fn(*mut c_void),
        object: *mut c_void,
        dso_symbol: *mut c_void,
    ) -> i32;
}

unsafe extern "C" fn run_tls_destructor(slot: *mut c_void) {
    core::ptr::drop_in_place(slot as *mut Option<Destructor>);
}

#[inline(always)]
unsafe fn touch_tls() {
    let slot = TLS_DESTRUCTOR.get();
    core::ptr::write_volatile(slot, Some(Destructor(&raw mut THREAD_BULK)));
    core::ptr::read_volatile(slot);

    let dso = &raw const __dso_handle as *mut c_void;
    if __cxa_thread_atexit_impl(run_tls_destructor, slot as *mut c_void, dso) != 0 {
        THREAD_BULK.init = false;
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

    let pending = PENDING_QUEUE.pop(node_id, class);
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
        .alloc(node_id, total)
        .ok_or(Err::OutOfMemory)?;

    add_slab_cached_va(total);

    RADIX.set_range(mem as usize, total, true);

    let metadata = mem as *mut MetaData;
    write(
        metadata,
        MetaData {
            next_page: null_mut(),
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

    let pending = THREAD_BULK.get_or_init(class);
    if !pending.is_null() {
        THREAD_BULK.free[class] = null_mut();
        let (head, tail, count) =
            init_blocks(class as u8, pending, block_size, max_init, current_stamp);
        if count > 0 {
            if remaining_blocks(pending, block_size) > 0 {
                THREAD_BULK.free[class] = pending;
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
    if remaining_blocks(metadata, block_size) > 0 {
        THREAD_BULK.free[class] = metadata;
    }

    Ok((head, tail, count))
}

unsafe fn cleanup_thread_bulk(thread: *mut ThreadBulk) {
    if thread.is_null() {
        return;
    }

    for class in 0..NUM_SIZE_CLASSES {
        drain_pending(&mut *thread, class);
    }
}

unsafe fn drain_pending(thread: &mut ThreadBulk, class: usize) {
    let pending = thread.free[class];
    if pending.is_null() {
        return;
    }

    thread.free[class] = null_mut();
    PENDING_QUEUE.insert(class, pending);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[repr(C, align(4096))]
    struct TestMeta(MetaData);

    static mut TEST_META: TestMeta = TestMeta(MetaData {
        next_page: null_mut(),
        start: 0,
        end: 4096,
        next: 0,
        node_id: 0,
    });

    #[test]
    fn drains_thread_pending_metadata_on_thread_exit() {
        const CLASS: usize = 0;

        unsafe { PENDING_QUEUE.init(1, false) };
        while !unsafe { PENDING_QUEUE.pop(0, CLASS) }.is_null() {}

        std::thread::spawn(|| unsafe {
            TEST_META.0.next_page = null_mut();
            TEST_META.0.node_id = 0;

            let pending = THREAD_BULK.get_or_init(CLASS);
            assert!(pending.is_null());
            THREAD_BULK.free[CLASS] = &raw mut TEST_META.0;
        })
        .join()
        .unwrap();

        let drained = unsafe { PENDING_QUEUE.pop(0, CLASS) };
        let expected = unsafe { &raw mut TEST_META.0 };
        assert_eq!(drained, expected);
    }
}
