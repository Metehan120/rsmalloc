use std::{
    arch::x86_64::{_MM_HINT_T0, _mm_prefetch},
    cell::UnsafeCell,
    hint::{likely, spin_loop, unlikely},
    ptr::{addr_of, null_mut, read_volatile},
    sync::atomic::{AtomicUsize, Ordering},
};

use rustix::mm::{MapFlags, ProtFlags, mmap_anonymous};

use crate::{
    GenericCache, Header, RseqCoreTrait,
    core_prim::wrappers::UnsafePointer,
    inner::libc_int::get_nprocs_conf,
    internals::{lock::SerialLock, once::Once},
    rseq_core::{rseq_core::RseqCore, rseq_main::get_rseq},
    utility::{NUM_SIZE_CLASSES, RSEQ_MAX_BLOCKS},
};

pub struct ClassCache {
    list: UnsafePointer<Header>,
    usage: usize,
}

pub struct MailCache {
    list: AtomicUsize,
    usage: AtomicUsize,
    trim_lock: SerialLock,
}

#[repr(C, align(2048))]
pub struct MainCache {
    cache: [ClassCache; NUM_SIZE_CLASSES],
    mail: [MailCache; NUM_SIZE_CLASSES],
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
            .unwrap_or_else(|_| panic!("Cannot create main cache"));

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
            let list = &mut (*inner.cache.add(current_cpu)).cache[class];
            let usage_ptr = &mut list.usage;

            if *usage_ptr > RSEQ_MAX_BLOCKS[class] {
                self.mail_push_batch(class, header, tail, batch_size, current_cpu);
                return;
            }

            let list_ptr = addr_of!(list.list) as *mut *mut Header;
            if likely(
                RseqCore.push_tailed(
                    list_ptr,
                    usage_ptr,
                    rseq,
                    current_cpu,
                    header,
                    tail,
                    batch_size,
                ) == 1,
            ) {
                _mm_prefetch(list.list.cast_as_ptr() as *mut i8, _MM_HINT_T0);
                break;
            }

            if unlikely(loop_count > 5) {
                self.mail_push_batch(class, header, tail, batch_size, current_cpu);
                return;
            }

            loop_count += 1;
            spin_loop();
        }
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

            if *usage_ptr > RSEQ_MAX_BLOCKS[class] {
                self.mail_push_single(class, header, current_cpu);
                return;
            }

            let list_ptr = addr_of!(list.list) as *mut *mut Header;
            if likely(RseqCore.push(list_ptr, usage_ptr, rseq, current_cpu, header) == 1) {
                _mm_prefetch(list.list.cast_as_ptr() as *mut i8, _MM_HINT_T0);
                break;
            }

            if unlikely(loop_count > 5) {
                self.mail_push_single(class, header, current_cpu);
                return;
            }

            loop_count += 1;
            spin_loop();
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

            let list = &mut (*inner.cache.add(current_cpu)).cache[class];
            let list_ptr = addr_of!(list.list) as *mut *mut Header;
            let usage_ptr = &mut list.usage as *mut usize;

            let header = RseqCore.pop(list_ptr, usage_ptr, rseq, current_cpu);

            if unlikely(header as isize == -1) {
                if loop_count > 5 {
                    if last_core != current_cpu {
                        let mail = self.mail_pop_single(class, current_cpu);

                        if !mail.is_null() {
                            return mail;
                        }
                    }
                    last_core = current_cpu;
                    loop_count = 0;
                }
                loop_count += 1;
                continue;
            }

            return UnsafePointer::new(header);
        }
    }

    #[inline(never)]
    unsafe fn pop_non_inline(&self, class: usize) -> UnsafePointer<Header> {
        self.pop(class)
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
    #[inline(always)]
    pub unsafe fn try_pop(
        &self,
        class: usize,
        batch_size: usize,
        cpu_id: usize,
    ) -> (UnsafePointer<Header>, UnsafePointer<Header>, usize) {
        let ncpu = (*self.inner.get()).ncpu;

        if let Some(popped) = self.mail_pop(class, cpu_id, batch_size) {
            return (
                UnsafePointer::new(popped.0),
                UnsafePointer::new(popped.1),
                popped.2,
            );
        }

        for i in 1..ncpu + 1 {
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

    pub unsafe fn mail_pop_single(&self, class: usize, cpu_id: usize) -> UnsafePointer<Header> {
        let inner = &mut *self.inner.get();
        let list = &mut (*inner.cache.add(cpu_id)).mail[class];
        let list_ptr = &list.list;
        let usage_ptr = &list.usage;

        loop {
            list.trim_lock.spin_until_unlock();

            let old = list_ptr.load(Ordering::Relaxed);
            let head = unpack_ptr(old);

            if head.is_null() {
                return UnsafePointer::NULL;
            }

            let next = (*head).next;

            if list_ptr
                .compare_exchange(old, pack(next, old), Ordering::Acquire, Ordering::Relaxed)
                .is_ok()
            {
                usage_ptr.fetch_sub(1, Ordering::Relaxed);
                return UnsafePointer::new(head);
            }
        }
    }

    #[inline(always)]
    pub unsafe fn mail_push_batch(
        &self,
        class: usize,
        start: *mut Header,
        tail: *mut Header,
        batch_size: usize,
        cpu_id: usize,
    ) {
        let inner = &mut *self.inner.get();
        let list = &mut (*inner.cache.add(cpu_id)).mail[class];
        let list_ptr = &list.list;
        let usage_ptr = &list.usage;

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
                usage_ptr.fetch_add(batch_size, Ordering::Relaxed);
                return;
            }
        }
    }

    #[inline(always)]
    pub unsafe fn mail_push_single(&self, class: usize, header: *mut Header, cpu_id: usize) {
        let inner = &mut *self.inner.get();
        let list = &mut (*inner.cache.add(cpu_id)).mail[class];
        let list_ptr = &list.list;
        let usage_ptr = &list.usage;

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
                usage_ptr.fetch_add(1, Ordering::Relaxed);
                return;
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
        let usage_ptr = &list.usage;

        loop {
            list.trim_lock.spin_until_unlock();

            let old = list_ptr.load(Ordering::Relaxed);
            let head = unpack_ptr(old);

            if head.is_null() {
                return None;
            }

            let mut tail = head;
            let mut count = 1usize;
            let mut next = (*tail).next;

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
                usage_ptr.fetch_sub(count, Ordering::Relaxed);
                (*tail).next = null_mut();
                return Some((head, tail, count));
            }
        }
    }
}

pub static RSEQ_CACHE: RseqCache = RseqCache::new();

#[cfg(test)]
mod tests {
    use std::hint::black_box;

    use super::*;

    fn header() -> *mut Header {
        Box::into_raw(Box::new(Header {
            next: null_mut(),
            class: 0,
            magic: 0,
            life_time: 0,
            _padding: 0,
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
