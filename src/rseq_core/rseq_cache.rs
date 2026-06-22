use std::{
    arch::x86_64::{_MM_HINT_T0, _mm_prefetch},
    cell::UnsafeCell,
    hint::{likely, unlikely},
    ptr::{addr_of, null_mut, read_volatile},
    sync::atomic::{
        AtomicUsize,
        Ordering::{self, Relaxed},
    },
};

use rustix::mm::{MapFlags, ProtFlags, mmap_anonymous};

#[cfg(feature = "debug")]
use crate::ABORTS;
#[cfg(feature = "cpu-refill-paths")]
use crate::MetaData;
use crate::{
    GenericCache, Header, RSMallocError, RseqCoreTrait,
    core_prim::wrappers::UnsafePointer,
    internals::{lock::SerialLock, once::Once},
    rseq_core::{rseq_core::RseqCore, rseq_main::get_rseq},
    utility::{NUM_SIZE_CLASSES, RSEQ_MAX_BLOCKS},
};

pub struct ClassCache {
    list: UnsafePointer<Header>,
    usage: AtomicUsize,
}

pub struct SelfMail {
    list: AtomicUsize,
    trim_lock: SerialLock,
}

#[cfg(feature = "cpu-refill-paths")]
pub struct CPUBulkClass {
    meta: UnsafeCell<*mut MetaData>,
    lock: SerialLock,
}

#[cfg(feature = "cpu-refill-paths")]
impl CPUBulkClass {
    #[inline(always)]
    pub fn lock(&self) -> &SerialLock {
        &self.lock
    }

    #[inline(always)]
    pub unsafe fn get_metadata(&self) -> *mut MetaData {
        *self.meta.get()
    }

    #[inline(always)]
    pub unsafe fn set_metadata(&self, meta: *mut MetaData) {
        *self.meta.get() = meta;
    }
}

// NOTE: Use 4096-byte alignment to avoid false sharing between cache lines and NUMA node balancing in future updates.
#[repr(C, align(4096))]
pub struct MainCache {
    cache: [ClassCache; NUM_SIZE_CLASSES],
    mail: [SelfMail; NUM_SIZE_CLASSES],
    #[cfg(feature = "cpu-refill-paths")]
    bulk_fill: [CPUBulkClass; NUM_SIZE_CLASSES],
}

pub struct RseqInner {
    cache: *mut MainCache,
    ncpu: usize,
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
                ncpu: 0,
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
            inner.ncpu = ncpu;
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
            if unlikely(current_cpu >= inner.ncpu) {
                self.mail_push_batch(class, header, tail, inner.ncpu);
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

            if unlikely(loop_count > 3) {
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
            if unlikely(current_cpu >= inner.ncpu) {
                self.mail_push_single(class, header, inner.ncpu);
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

            if unlikely(loop_count > 3) {
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
            if unlikely(current_cpu >= inner.ncpu) {
                return self.try_pop_single(class, inner.ncpu);
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
fn pack(ptr: *mut Header, old_word: usize) -> usize {
    let old_tag = old_word & TAG_MASK;
    let new_tag = old_tag.wrapping_add(1) & TAG_MASK;

    ((ptr as usize) & PTR_MASK) | new_tag
}

#[inline(always)]
fn unpack_ptr(word: usize) -> *mut Header {
    (word & PTR_MASK) as *mut Header
}

impl RseqCache {
    // TODO: add numa-awareness, prefer local node first
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

    // TODO: Add NUMA-aware stealing
    #[inline(always)]
    pub unsafe fn try_pop(
        &self,
        class: usize,
        batch_size: usize,
        cpu_id: usize,
    ) -> (UnsafePointer<Header>, UnsafePointer<Header>, usize) {
        let ncpu = (*self.inner.get()).ncpu;

        #[cfg(feature = "rseq-thread-failure-fallback")]
        let cpu_id = if unlikely(cpu_id >= ncpu) {
            ncpu
        } else {
            cpu_id
        };

        if let Some(popped) = self.mail_pop(class, cpu_id, batch_size) {
            return (
                UnsafePointer::new(popped.0),
                UnsafePointer::new(popped.1),
                popped.2,
            );
        }

        for i in 1..ncpu {
            let victim = (cpu_id + i) % ncpu;

            if let Some(block) = self.mail_pop(class, victim, batch_size) {
                return (
                    UnsafePointer::new(block.0),
                    UnsafePointer::new(block.1),
                    block.2,
                );
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
            list.trim_lock.spin_until_unlock();

            let old = list_ptr.load(Ordering::Relaxed);
            let old_head = unpack_ptr(old);

            (*tail).next = old_head;

            let new = pack(start, old);

            if list_ptr
                .compare_exchange(old, new, Ordering::Release, Ordering::Relaxed)
                .is_ok()
            {
                return;
            }
        }
    }

    #[inline(always)]
    pub unsafe fn mail_push_single(&self, class: usize, header: *mut Header, cpu_id: usize) {
        let inner = &mut *self.inner.get();
        let list = &mut (*inner.cache.add(cpu_id)).mail[class];
        let list_ptr = &list.list;

        loop {
            list.trim_lock.spin_until_unlock();

            let old = list_ptr.load(Ordering::Relaxed);
            let old_head = unpack_ptr(old);

            (*header).next = old_head;

            let new = pack(header, old);

            if list_ptr
                .compare_exchange(old, new, Ordering::Release, Ordering::Relaxed)
                .is_ok()
            {
                return;
            }
        }
    }

    #[inline(always)]
    pub unsafe fn mail_pop_single(&self, class: usize, cpu_id: usize) -> UnsafePointer<Header> {
        let inner = &mut *self.inner.get();
        let list = &mut (*inner.cache.add(cpu_id)).mail[class];
        let list_ptr = &list.list;

        loop {
            list.trim_lock.spin_until_unlock();

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

            list.trim_lock.spin_until_unlock();

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
        }
    }

    #[cfg(feature = "cpu-refill-paths")]
    pub unsafe fn get_bulk_fill(&self, size_class: usize, cpu_id: usize) -> &CPUBulkClass {
        let inner = &*self.inner.get();

        #[cfg(feature = "rseq-thread-failure-fallback")]
        let cpu_id = if unlikely(cpu_id >= inner.ncpu) {
            inner.ncpu
        } else {
            cpu_id
        };

        &(*inner.cache.add(cpu_id)).bulk_fill[size_class]
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
            canary: 0,
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
