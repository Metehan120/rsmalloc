#[cfg(feature = "debug-exact")]
use std::arch::x86_64::__rdtscp;
use std::{
    ffi::c_void,
    mem::forget,
    ptr::null_mut,
    sync::atomic::{
        AtomicBool, AtomicUsize,
        Ordering::{Acquire, Relaxed},
    },
};

use rustix::{
    mm::{Advice, madvise},
    system::sysinfo,
};

use crate::{
    AVERAGE_BLOCK_TIMES, CURRENT_STAMP, DISABLE_TRIM_THREAD, Flags, GLOBAL_TRIM_LOCK, Header, NCPU,
    big_allocations::buddy::BUDDY_BACKEND,
    core_prim::predictor::TRIM_SMOOTHING,
    global_vals::{TOTAL_CACHED_VA, TRIM_THRESHOLD},
    internals::lock::LockGuard,
    rseq_core::slab_cache::{SLAB_CACHE, Tagging},
    traits::Lock,
    utility::{NUM_SIZE_CLASSES, SIZE_CLASSES, get_size_4096_class},
};

pub static TRIM_GUARD: AtomicBool = AtomicBool::new(false);

#[cold]
#[inline(never)]
pub unsafe fn spawn(entry: unsafe fn() -> !) -> bool {
    std::thread::Builder::new()
        .name("rsmalloc-trimmer".into())
        .stack_size(64 * 1024)
        .spawn(move || unsafe {
            entry();
        })
        .is_ok()
}

pub unsafe fn maybe_start_trimmer() {
    use std::sync::atomic::Ordering;

    if TOTAL_CACHED_VA.load(Ordering::Relaxed) < TRIM_THRESHOLD
        || TRIM_GUARD.load(Ordering::Relaxed) == true
    {
        return;
    }

    if TRIM_GUARD
        .compare_exchange(false, true, Ordering::Release, Ordering::Acquire)
        .is_ok()
    {
        if !spawn(trimmer_main) {
            TRIM_GUARD.store(false, Ordering::Relaxed);
        }
    };
}

fn check_memory_pressure() -> usize {
    let info = sysinfo();

    let unit = info.mem_unit as usize;
    let total_ram = (info.totalram as usize).saturating_mul(unit);
    let free_ram = (info.freeram as usize).saturating_mul(unit);
    let total_swap = (info.totalswap as usize).saturating_mul(unit);
    let free_swap = (info.freeswap as usize).saturating_mul(unit);

    let total_available = free_ram + free_swap;
    let total_memory = total_ram + total_swap;

    if total_memory == 0 {
        return 50;
    }

    let used = total_memory.saturating_sub(total_available);
    (used * 100) / total_memory
}

const ENABLE_AFTER: usize = 2;
pub static DISABLE_BUDDY: AtomicBool = AtomicBool::new(false);
pub static mut BUDDY_DISABLE_PERCENTAGE: usize = 85;
pub static mut BUDDY_ENABLE_PERCENTAGE: usize = 80;
pub static mut DISABLE_RELIEF: bool = true;
pub static UNDER_AFTER: AtomicUsize = AtomicUsize::new(0);
#[cfg(feature = "debug")]
pub static TOTAL_TRIM_CALLS: AtomicUsize = AtomicUsize::new(0);
#[cfg(feature = "debug")]
pub static TOTAL_TRIMMED_VA: AtomicUsize = AtomicUsize::new(0);
#[cfg(feature = "debug-exact")]
pub static TOTAL_TRIMMED_BLOCKS: AtomicUsize = AtomicUsize::new(0);
#[cfg(feature = "debug-exact")]
pub static TOTAL_TRIMMED_TIME: AtomicUsize = AtomicUsize::new(0);

pub unsafe fn relief_paths() {
    let pressure = check_memory_pressure();

    if pressure >= BUDDY_DISABLE_PERCENTAGE && !DISABLE_BUDDY.load(Relaxed) {
        DISABLE_BUDDY.store(true, Relaxed);
        UNDER_AFTER.store(0, Relaxed);
        BUDDY_BACKEND.trim(0);

        return;
    }

    if DISABLE_BUDDY.load(Relaxed) && pressure <= BUDDY_ENABLE_PERCENTAGE {
        let under = UNDER_AFTER.fetch_add(1, Relaxed) + 1;

        if under >= ENABLE_AFTER {
            DISABLE_BUDDY.store(false, Relaxed);
            UNDER_AFTER.store(0, Relaxed);
        }
    } else {
        UNDER_AFTER.store(0, Relaxed);
    }
}

#[inline(never)]
pub unsafe fn trimmer_main() -> ! {
    let mut latest_stamp = 0;
    let mut total_elapsed = 0;

    loop {
        use crate::get_clock;
        use std::{thread::sleep, time::Duration};

        sleep(Duration::from_millis(100));

        total_elapsed += 100;
        if total_elapsed % 300 == 0 && !DISABLE_RELIEF {
            relief_paths();
        }

        let stamp = (get_clock().elapsed().as_millis() / 100) as u32;
        CURRENT_STAMP.store(stamp, Relaxed);

        if stamp.saturating_sub(latest_stamp) > AVERAGE_BLOCK_TIMES.load(Relaxed).max(30)
            && !DISABLE_TRIM_THREAD
        {
            use crate::big_allocations::buddy::BUDDY_BACKEND;
            latest_stamp = stamp;

            trim_small(0);
            BUDDY_BACKEND.trim_old(0);
        }
    }
}

const TRIM_REPUSH_BATCH: usize = 16;

pub unsafe fn trim_small(requested_size: usize) -> usize {
    if TOTAL_CACHED_VA.load(Relaxed) < TRIM_THRESHOLD {
        return 0;
    }

    let LockGuard::Free(_global_trim_guard) = GLOBAL_TRIM_LOCK.try_lock() else {
        return 0;
    };

    #[cfg(feature = "debug")]
    TOTAL_TRIM_CALLS.fetch_add(1, Relaxed);

    let mut total_trimmed = 0;
    let inner = SLAB_CACHE.get_inner();

    for cpu in 0..NCPU {
        for class in get_size_4096_class()..NUM_SIZE_CLASSES {
            let main_list = SLAB_CACHE.get_list(cpu, class);
            forget(main_list.trim_lock.lock());

            let output = {
                let mut list = main_list.list.load(Acquire);
                loop {
                    let (unpacked, tag) = Tagging.unpack_ptr(list);

                    if unpacked.is_null() {
                        break null_mut();
                    }

                    match main_list.list.compare_exchange(
                        list,
                        Tagging.pack(null_mut(), tag),
                        Acquire,
                        Relaxed,
                    ) {
                        Ok(_) => break unpacked,
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

            let mut trim_list = null_mut();
            let mut total_push = 0;
            let mut push_list_start = null_mut();
            let mut push_list = null_mut();

            let stamp = CURRENT_STAMP.load(Relaxed);
            let avg_life = TRIM_SMOOTHING[class].time(100) as u32;
            let mut next = output;
            while !next.is_null() {
                let old_next = (*next).next;
                let life_time = (*next).life_time;
                let mut is_push = false;

                if stamp.saturating_sub(life_time) > avg_life && (*next).flags == Flags::Allocated {
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

                if total_push == TRIM_REPUSH_BATCH && is_push {
                    SLAB_CACHE.transfer_push_batch(class, push_list, push_list_start, cpu, inner);
                    main_list.trim_lock.unlock();

                    total_push = 0;
                    push_list = null_mut();
                    push_list_start = null_mut();
                }

                next = old_next;
            }

            if total > 0 {
                let new_avg = (avg / total).clamp(1, 100);
                TRIM_SMOOTHING[class].update_refill(new_avg as usize, 1, 100);
            }

            if total_push > 0 {
                SLAB_CACHE.transfer_push_batch(class, push_list, push_list_start, cpu, inner);
            }

            main_list.trim_lock.unlock();

            while !trim_list.is_null() {
                #[cfg(feature = "debug-exact")]
                TOTAL_TRIMMED_BLOCKS.fetch_add(1, Relaxed);
                let next = (*trim_list).next;
                let mut did_trim = false;
                if requested_size == 0 || total_trimmed < requested_size {
                    #[cfg(feature = "debug-exact")]
                    let mut aux = 0;
                    #[cfg(feature = "debug-exact")]
                    let start_of = __rdtscp(&mut aux);

                    let is_ok = release_memory(trim_list, SIZE_CLASSES[class]);
                    if is_ok {
                        #[cfg(feature = "debug")]
                        TOTAL_TRIMMED_VA.fetch_add(SIZE_CLASSES[class], Relaxed);
                        (*trim_list).flags = Flags::Trimmed;
                        total_trimmed += SIZE_CLASSES[class];
                        did_trim = true;
                    }

                    #[cfg(feature = "debug-exact")]
                    let current = __rdtscp(&mut aux);
                    #[cfg(feature = "debug-exact")]
                    let elapsed = current - start_of;

                    #[cfg(feature = "debug-exact")]
                    TOTAL_TRIMMED_TIME.fetch_add(elapsed as usize, Relaxed);
                }
                (*trim_list).life_time = stamp;
                if did_trim {
                    SLAB_CACHE.transfer_push_single_trimmed(class, trim_list, cpu, inner);
                } else {
                    SLAB_CACHE.transfer_push_single(class, trim_list, cpu, inner);
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

#[inline]
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
