use std::{
    cell::UnsafeCell,
    hint::{likely, unlikely},
    ptr::{addr_of, null_mut},
    sync::atomic::{AtomicPtr, AtomicUsize, Ordering},
};

use rustix::mm::{MapFlags, ProtFlags, mmap_anonymous};

use crate::{
    GenericCache, Header, RseqCoreTrait,
    core_prim::wrappers::UnsafePointer,
    internals::{lock::SerialLock, once::Once},
    rseq_core::{rseq_core::RseqCore, rseq_main::get_rseq},
    utility::{NUM_SIZE_CLASSES, RSEQ_MAX_BLOCKS},
};

pub struct ClassCache {
    list: UnsafePointer<Header>,
    usage: usize,
}

pub struct MailCache {
    list: AtomicPtr<Header>,
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

// TODO: Detect CPU count during initialization
unsafe fn get_max_cpu() -> u32 {
    256
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
            let list = &mut (*inner.cache.add(rseq.cpu_id as usize)).cache[class];
            let usage_ptr = &mut list.usage;
            if *usage_ptr > RSEQ_MAX_BLOCKS[class] {
                self.mail_push_batch(class, header, tail, batch_size, rseq.cpu_id as usize);
                return;
            }

            let list_ptr = addr_of!(list.list) as *mut *mut Header;
            if likely(
                RseqCore.push_tailed(list_ptr, usage_ptr, rseq, header, tail, batch_size) == 1,
            ) {
                break;
            } else {
                if unlikely(loop_count > 2500) {
                    self.mail_push_batch(class, header, tail, batch_size, rseq.cpu_id as usize);
                    return;
                }
                loop_count += 1;
            }
        }
    }

    #[inline(always)]
    unsafe fn push(&self, class: usize, header: *mut Header) {
        let inner = &mut *self.inner.get();
        let rseq = get_rseq();
        let mut loop_count = 0;

        loop {
            let list = &mut (*inner.cache.add(rseq.cpu_id as usize)).cache[class];
            let usage_ptr = &mut list.usage;
            if *usage_ptr > RSEQ_MAX_BLOCKS[class] {
                self.mail_push_single(class, header, rseq.cpu_id as usize);
                return;
            }

            let list_ptr = addr_of!(list.list) as *mut *mut Header;
            if likely(RseqCore.push(list_ptr, usage_ptr, rseq, header) == 1) {
                break;
            } else {
                if unlikely(loop_count > 2500) {
                    self.mail_push_single(class, header, rseq.cpu_id as usize);
                    return;
                }
                loop_count += 1;
            }
        }
    }

    #[inline(always)]
    unsafe fn pop(&self, class: usize) -> UnsafePointer<Header> {
        let inner = &mut *self.inner.get();
        let rseq = get_rseq();

        loop {
            let list = &mut (*inner.cache.add(rseq.cpu_id as usize)).cache[class];
            let list_ptr = addr_of!(list.list) as *mut *mut Header;
            let usage_ptr = &mut list.usage as *mut usize;

            let header = RseqCore.pop(list_ptr, usage_ptr, rseq);

            if unlikely(header as isize == -1) {
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

// TODO: Add ABA safety for MailCache
impl RseqCache {
    #[inline(always)]
    pub unsafe fn try_pop(
        &self,
        class: usize,
        batch_size: usize,
    ) -> (UnsafePointer<Header>, UnsafePointer<Header>, usize) {
        let cpu_id = get_rseq().cpu_id as usize;
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
            (*tail).next = old;

            if list_ptr
                .compare_exchange_weak(old, start, Ordering::Release, Ordering::Relaxed)
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
            (*header).next = old;

            if list_ptr
                .compare_exchange_weak(old, header, Ordering::Release, Ordering::Relaxed)
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

            let head = list_ptr.load(Ordering::Relaxed);
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

            if list_ptr
                .compare_exchange_weak(head, next, Ordering::Acquire, Ordering::Relaxed)
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
