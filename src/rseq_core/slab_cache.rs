#[cfg(feature = "debug")]
use std::sync::atomic::Ordering::Relaxed;

use std::{
    arch::x86_64::{_MM_HINT_T0, _mm_prefetch},
    cell::UnsafeCell,
    hint::{likely, spin_loop},
    ptr::{addr_of, eq, null_mut, read_volatile},
    sync::atomic::{
        AtomicU64, AtomicUsize,
        Ordering::{self},
    },
};

use rustix::mm::{MapFlags, ProtFlags, mmap_anonymous};

#[cfg(feature = "debug")]
use crate::ABORTS;
use crate::{
    GenericCache, Header, NCPU, RSMallocError, RseqCoreTrait,
    core_prim::wrappers::UnsafePointer,
    internals::{
        binder::bind_node,
        numa_parser::{NumaTopology, parse_numa_topology},
        once::Once,
    },
    record_mmap_call,
    rseq_core::{pending_queue::PENDING_QUEUE, rseq_asm::RseqCore, rseq_offsets::get_rseq},
    utility::{CACHE_HIGH_BLOCKS, NUM_SIZE_CLASSES},
};

pub struct RseqCache {
    list: UnsafePointer<Header>,
    usage: AtomicUsize,
}

pub struct TransferCache {
    pub list: AtomicUsize,
    pub trimmed: AtomicUsize,
}

// NOTE: Use 4096-byte alignment to avoid false sharing between cache lines and NUMA node balancing.
#[repr(C, align(4096))]
pub struct MainCache {
    cache: [RseqCache; NUM_SIZE_CLASSES],
    mail: [TransferCache; NUM_SIZE_CLASSES],
}

#[derive(Debug, Clone, Copy)]
pub struct SlabCacheInner {
    cache: *mut MainCache,
    numa: NumaTopology,
    pub is_numa: bool,
    bitmap_words: usize,
    nonempty_bitmap: *mut AtomicU64,
}

pub struct SlabCache {
    inner: UnsafeCell<SlabCacheInner>,
    once: Once,
}

unsafe impl Send for SlabCache {}
unsafe impl Sync for SlabCache {}

// here be dragons: TODO: use it until found a better way
unsafe extern "C" {
    fn get_nprocs_conf() -> i32;
}

unsafe fn get_max_cpu() -> u32 {
    let ncpu = get_nprocs_conf();
    if ncpu > 0 { ncpu as u32 } else { 256 }
}

impl SlabCache {
    pub const fn new() -> Self {
        Self {
            inner: UnsafeCell::new(SlabCacheInner {
                cache: null_mut(),
                numa: NumaTopology {
                    cpu_to_node: null_mut(),
                    ncpu: 0,
                    node_ids: null_mut(),
                    nnodes: 0,
                    cpu_ranges: null_mut(),
                    nranges: 0,
                },
                is_numa: false,
                nonempty_bitmap: null_mut(),
                bitmap_words: 0,
            }),
            once: Once::new(),
        }
    }

    pub unsafe fn ensure_cache(&self) {
        self.once.call_once(|| {
            let cache = self.inner.get();
            let inner = &mut *cache;
            let ncpu = get_max_cpu() as usize;
            let alloc_size = ncpu + 1;
            let cache_bytes = size_of::<MainCache>() * alloc_size;
            record_mmap_call(cache_bytes);
            let list = mmap_anonymous(
                null_mut(),
                cache_bytes,
                ProtFlags::READ | ProtFlags::WRITE,
                MapFlags::PRIVATE,
            )
            .unwrap_or_else(|err| {
                RSMallocError::OutOfMemory.log_and_abort(
                    null_mut(),
                    "cannot create main cache",
                    Some(err.raw_os_error()),
                )
            });

            inner.cache = list as *mut MainCache;
            if let Some(numa) = parse_numa_topology(ncpu) {
                inner.numa = numa;

                if numa.nnodes > 1 {
                    inner.is_numa = true;
                }
            } else {
                inner.numa = NumaTopology {
                    cpu_to_node: null_mut(),
                    ncpu: ncpu,
                    node_ids: null_mut(),
                    nnodes: 1,
                    cpu_ranges: null_mut(),
                    nranges: 2,
                };
            }

            if inner.is_numa {
                for range in 0..inner.numa.nranges {
                    let cpu_range = *inner.numa.cpu_ranges.add(range);
                    let start = cpu_range.start_cpu;
                    let end = cpu_range.end_cpu.min(ncpu.saturating_sub(1));

                    if start <= end {
                        let cache = inner.cache.add(start) as *mut _;
                        let len = size_of::<MainCache>() * (end - start + 1);
                        bind_node(cache, len, cpu_range.node_id);
                    }
                }
            }

            let alloc_size = ncpu + 1;
            let bitmap_words = ((alloc_size + 63) / 64).max(1);

            let bitmap_bytes = size_of::<AtomicU64>() * bitmap_words * NUM_SIZE_CLASSES;
            record_mmap_call(bitmap_bytes);
            let empty_bitmap = mmap_anonymous(
                null_mut(),
                bitmap_bytes,
                ProtFlags::READ | ProtFlags::WRITE,
                MapFlags::PRIVATE,
            )
            .unwrap_or_else(|err| {
                RSMallocError::OutOfMemory.log_and_abort(
                    null_mut(),
                    "cannot initialize empty bitmap",
                    Some(err.raw_os_error()),
                )
            }) as *mut AtomicU64;
            inner.nonempty_bitmap = empty_bitmap;
            inner.bitmap_words = bitmap_words;

            PENDING_QUEUE.init(inner.numa.nranges, inner.is_numa);
            NCPU = ncpu;
        });
    }

    #[inline(always)]
    fn cpu_word_bit(&self, cpu_id: usize) -> (usize, u64) {
        let word = cpu_id >> 6;
        let bit = 1u64 << (cpu_id & 63);
        (word, bit)
    }

    #[inline(always)]
    unsafe fn bitmap_word(
        &self,
        inner: &SlabCacheInner,
        class: usize,
        word: usize,
    ) -> *mut AtomicU64 {
        inner.nonempty_bitmap.add(class * inner.bitmap_words + word)
    }

    #[inline(always)]
    unsafe fn mark_class_nonempty(&self, inner: &SlabCacheInner, class: usize, cpu_id: usize) {
        let (word, bit) = self.cpu_word_bit(cpu_id);
        let ptr = self.bitmap_word(inner, class, word);

        if (*ptr).load(Ordering::Relaxed) & bit == 0 {
            (*ptr).fetch_or(bit, Ordering::Relaxed);
        }
    }

    #[inline(always)]
    unsafe fn clear_class_hint(&self, inner: &SlabCacheInner, class: usize, cpu_id: usize) {
        let (word, bit) = self.cpu_word_bit(cpu_id);
        let ptr = self.bitmap_word(inner, class, word);

        if (*ptr).load(Ordering::Relaxed) & bit != 0 {
            (*ptr).fetch_and(!bit, Ordering::Relaxed);
        }
    }

    #[inline(always)]
    unsafe fn first_nonempty_cpu_in_range(
        &self,
        inner: &SlabCacheInner,
        class: usize,
        cpu_id: usize,
        batch_size: usize,
        start: usize,
        end: usize,
    ) -> Option<(UnsafePointer<Header>, UnsafePointer<Header>, usize)> {
        let base = class * inner.bitmap_words;

        let start_word = start >> 6;
        let end_word = end >> 6;

        for word_idx in start_word..=end_word {
            let mut bits = (*inner.nonempty_bitmap.add(base + word_idx)).load(Ordering::Relaxed);

            if word_idx == start_word {
                bits &= !0u64 << (start & 63);
            }

            if word_idx == end_word {
                let last = end & 63;
                bits &= if last == 63 {
                    !0u64
                } else {
                    (1u64 << (last + 1)) - 1
                };
            }

            while bits != 0 {
                let bit = bits.trailing_zeros() as usize;
                bits &= bits - 1;

                let victim = word_idx * 64 + bit;

                if victim == cpu_id {
                    continue;
                }

                if let Some(block) = self.transfer_pop(class, victim, batch_size) {
                    return Some((
                        UnsafePointer::new(block.0),
                        UnsafePointer::new(block.1),
                        block.2,
                    ));
                }
            }
        }

        None
    }
}

impl GenericCache for SlabCache {
    #[inline(always)]
    unsafe fn push_tailed(
        &self,
        class: usize,
        header: *mut Header,
        tail: *mut Header,
        batch_size: usize,
    ) {
        let inner = &mut *self.inner.get();
        let rseq = get_rseq();

        let current_cpu = read_volatile(&rseq.cpu_id) as usize;
        let list = &mut (*inner.cache.add(current_cpu)).cache[class];
        let usage_ptr = &mut list.usage;

        if usage_ptr.load(Ordering::Relaxed) >= CACHE_HIGH_BLOCKS[class] {
            self.transfer_push_batch(class, header, tail, current_cpu, inner);
            return;
        }

        let list_ptr = addr_of!(list.list) as *mut *mut Header;
        if likely(
            RseqCore.push_tailed(
                list_ptr,
                rseq,
                current_cpu,
                header,
                tail,
                usage_ptr.as_ptr(),
                batch_size,
            ) == 1,
        ) {
            return;
        }

        self.transfer_push_batch(class, header, tail, current_cpu, inner);

        #[cfg(feature = "debug")]
        ABORTS.fetch_add(1, Relaxed);
    }

    #[inline(always)]
    unsafe fn push(&self, class: usize, header: *mut Header) {
        let inner = &mut *self.inner.get();
        let rseq = get_rseq();
        let mut loop_count = 0;

        loop {
            let current_cpu = read_volatile(&rseq.cpu_id) as usize;
            let list = &mut (*inner.cache.add(current_cpu)).cache[class];
            let usage_ptr = &mut list.usage;

            if usage_ptr.load(Ordering::Relaxed) >= CACHE_HIGH_BLOCKS[class] {
                self.transfer_push_single(class, header, current_cpu, inner);
                return;
            }

            let list_ptr = addr_of!(list.list) as *mut *mut Header;
            if likely(RseqCore.push(list_ptr, rseq, current_cpu, header, usage_ptr.as_ptr()) == 1) {
                break;
            }

            if loop_count > 3 {
                self.transfer_push_single(class, header, current_cpu, inner);
                return;
            }

            #[cfg(feature = "debug")]
            ABORTS.fetch_add(1, Relaxed);

            loop_count += 1;
        }
    }

    #[inline(always)]
    unsafe fn pop(&self, class: usize) -> UnsafePointer<Header> {
        let inner = &mut *self.inner.get();
        let rseq = get_rseq();
        let mut loop_count = 0;

        loop {
            let current_cpu = read_volatile(&rseq.cpu_id) as usize;
            let list = &mut (*inner.cache.add(current_cpu)).cache[class];
            let list_ptr = addr_of!(list.list) as *mut *mut Header;
            let usage_ptr = &list.usage;
            let header = RseqCore.pop(list_ptr, rseq, current_cpu, usage_ptr.as_ptr());

            if header as isize == -1 {
                if loop_count == 3 {
                    let mail = self.transfer_pop_single(class, current_cpu);
                    if !mail.is_null() {
                        return mail;
                    }
                    loop_count = 0;
                }

                #[cfg(feature = "debug")]
                ABORTS.fetch_add(1, Relaxed);

                loop_count += 1;
                continue;
            }

            return UnsafePointer::new(header);
        }
    }
}

const PTR_ALIGN: usize = align_of::<Header>();
const TAG_MASK: usize = PTR_ALIGN - 1;
const PTR_MASK: usize = !TAG_MASK;

#[inline(always)]
pub fn pack(ptr: *mut Header, old_tag: usize) -> usize {
    ((ptr as usize) & PTR_MASK) | old_tag.wrapping_add(1) & TAG_MASK
}

#[inline(always)]
pub fn unpack_ptr(word: usize) -> (*mut Header, usize) {
    ((word & PTR_MASK) as *mut Header, word)
}

impl SlabCache {
    #[cfg(feature = "debug")]
    pub unsafe fn get_rseq_cpu_class_usage_bytes(&self, cpu_id: usize, class: usize) -> usize {
        use crate::utility::{SIZE_CLASSES, align_to};
        let inner = self.get_inner();
        let cpu = &(*inner.cache.add(cpu_id)).cache[class];
        let blocks = cpu.usage.load(Relaxed);
        let block_size = align_to(SIZE_CLASSES[class] + Header::SIZE, 16);
        blocks.saturating_mul(block_size)
    }

    #[cfg(feature = "debug")]
    pub unsafe fn get_rseq_cpu_usage_bytes(&self, cpu_id: usize) -> usize {
        let mut total_usage = 0usize;
        for class in 0..NUM_SIZE_CLASSES {
            total_usage =
                total_usage.saturating_add(self.get_rseq_cpu_class_usage_bytes(cpu_id, class));
        }

        total_usage
    }

    #[cfg(feature = "debug-print")]
    pub unsafe fn transfer_hint_bits(&self, class: usize) -> String {
        let inner = &*self.inner.get();
        let ncpu = inner.numa.ncpu;
        let mut out = String::with_capacity(ncpu);

        for cpu in 0..ncpu {
            let (word, bit) = self.cpu_word_bit(cpu);
            let ptr = self.bitmap_word(inner, class, word);
            let bits = (*ptr).load(Ordering::Relaxed);
            out.push(if bits & bit != 0 { '1' } else { '0' });
        }

        out
    }

    pub fn get_inner(&self) -> &mut SlabCacheInner {
        unsafe { &mut *self.inner.get() }
    }

    #[inline(always)]
    pub unsafe fn node_for_cpu(&self, cpu_id: usize, inner: &SlabCacheInner) -> u16 {
        if !inner.is_numa {
            return 0;
        }
        *inner.numa.cpu_to_node.add(cpu_id)
    }

    #[inline(always)]
    pub unsafe fn get_numa_and_inner(&self) -> (&NumaTopology, &SlabCacheInner) {
        let inner = &*self.inner.get();
        (&inner.numa, inner)
    }

    pub unsafe fn get_list(&self, cpu_id: usize, class: usize) -> &mut TransferCache {
        let inner = &mut *self.inner.get();
        let list = &mut (*inner.cache.add(cpu_id)).mail[class];
        list
    }

    #[inline(always)]
    pub unsafe fn try_pop(
        &self,
        class: usize,
        batch_size: usize,
        cpu_id: usize,
    ) -> (UnsafePointer<Header>, UnsafePointer<Header>, usize) {
        if let Some(popped) = self.transfer_pop(class, cpu_id, batch_size) {
            return (
                UnsafePointer::new(popped.0),
                UnsafePointer::new(popped.1),
                popped.2,
            );
        }

        let inner = *self.inner.get();
        let (start, end, node_id) = if inner.is_numa {
            let numa_id = *inner.numa.cpu_to_node.add(cpu_id);
            let range = *inner.numa.cpu_ranges.add(numa_id as usize);

            (range.start_cpu, range.end_cpu, numa_id)
        } else {
            (0, inner.numa.ncpu - 1, 0)
        };

        if let Some(block) =
            self.first_nonempty_cpu_in_range(&inner, class, cpu_id, batch_size, start, end)
        {
            #[cfg(feature = "transfer-debug")]
            crate::TOTAL_TRANSFER_STEALS.fetch_add(1, Ordering::Relaxed);

            return block;
        }

        if inner.is_numa {
            for i in 1..inner.numa.nranges {
                let node_id = (i + node_id as usize) % inner.numa.nranges;
                let (start, end) = {
                    let cpu = *inner.numa.cpu_ranges.add(node_id);
                    (cpu.start_cpu, cpu.end_cpu)
                };

                if let Some(block) =
                    self.first_nonempty_cpu_in_range(&inner, class, cpu_id, batch_size, start, end)
                {
                    #[cfg(feature = "transfer-debug")]
                    crate::TOTAL_TRANSFER_STEALS.fetch_add(1, Ordering::Relaxed);

                    return block;
                }
            }
        }

        #[cfg(feature = "transfer-debug")]
        crate::DRY_TRANSFER_STEALS.fetch_add(1, Ordering::Relaxed);

        (UnsafePointer::NULL, UnsafePointer::NULL, 0)
    }

    #[inline(always)]
    pub unsafe fn transfer_push_batch(
        &self,
        class: usize,
        start: *mut Header,
        tail: *mut Header,
        cpu_id: usize,
        inner: &mut SlabCacheInner,
    ) {
        #[cfg(feature = "transfer-debug-exact")]
        crate::TOTAL_TRANSFER_PUSH_CALLS.fetch_add(1, Ordering::Relaxed);

        let list = &mut (*inner.cache.add(cpu_id)).mail[class];
        let list_ptr = &list.list;

        loop {
            let old = list_ptr.load(Ordering::Relaxed);
            let (old_head, tag) = unpack_ptr(old);

            (*tail).next = old_head;

            if list_ptr
                .compare_exchange(old, pack(start, tag), Ordering::Release, Ordering::Relaxed)
                .is_ok()
            {
                if old_head.is_null() {
                    self.mark_class_nonempty(inner, class, cpu_id);
                }
                return;
            }

            #[cfg(feature = "transfer-debug")]
            crate::TOTAL_TRANSFER_RETRIES.fetch_add(1, Ordering::Relaxed);

            spin_loop();
        }
    }

    #[inline(always)]
    pub unsafe fn transfer_push_single_to(
        &self,
        list_ptr: &AtomicUsize,
        class: usize,
        header: *mut Header,
        cpu_id: usize,
        inner: &mut SlabCacheInner,
    ) {
        #[cfg(feature = "transfer-debug-exact")]
        crate::TOTAL_TRANSFER_PUSH_CALLS.fetch_add(1, Ordering::Relaxed);

        loop {
            let old = list_ptr.load(Ordering::Relaxed);
            let (old_head, tag) = unpack_ptr(old);

            (*header).next = old_head;
            if list_ptr
                .compare_exchange(old, pack(header, tag), Ordering::Release, Ordering::Relaxed)
                .is_ok()
            {
                if old_head.is_null() {
                    self.mark_class_nonempty(inner, class, cpu_id);
                }
                return;
            }

            #[cfg(feature = "transfer-debug")]
            crate::TOTAL_TRANSFER_RETRIES.fetch_add(1, Ordering::Relaxed);

            spin_loop();
        }
    }

    pub unsafe fn transfer_push_single(
        &self,
        class: usize,
        header: *mut Header,
        cpu_id: usize,
        inner: &mut SlabCacheInner,
    ) {
        let list = &mut (*inner.cache.add(cpu_id)).mail[class];
        let list_ptr = &list.list;

        self.transfer_push_single_to(list_ptr, class, header, cpu_id, inner);
    }

    pub unsafe fn transfer_push_single_trimmed(
        &self,
        class: usize,
        header: *mut Header,
        cpu_id: usize,
        inner: &mut SlabCacheInner,
    ) {
        let list = &mut (*inner.cache.add(cpu_id)).mail[class];
        let list_ptr = &list.trimmed;

        self.transfer_push_single_to(list_ptr, class, header, cpu_id, inner);
    }

    #[inline(always)]
    pub unsafe fn transfer_pop_single(&self, class: usize, cpu_id: usize) -> UnsafePointer<Header> {
        #[cfg(feature = "transfer-debug-exact")]
        crate::TOTAL_TRANSFER_POP_CALLS.fetch_add(1, Ordering::Relaxed);

        let inner = &mut *self.inner.get();
        let list = &mut (*inner.cache.add(cpu_id)).mail[class];
        let normal_ptr = &list.list;
        let trimmed_ptr = &list.trimmed;
        let mut list_ptr = normal_ptr;

        loop {
            let old = list_ptr.load(Ordering::Acquire);
            let (head, tag) = unpack_ptr(old);

            if head.is_null() {
                if eq(list_ptr, normal_ptr) {
                    list_ptr = &trimmed_ptr;
                    continue;
                }

                self.clear_class_hint(inner, class, cpu_id);
                if !unpack_ptr(normal_ptr.load(Ordering::Acquire)).0.is_null()
                    || !unpack_ptr(trimmed_ptr.load(Ordering::Acquire)).0.is_null()
                {
                    self.mark_class_nonempty(inner, class, cpu_id);
                }
                return UnsafePointer::NULL;
            }

            let next = (*head).next;
            if list_ptr
                .compare_exchange(old, pack(next, tag), Ordering::Acquire, Ordering::Relaxed)
                .is_ok()
            {
                if !next.is_null() {
                    _mm_prefetch(next as *const i8, _MM_HINT_T0);
                }
                return UnsafePointer::new(head);
            }

            #[cfg(feature = "transfer-debug")]
            crate::TOTAL_TRANSFER_RETRIES.fetch_add(1, Ordering::Relaxed);

            spin_loop();
        }
    }

    #[inline(always)]
    pub unsafe fn transfer_pop(
        &self,
        class: usize,
        cpu_id: usize,
        batch_size: usize,
    ) -> Option<(*mut Header, *mut Header, usize)> {
        #[cfg(feature = "transfer-debug-exact")]
        crate::TOTAL_TRANSFER_POP_CALLS.fetch_add(1, Ordering::Relaxed);

        let inner = &mut *self.inner.get();
        let list = &mut (*inner.cache.add(cpu_id)).mail[class];
        let normal_ptr = &list.list;
        let trimmed_ptr = &list.trimmed;
        let mut list_ptr = normal_ptr;

        loop {
            let old = list_ptr.load(Ordering::Acquire);
            let (head, tag) = unpack_ptr(old);

            if head.is_null() {
                if eq(list_ptr, normal_ptr) {
                    list_ptr = &trimmed_ptr;
                    continue;
                }

                self.clear_class_hint(inner, class, cpu_id);
                if !unpack_ptr(normal_ptr.load(Ordering::Acquire)).0.is_null()
                    || !unpack_ptr(trimmed_ptr.load(Ordering::Acquire)).0.is_null()
                {
                    self.mark_class_nonempty(inner, class, cpu_id);
                }
                return None;
            }

            let mut tail = head;
            let mut count = 1usize;
            let mut next = (*tail).next;
            if !next.is_null() {
                _mm_prefetch(next as *const i8, _MM_HINT_T0)
            };

            while count < batch_size && !next.is_null() {
                tail = next;
                next = (*tail).next;
                count += 1;
            }

            if list_ptr
                .compare_exchange(old, pack(next, tag), Ordering::Acquire, Ordering::Relaxed)
                .is_ok()
            {
                if !next.is_null() {
                    _mm_prefetch(next as *const i8, _MM_HINT_T0);
                }
                (*tail).next = null_mut();
                return Some((head, tail, count));
            }

            #[cfg(feature = "transfer-debug")]
            crate::TOTAL_TRANSFER_RETRIES.fetch_add(1, Ordering::Relaxed);

            spin_loop();
        }
    }
}

pub static SLAB_CACHE: SlabCache = SlabCache::new();

#[cfg(test)]
#[cfg(not(feature = "extended-header"))]
mod tests {
    use std::hint::black_box;

    use super::*;

    fn header() -> *mut Header {
        Box::into_raw(Box::new(Header {
            next: null_mut(),
            class: 0,
            magic: 0,
            life_time: 0,
            flags: 0,
        }))
    }

    #[test]
    fn test_correctness() {
        unsafe {
            let rseq = SlabCache::new();
            black_box(rseq.ensure_cache());
            rseq.push(0, header());
            let cache = rseq.pop(0);
            eprintln!("{}", cache.is_null());
            assert!(!cache.is_null())
        }
    }
}
