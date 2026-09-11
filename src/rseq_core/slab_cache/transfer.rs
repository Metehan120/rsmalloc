use std::{hint::spin_loop, ptr::eq, sync::atomic::Ordering};

use portable_atomic::AtomicU128;

use crate::{
    Header,
    core_prim::hw::{HardwareFeature, PrefetchHint, SafeToPrefetch},
    rseq_core::{
        aba::Tagging,
        slab_cache::{SlabCache, SlabCacheInner, TransferReturn},
    },
    traits::Lock,
};

impl SlabCache {
    #[inline(always)]
    pub unsafe fn try_pop(
        &self,
        class: usize,
        batch_size: usize,
        cpu_id: usize,
    ) -> Option<TransferReturn> {
        let inner = &*self.inner.get();

        if !self.is_empty(inner, class, cpu_id) {
            if let Some(popped) = self.transfer_pop_batch(class, cpu_id, batch_size) {
                return Some(popped);
            }
        }

        self.pop_slow(inner, class, cpu_id, batch_size)
    }

    #[inline(always)]
    unsafe fn pop_slow(
        &self,
        inner: &SlabCacheInner,
        class: usize,
        cpu_id: usize,
        batch_size: usize,
    ) -> Option<TransferReturn> {
        let (start, end, node_id) = if inner.is_numa {
            self.numa_cpu(&inner, cpu_id)
        } else {
            (0, inner.numa.ncpu - 1, 0)
        };

        if let Some(block) =
            self.first_nonempty_cpu_in_range(&inner, class, cpu_id, batch_size, start, end)
        {
            #[cfg(feature = "transfer-debug")]
            crate::TOTAL_TRANSFER_STEALS.fetch_add(1, Ordering::Relaxed);
            return Some(block);
        }

        if inner.is_numa {
            if let Some(numa_block) =
                self.slowest_numa_steal_path(class, &inner, cpu_id, node_id, batch_size)
            {
                return Some(numa_block);
            }
        }

        #[cfg(feature = "transfer-debug")]
        crate::DRY_TRANSFER_STEALS.fetch_add(1, Ordering::Relaxed);

        None
    }

    #[cold]
    #[inline(never)]
    pub unsafe fn slowest_numa_steal_path(
        &self,
        class: usize,
        inner: &SlabCacheInner,
        cpu_id: usize,
        node_id: u16,
        batch_size: usize,
    ) -> Option<TransferReturn> {
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
                return Some(block);
            }
        }

        None
    }

    #[inline(always)]
    pub unsafe fn transfer_push_batch(
        &self,
        class: usize,
        start: *mut Header,
        tail: *mut Header,
        #[cfg(feature = "debug-exact")] batch_size: usize,
        cpu_id: usize,
        inner: &SlabCacheInner,
    ) {
        #[cfg(feature = "transfer-debug-exact")]
        crate::TOTAL_TRANSFER_PUSH_CALLS.fetch_add(1, Ordering::Relaxed);

        let list = &inner.cache.get_offset(cpu_id).mail[class];
        let list_ptr = &list.list;

        loop {
            let old = list_ptr.load(Ordering::Relaxed);
            let pack = Tagging.untag_ptr(old);

            (*tail).next = pack.current_header;

            if list_ptr
                .compare_exchange(
                    old,
                    Tagging.tag_ptr(start, pack.old_packed),
                    Ordering::Release,
                    Ordering::Relaxed,
                )
                .is_ok()
            {
                if pack.current_header.is_null() {
                    self.mark_class_nonempty(inner, class, cpu_id);
                }
                crate::global_vals::record_transfer_push!(class, batch_size);
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
        inner: &SlabCacheInner,
    ) {
        let list = &inner.cache.get_offset(cpu_id).mail[class];
        let list_ptr = &list.list;

        self.transfer_push_single_to(list_ptr, class, header, cpu_id, inner);
    }

    pub unsafe fn transfer_push_single_trimmed(
        &self,
        class: usize,
        header: *mut Header,
        cpu_id: usize,
        inner: &SlabCacheInner,
    ) {
        let list = &inner.cache.get_offset(cpu_id).mail[class];
        let list_ptr = &list.trimmed;

        self.transfer_push_single_to(list_ptr, class, header, cpu_id, inner);
    }

    #[inline(always)]
    pub unsafe fn transfer_push_single_to(
        &self,
        list_ptr: &AtomicU128,
        class: usize,
        header: *mut Header,
        cpu_id: usize,
        inner: &SlabCacheInner,
    ) {
        #[cfg(feature = "transfer-debug-exact")]
        crate::TOTAL_TRANSFER_PUSH_CALLS.fetch_add(1, Ordering::Relaxed);

        loop {
            let old = list_ptr.load(Ordering::Relaxed);
            let pack = Tagging.untag_ptr(old);

            (*header).next = pack.current_header;
            if list_ptr
                .compare_exchange(
                    old,
                    Tagging.tag_ptr(header, pack.old_packed),
                    Ordering::Release,
                    Ordering::Relaxed,
                )
                .is_ok()
            {
                if pack.current_header.is_null() {
                    self.mark_class_nonempty(inner, class, cpu_id);
                }
                crate::global_vals::record_transfer_push!(class, 1);
                return;
            }

            #[cfg(feature = "transfer-debug")]
            crate::TOTAL_TRANSFER_RETRIES.fetch_add(1, Ordering::Relaxed);

            spin_loop();
        }
    }

    #[inline(never)]
    unsafe fn clear_hint(
        &self,
        ptr: &AtomicU128,
        other_ptr: &AtomicU128,
        inner: &SlabCacheInner,
        class: usize,
        cpu_id: usize,
    ) {
        self.clear_class_hint(inner, class, cpu_id);

        if !Tagging
            .untag_ptr(ptr.load(Ordering::Acquire))
            .current_header
            .is_null()
            || !Tagging
                .untag_ptr(other_ptr.load(Ordering::Acquire))
                .current_header
                .is_null()
        {
            self.mark_class_nonempty(inner, class, cpu_id);
        }
    }

    #[inline(always)]
    pub unsafe fn transfer_pop_batch(
        &self,
        class: usize,
        cpu_id: usize,
        batch_size: usize,
    ) -> Option<TransferReturn> {
        #[cfg(feature = "transfer-debug-exact")]
        crate::TOTAL_TRANSFER_POP_CALLS.fetch_add(1, Ordering::Relaxed);

        let inner = self.get_inner();
        let list = &inner.cache.get_offset(cpu_id).mail[class];
        let normal_ptr = &list.list;
        let trimmed_ptr = &list.trimmed;
        let mut list_ptr = normal_ptr;

        'retry: loop {
            let mut old = list_ptr.load(Ordering::Acquire);
            let mut pack = Tagging.untag_ptr(old);

            if list.trim_lock.get_lock() {
                loop {
                    old = list_ptr.load(Ordering::Acquire);
                    pack = Tagging.untag_ptr(old);
                    if !pack.current_header.is_null() {
                        break;
                    }
                    if !list.trim_lock.get_lock() {
                        continue 'retry;
                    }
                    spin_loop();
                }
            }

            if pack.current_header.is_null() {
                if eq(list_ptr, normal_ptr) {
                    list_ptr = &trimmed_ptr;
                    continue;
                }
                self.clear_hint(normal_ptr, trimmed_ptr, inner, class, cpu_id);
                return None;
            }

            let mut tail = pack.current_header;
            let mut count = 1usize;
            let mut next = (*tail).next;
            while count < batch_size && !next.is_null() {
                tail = next;
                next = (*tail).next;
                count += 1;
            }

            if list_ptr
                .compare_exchange(
                    old,
                    Tagging.tag_ptr(next, pack.old_packed),
                    Ordering::Acquire,
                    Ordering::Relaxed,
                )
                .is_ok()
            {
                if !next.is_null() {
                    HardwareFeature.prefetch(SafeToPrefetch::new(next), PrefetchHint::PreferL1)
                } else {
                    self.clear_hint(normal_ptr, trimmed_ptr, inner, class, cpu_id);
                }
                crate::global_vals::record_transfer_pop!(class, count);
                return Some(TransferReturn {
                    start: pack.current_header,
                    end: tail,
                    total: count,
                });
            }

            #[cfg(feature = "transfer-debug")]
            crate::TOTAL_TRANSFER_RETRIES.fetch_add(1, Ordering::Relaxed);

            spin_loop();
        }
    }
}
