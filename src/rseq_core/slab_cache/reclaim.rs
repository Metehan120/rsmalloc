use std::{
    mem::forget,
    os::raw::c_void,
    ptr::null_mut,
    sync::atomic::Ordering::{Acquire, Relaxed},
};

use rustix::mm::{Advice, madvise};

#[cfg(feature = "debug")]
use crate::backend::reclaimer::{TOTAL_TRIM_CALLS, TOTAL_TRIMMED_VA};
use crate::{
    Flags, Header,
    big_allocations::segmented_bitmap::SEGMENTED_BITMAP_TOTAL_CACHED_VA,
    core_prim::predictor::TRIM_SMOOTHING,
    global_vals::{
        AVERAGE_BLOCK_TIMES, BIG_TRIM_THRESHOLD, CURRENT_STAMP, GLOBAL_TRIM_LOCK, NCPU,
        SMALL_TRIM_THRESHOLD, TOTAL_CACHED_VA,
    },
    internals::lock::LockGuard,
    rseq_core::{aba::Tagging, slab_cache::SlabCache},
    traits::Lock,
    utility::{ITERATIONS, NUM_SIZE_CLASSES, SIZE_CLASSES, get_size_4096_class},
};
#[cfg(feature = "debug-exact")]
use crate::{
    backend::reclaimer::{TOTAL_TRIMMED_BLOCKS, TOTAL_TRIMMED_TIME},
    core_prim::hw::HardwareFeature,
};

impl SlabCache {
    pub unsafe fn trim_small(&self, requested_size: usize) -> usize {
        if TOTAL_CACHED_VA.load(Relaxed) < SMALL_TRIM_THRESHOLD
            && SEGMENTED_BITMAP_TOTAL_CACHED_VA.load(Relaxed) < BIG_TRIM_THRESHOLD
        {
            return 0;
        }

        let LockGuard::Free(_global_trim_guard) = GLOBAL_TRIM_LOCK.try_lock() else {
            return 0;
        };

        #[cfg(feature = "debug")]
        TOTAL_TRIM_CALLS.fetch_add(1, Relaxed);

        let mut total_trimmed = 0;
        let inner = self.get_inner();

        for cpu in 0..NCPU {
            for class in get_size_4096_class()..NUM_SIZE_CLASSES {
                let main_list = self.get_list(cpu, class);
                forget(main_list.trim_lock.lock());

                let output = {
                    let mut list = main_list.list.load(Acquire);
                    loop {
                        let pack = Tagging.untag_ptr(list);

                        if pack.current_header.is_null() {
                            break null_mut();
                        }

                        match main_list.list.compare_exchange(
                            list,
                            Tagging.tag_ptr(null_mut(), pack.old_packed),
                            Acquire,
                            Relaxed,
                        ) {
                            Ok(_) => break pack.current_header,
                            Err(new) => list = new,
                        }
                    }
                };

                if output.is_null() {
                    if !cfg!(feature = "trim-aggressively") {
                        TRIM_SMOOTHING[class].update_refill(100, 1, 100);
                    }
                    main_list.trim_lock.unlock();
                    continue;
                }

                let mut avg: u32 = 0;
                let mut total = 0;
                #[cfg(feature = "debug-exact")]
                let mut total_popped = 0;

                let mut trim_list = null_mut();
                let mut total_push = 0;
                let mut push_list_start = null_mut();
                let mut push_list = null_mut();
                let stamp = CURRENT_STAMP.load(Relaxed);
                let avg_life = TRIM_SMOOTHING[class].time(100) as u32;

                let mut next = output;
                while !next.is_null() {
                    #[cfg(feature = "debug-exact")]
                    {
                        total_popped += 1;
                    }
                    let old_next = (*next).next;
                    let life_time = (*next).life_time;
                    let mut is_push = false;
                    if stamp.saturating_sub(life_time) > avg_life
                        && (*next).flags == Flags::Allocated
                    {
                        (*next).next = trim_list;
                        trim_list = next;
                    } else {
                        (*next).next = push_list;
                        push_list = next;
                        if total_push == 0 {
                            push_list_start = next;
                        }
                        total_push += 1;
                        is_push = true;
                    }

                    if life_time != 0 {
                        avg = avg.saturating_add(stamp.saturating_sub(life_time));
                        total += 1;
                    }

                    if total_push == ITERATIONS[class] + 1 && is_push {
                        self.transfer_push_batch(
                            class,
                            push_list,
                            push_list_start,
                            #[cfg(feature = "debug-exact")]
                            total_push,
                            cpu,
                            inner,
                        );
                        main_list.trim_lock.unlock();

                        total_push = 0;
                        push_list = null_mut();
                        push_list_start = null_mut();
                    }

                    next = old_next;
                }

                crate::global_vals::record_transfer_pop!(class, total_popped);

                if total > 0 {
                    let new_avg = (avg / total).clamp(1, 100);
                    TRIM_SMOOTHING[class].update_refill(new_avg as usize, 1, 100);
                }

                if total_push > 0 {
                    self.transfer_push_batch(
                        class,
                        push_list,
                        push_list_start,
                        #[cfg(feature = "debug-exact")]
                        total_push,
                        cpu,
                        inner,
                    );
                }
                main_list.trim_lock.unlock();

                while !trim_list.is_null() {
                    #[cfg(feature = "debug-exact")]
                    TOTAL_TRIMMED_BLOCKS.fetch_add(1, Relaxed);
                    let next = (*trim_list).next;
                    let mut did_trim = false;
                    if requested_size == 0 || total_trimmed < requested_size {
                        #[cfg(feature = "debug-exact")]
                        let mut start_of = HardwareFeature::new_cycle_clock();

                        let is_ok = Self::release_memory(trim_list, SIZE_CLASSES[class]);
                        if is_ok {
                            #[cfg(feature = "debug")]
                            TOTAL_TRIMMED_VA.fetch_add(SIZE_CLASSES[class], Relaxed);
                            (*trim_list).flags = Flags::Reclaimed;
                            total_trimmed += SIZE_CLASSES[class];
                            did_trim = true;
                        }

                        #[cfg(feature = "debug-exact")]
                        let elapsed = start_of.elapsed();

                        #[cfg(feature = "debug-exact")]
                        TOTAL_TRIMMED_TIME.fetch_add(elapsed as usize, Relaxed);
                    }
                    (*trim_list).life_time = stamp;
                    if did_trim {
                        self.transfer_push_single_trimmed(class, trim_list, cpu, inner);
                    } else {
                        self.transfer_push_single(class, trim_list, cpu, inner);
                    }
                    trim_list = next;
                }
            }
        }

        let mut global_avg: u64 = 0;
        let mut global_count: u64 = 0;
        for class in get_size_4096_class()..NUM_SIZE_CLASSES {
            global_avg += TRIM_SMOOTHING[class].time(100) as u64;
            global_count += 1;
        }

        if global_count > 0 {
            AVERAGE_BLOCK_TIMES.store((global_avg / global_count) as u32, Relaxed);
        }

        total_trimmed
    }

    unsafe fn release_memory(header_ptr: *mut Header, size: usize) -> bool {
        const PAGE_SIZE: usize = 4096;
        const PAGE_MASK: usize = !(PAGE_SIZE - 1);

        let header = header_ptr as usize;
        let user_start = header + Header::SIZE;
        let user_end = user_start + size;

        let page_start = (user_start + PAGE_SIZE - 1) & PAGE_MASK;
        let page_end = user_end & PAGE_MASK;

        if page_start >= page_end {
            return false;
        }
        let length = page_end - page_start;

        if cfg!(feature = "lazy-page-trim") {
            madvise(page_start as *mut c_void, length, Advice::LinuxFree).is_ok()
        } else {
            madvise(page_start as *mut c_void, length, Advice::LinuxDontNeed).is_ok()
        }
    }
}
