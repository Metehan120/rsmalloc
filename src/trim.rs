use std::{
    ffi::c_void,
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
    ALLOCATED_FLAG, AVERAGE_BLOCK_TIMES, CURRENT_STAMP, DISABLE_TRIM_THREAD, GLOBAL_TRIM_LOCK,
    Header, TRIMMED_FLAG,
    big_allocations::buddy::BIG_BUDDY_ALLOCATOR,
    rseq_core::rseq_cache::{RSEQ_CACHE, pack, unpack_ptr},
    utility::{NUM_SIZE_CLASSES, SIZE_CLASSES, get_size_4096_class},
};

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

pub unsafe fn relief_paths() {
    let pressure = check_memory_pressure();

    if pressure >= BUDDY_DISABLE_PERCENTAGE && !DISABLE_BUDDY.load(Relaxed) {
        DISABLE_BUDDY.store(true, Relaxed);
        UNDER_AFTER.store(0, Relaxed);
        BIG_BUDDY_ALLOCATOR.trim(0);

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

// Leave it at around 16-32
const TRIM_REPUSH_BATCH: usize = 16;

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

        let stamp = get_clock().elapsed().as_millis() as u32;
        CURRENT_STAMP.store(stamp, Relaxed);

        if stamp.saturating_sub(latest_stamp) > AVERAGE_BLOCK_TIMES.load(Relaxed)
            && !DISABLE_TRIM_THREAD
        {
            use crate::big_allocations::buddy::BIG_BUDDY_ALLOCATOR;
            latest_stamp = stamp;

            trim_small(0);
            BIG_BUDDY_ALLOCATOR.trim_old(0);
        }
    }
}

pub unsafe fn trim_small(requested_size: usize) -> usize {
    let Some(_global_trim_guard) = GLOBAL_TRIM_LOCK.try_lock() else {
        return 0;
    };

    let ncpu = RSEQ_CACHE.get_ncpu();
    let mut total_trimmed = 0;

    for cpu in 0..ncpu {
        for class in get_size_4096_class()..NUM_SIZE_CLASSES {
            let main_list = RSEQ_CACHE.get_list(cpu, class);
            let output = {
                let mut list = main_list.list.load(Acquire);
                loop {
                    let unpacked = unpack_ptr(list);

                    if unpacked.is_null() {
                        break null_mut();
                    }

                    match main_list.list.compare_exchange(
                        list,
                        pack(null_mut(), list),
                        Acquire,
                        Relaxed,
                    ) {
                        Ok(_) => break unpacked,
                        Err(new) => list = new,
                    }
                }
            };

            if output.is_null() {
                continue;
            }

            let mut avg: u32 = 0;
            let mut total = 0;

            let mut trim_list = null_mut();
            let mut total_push = 0;
            let mut push_list_start = null_mut();
            let mut push_list = null_mut();

            let stamp = CURRENT_STAMP.load(Relaxed);
            let avg_life = AVERAGE_BLOCK_TIMES.load(Relaxed);
            let mut next = output;
            while !next.is_null() {
                let old_next = (*next).next;
                let life_time = (*next).life_time;
                let mut is_push = false;

                if stamp.saturating_sub(life_time) > avg_life && (*next).flags == ALLOCATED_FLAG {
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
                    RSEQ_CACHE.mail_push_batch(class, push_list, push_list_start, cpu);
                    total_push = 0;
                    push_list = null_mut();
                    push_list_start = null_mut();
                }

                next = old_next;
            }

            if total > 0 {
                let avg = (avg / total).clamp(100, 10000).saturating_add(10);
                AVERAGE_BLOCK_TIMES.store(avg, Relaxed);
            }

            if total_push > 0 {
                RSEQ_CACHE.mail_push_batch(class, push_list, push_list_start, cpu);
            }

            while !trim_list.is_null() {
                let next = (*trim_list).next;
                if requested_size == 0 || total_trimmed < requested_size {
                    let is_ok = release_memory(trim_list, SIZE_CLASSES[class]);
                    if is_ok {
                        (*trim_list).flags = TRIMMED_FLAG;
                        total_trimmed += SIZE_CLASSES[class];
                    }
                }
                (*trim_list).life_time = stamp;
                RSEQ_CACHE.mail_push_single(class, trim_list, cpu);
                trim_list = next;
            }
        }
    }

    total_trimmed
}

#[inline]
fn release_memory(header_ptr: *mut Header, size: usize) -> bool {
    unsafe {
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

        #[cfg(feature = "lazy-page-trim")]
        {
            madvise(page_start as *mut c_void, length, Advice::LinuxFree).is_ok()
        }
        #[cfg(not(feature = "lazy-page-trim"))]
        {
            madvise(page_start as *mut c_void, length, Advice::LinuxDontNeed).is_ok()
        }
    }
}
