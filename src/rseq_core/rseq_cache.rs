use std::{
    arch::x86_64::{_MM_HINT_T0, _mm_prefetch},
    cell::UnsafeCell,
    hint::{likely, spin_loop, unlikely},
    ptr::{addr_of, null_mut, read_volatile},
    sync::atomic::{
        AtomicU64, AtomicUsize,
        Ordering::{self, Relaxed},
    },
};

use rustix::mm::{MapFlags, ProtFlags, mmap_anonymous};

#[cfg(feature = "debug")]
use crate::ABORTS;
use crate::{
    GenericCache, Header, RSMallocError, RseqCoreTrait,
    core_prim::wrappers::UnsafePointer,
    internals::{
        numa_parser::{NumaTopology, parse_numa_topology},
        once::Once,
    },
    rseq_core::{pending_queue::PENDING_QUEUE, rseq_asm::RseqCore, rseq_main::get_rseq},
    utility::{NUM_SIZE_CLASSES, RSEQ_MAX_BLOCKS},
};

pub struct ClassCache {
    list: UnsafePointer<Header>,
    usage: AtomicUsize,
}

pub struct SelfMail {
    pub list: AtomicUsize,
}

// NOTE: Use 4096-byte alignment to avoid false sharing between cache lines and NUMA node balancing.
#[repr(C, align(4096))]
pub struct MainCache {
    cache: [ClassCache; NUM_SIZE_CLASSES],
    mail: [SelfMail; NUM_SIZE_CLASSES],
}

#[derive(Debug, Clone, Copy)]
pub struct RseqInner {
    cache: *mut MainCache,
    numa: NumaTopology,
    numa_map: *mut AtomicU64,
    pub is_numa: bool,
}

pub struct RseqCache {
    inner: UnsafeCell<RseqInner>,
    once: Once,
}

unsafe impl Send for RseqCache {}
unsafe impl Sync for RseqCache {}

// here be dragons: TODO: use it until found a better way
unsafe extern "C" {
    fn get_nprocs_conf() -> i32;
}

unsafe fn get_max_cpu() -> u32 {
    let ncpu = get_nprocs_conf();
    if ncpu > 0 { ncpu as u32 } else { 256 }
}

impl RseqCache {
    pub const fn new() -> Self {
        Self {
            inner: UnsafeCell::new(RseqInner {
                cache: null_mut(),
                numa: NumaTopology {
                    cpu_to_node: null_mut(),
                    ncpu: 0,
                    node_ids: null_mut(),
                    nnodes: 0,
                    cpu_ranges: null_mut(),
                    nranges: 0,
                },
                numa_map: null_mut(),
                is_numa: false,
            }),
            once: Once::new(),
        }
    }

    #[inline(always)]
    fn class(&self, class: usize) -> u64 {
        1u64 << class
    }

    #[inline(always)]
    unsafe fn mark_numa_nonempty(&self, inner: &RseqInner, node: usize, class: usize) {
        let numa_map = inner.numa_map.add(node);
        let bit = self.class(class);

        if (*numa_map).load(Ordering::Acquire) & bit == 0 {
            (*numa_map).fetch_or(bit, Ordering::Release);
        }
    }

    #[inline(always)]
    unsafe fn clear_numa_hint(&self, inner: &RseqInner, node: usize, class: usize) {
        let numa_map = inner.numa_map.add(node);
        let bit = self.class(class);

        if (*numa_map).load(Ordering::Acquire) & bit != 0 {
            (*numa_map).fetch_and(!bit, Ordering::Release);
        }
    }

    #[inline(always)]
    unsafe fn is_empty(&self, inner: &RseqInner, node: usize, class: usize) -> bool {
        let numa_map = inner.numa_map.add(node);
        (*numa_map).load(Ordering::Acquire) & self.class(class) == 0
    }

    pub unsafe fn ensure_cache(&self) {
        self.once.call_once(|| {
            let cache = self.inner.get();
            let inner = &mut *cache;
            let ncpu = get_max_cpu() as usize;
            let alloc_size = ncpu + 1;
            let list = mmap_anonymous(
                null_mut(),
                size_of::<MainCache>() * alloc_size,
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
                    nnodes: 0,
                    cpu_ranges: null_mut(),
                    nranges: 1,
                };
            }

            if inner.is_numa {
                let range = mmap_anonymous(
                    null_mut(),
                    size_of::<AtomicU64>() * inner.numa.nranges,
                    ProtFlags::READ | ProtFlags::WRITE,
                    MapFlags::PRIVATE,
                )
                .unwrap_or_else(|err| {
                    RSMallocError::OutOfMemory.log_and_abort(
                        null_mut(),
                        "cannot initialize numa bitmap",
                        Some(err.raw_os_error()),
                    )
                }) as *mut AtomicU64;
                inner.numa_map = range;
            }

            PENDING_QUEUE.init(inner.numa.nranges, inner.is_numa);
        });
    }
}

impl GenericCache for RseqCache {
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
        let mut loop_count = 0;

        loop {
            let current_cpu = read_volatile(&rseq.cpu_id) as usize;

            #[cfg(feature = "rseq-thread-failure-fallback")]
            if unlikely(current_cpu >= inner.numa.ncpu) {
                self.mail_push_batch(class, header, tail, inner.numa.ncpu);
                return;
            }

            let list = &mut (*inner.cache.add(current_cpu)).cache[class];
            let usage_ptr = &mut list.usage;

            if usage_ptr.load(Ordering::Relaxed) >= RSEQ_MAX_BLOCKS[class] {
                self.mail_push_batch(class, header, tail, current_cpu);
                return;
            }

            let list_ptr = addr_of!(list.list) as *mut *mut Header;
            if likely(RseqCore.push_tailed(list_ptr, rseq, current_cpu, header, tail) == 1) {
                let usage = usage_ptr.load(Relaxed);
                usage_ptr.store(usage + batch_size, Relaxed);
                break;
            }

            if loop_count > 3 {
                self.mail_push_batch(class, header, tail, current_cpu);
                return;
            }

            #[cfg(feature = "debug")]
            ABORTS.fetch_add(1, Relaxed);

            loop_count += 1;
        }
    }

    #[inline(always)]
    unsafe fn push(&self, class: usize, header: *mut Header) {
        let inner = &mut *self.inner.get();
        let rseq = get_rseq();
        let mut loop_count = 0;

        loop {
            let current_cpu = read_volatile(&rseq.cpu_id) as usize;

            #[cfg(feature = "rseq-thread-failure-fallback")]
            if unlikely(current_cpu >= inner.numa.ncpu) {
                self.mail_push_single(class, header, inner.numa.ncpu);
                return;
            }

            let list = &mut (*inner.cache.add(current_cpu)).cache[class];
            let usage_ptr = &mut list.usage;

            if usage_ptr.load(Ordering::Relaxed) >= RSEQ_MAX_BLOCKS[class] {
                self.mail_push_single(class, header, current_cpu);
                return;
            }

            let list_ptr = addr_of!(list.list) as *mut *mut Header;
            if likely(RseqCore.push(list_ptr, rseq, current_cpu, header) == 1) {
                let usage = usage_ptr.load(Relaxed);
                usage_ptr.store(usage + 1, Relaxed);
                break;
            }

            if loop_count > 3 {
                self.mail_push_single(class, header, current_cpu);
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
        let mut last_core = 0;

        loop {
            let current_cpu = read_volatile(&rseq.cpu_id) as usize;
            #[cfg(feature = "rseq-thread-failure-fallback")]
            if unlikely(current_cpu >= inner.numa.ncpu) {
                return self.try_pop_single(class, inner.numa.ncpu);
            }
            let list = &mut (*inner.cache.add(current_cpu)).cache[class];
            let list_ptr = addr_of!(list.list) as *mut *mut Header;
            let usage_ptr = &mut list.usage;
            let header = RseqCore.pop(list_ptr, rseq, current_cpu);

            if unlikely(header as isize == -1) {
                if loop_count >= 3 {
                    if last_core != current_cpu {
                        let mail = self.mail_pop_single(class, current_cpu);

                        if !mail.is_null() {
                            return mail;
                        }
                    }

                    last_core = current_cpu;
                    loop_count = 0;
                }

                #[cfg(feature = "debug")]
                ABORTS.fetch_add(1, Relaxed);

                loop_count += 1;
                continue;
            }

            // Approximate pressure counter: prefer stale-low drift over stale-high drift.
            // Stale-high values push too much traffic into mail and can cause performance cliffs.
            // Stale-low values may overfill this CPU cache slightly, which is acceptable here.
            let usage = usage_ptr.load(Relaxed);
            if usage > 0 {
                usage_ptr.store(usage - 1, Relaxed);
            }

            return UnsafePointer::new(header);
        }
    }
}

const PTR_ALIGN: usize = align_of::<Header>();
const TAG_MASK: usize = PTR_ALIGN - 1;
const PTR_MASK: usize = !TAG_MASK;

#[inline(always)]
pub fn pack(ptr: *mut Header, old_word: usize) -> usize {
    let old_tag = old_word & TAG_MASK;
    let new_tag = old_tag.wrapping_add(1) & TAG_MASK;

    ((ptr as usize) & PTR_MASK) | new_tag
}

#[inline(always)]
pub fn unpack_ptr(word: usize) -> *mut Header {
    (word & PTR_MASK) as *mut Header
}

impl RseqCache {
    pub fn get_ncpu(&self) -> usize {
        unsafe { (*self.inner.get()).numa.ncpu }
    }

    #[inline(always)]
    pub unsafe fn node_for_cpu(&self, cpu_id: usize, inner: &RseqInner) -> u16 {
        if !inner.is_numa {
            return 0;
        }
        *inner.numa.cpu_to_node.add(cpu_id)
    }

    #[inline(always)]
    pub unsafe fn get_numa_and_inner(&self) -> (&NumaTopology, &RseqInner) {
        let inner = &*self.inner.get();
        (&inner.numa, inner)
    }

    pub unsafe fn get_list(&self, cpu_id: usize, class: usize) -> &mut SelfMail {
        let inner = &mut *self.inner.get();
        let list = &mut (*inner.cache.add(cpu_id)).mail[class];
        list
    }

    // Numa-aware steal is impossible
    #[cfg(feature = "rseq-thread-failure-fallback")]
    #[inline(never)]
    pub unsafe fn try_pop_single(&self, class: usize, ncpu: usize) -> UnsafePointer<Header> {
        let mail = self.mail_pop_single(class, ncpu);
        if !mail.is_null() {
            return mail;
        }

        for i in 1..ncpu + 1 {
            let victim = (ncpu + i) % ncpu;

            let mail = self.mail_pop_single(class, victim);
            if !mail.is_null() {
                return mail;
            }
        }

        UnsafePointer::NULL
    }

    #[inline(always)]
    pub unsafe fn try_pop(
        &self,
        class: usize,
        batch_size: usize,
        cpu_id: usize,
    ) -> (UnsafePointer<Header>, UnsafePointer<Header>, usize) {
        if let Some(popped) = self.mail_pop(class, cpu_id, batch_size) {
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

        let len = end - start + 1;
        let local = cpu_id.saturating_sub(start);

        for step in 1..len {
            let victim = start + ((local + step) % len);

            if let Some(block) = self.mail_pop(class, victim, batch_size) {
                return (
                    UnsafePointer::new(block.0),
                    UnsafePointer::new(block.1),
                    block.2,
                );
            }
        }

        if inner.is_numa {
            for i in 1..inner.numa.nranges {
                let node_id = (i + node_id as usize) % inner.numa.nranges;

                if self.is_empty(&inner, node_id, class) {
                    continue;
                }

                let (start, end) = {
                    let cpu = *inner.numa.cpu_ranges.add(node_id);
                    (cpu.start_cpu, cpu.end_cpu)
                };

                let len = end - start + 1;
                let local = cpu_id.saturating_sub(start);

                for step in 1..len {
                    let victim = start + ((local + step) % len);

                    if let Some(block) = self.mail_pop(class, victim, batch_size) {
                        return (
                            UnsafePointer::new(block.0),
                            UnsafePointer::new(block.1),
                            block.2,
                        );
                    }
                }

                self.clear_numa_hint(&inner, node_id, class);
            }
        }

        (UnsafePointer::NULL, UnsafePointer::NULL, 0)
    }

    #[inline(always)]
    pub unsafe fn mail_push_batch(
        &self,
        class: usize,
        start: *mut Header,
        tail: *mut Header,
        cpu_id: usize,
    ) {
        let inner = &mut *self.inner.get();
        let list = &mut (*inner.cache.add(cpu_id)).mail[class];
        let list_ptr = &list.list;

        loop {
            let old = list_ptr.load(Ordering::Relaxed);
            let old_head = unpack_ptr(old);

            (*tail).next = old_head;

            let new = pack(start, old);

            if list_ptr
                .compare_exchange(old, new, Ordering::Release, Ordering::Relaxed)
                .is_ok()
            {
                if inner.is_numa {
                    let node_id = self.node_for_cpu(cpu_id, inner);
                    self.mark_numa_nonempty(&inner, node_id as usize, class);
                }
                return;
            }

            spin_loop();
        }
    }

    #[inline(always)]
    pub unsafe fn mail_push_single(&self, class: usize, header: *mut Header, cpu_id: usize) {
        let inner = &mut *self.inner.get();
        let list = &mut (*inner.cache.add(cpu_id)).mail[class];
        let list_ptr = &list.list;

        loop {
            let old = list_ptr.load(Ordering::Relaxed);
            let old_head = unpack_ptr(old);

            (*header).next = old_head;

            let new = pack(header, old);

            if list_ptr
                .compare_exchange(old, new, Ordering::Release, Ordering::Relaxed)
                .is_ok()
            {
                if inner.is_numa {
                    let node_id = self.node_for_cpu(cpu_id, inner);
                    self.mark_numa_nonempty(&inner, node_id as usize, class);
                }
                return;
            }

            spin_loop();
        }
    }

    #[inline(always)]
    pub unsafe fn mail_pop_single(&self, class: usize, cpu_id: usize) -> UnsafePointer<Header> {
        let inner = &mut *self.inner.get();
        let list = &mut (*inner.cache.add(cpu_id)).mail[class];
        let list_ptr = &list.list;

        loop {
            let old = list_ptr.load(Ordering::Acquire);
            let head = unpack_ptr(old);

            if head.is_null() {
                return UnsafePointer::NULL;
            }

            let next = (*head).next;

            if list_ptr
                .compare_exchange(old, pack(next, old), Ordering::Acquire, Ordering::Relaxed)
                .is_ok()
            {
                return UnsafePointer::new(head);
            }

            spin_loop();
        }
    }

    #[inline(always)]
    pub unsafe fn mail_pop(
        &self,
        class: usize,
        cpu_id: usize,
        batch_size: usize,
    ) -> Option<(*mut Header, *mut Header, usize)> {
        let inner = &mut *self.inner.get();
        let list = &mut (*inner.cache.add(cpu_id)).mail[class];
        let list_ptr = &list.list;

        loop {
            let old = list_ptr.load(Ordering::Acquire);
            let head = unpack_ptr(old);

            if head.is_null() {
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

            let new = pack(next, old);

            if list_ptr
                .compare_exchange(old, new, Ordering::Acquire, Ordering::Relaxed)
                .is_ok()
            {
                (*tail).next = null_mut();
                return Some((head, tail, count));
            }

            spin_loop();
        }
    }
}

pub static RSEQ_CACHE: RseqCache = RseqCache::new();

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
            let rseq = RseqCache::new();
            black_box(rseq.ensure_cache());
            rseq.push(0, header());
            let cache = rseq.pop(0);
            eprintln!("{}", cache.is_null());
            assert!(!cache.is_null())
        }
    }
}
