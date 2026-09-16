use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering::Relaxed};

use rustix::system::sysinfo;

use crate::{
    AVERAGE_BLOCK_TIMES, CURRENT_STAMP, DISABLE_TRIM_THREAD,
    big_allocations::segmented_bitmap::SEGMENTED_BITMAP_BACKEND,
    global_vals::{SMALL_TRIM_THRESHOLD, TOTAL_CACHED_VA},
    rseq_core::slab_cache::SLAB_CACHE,
};

pub static RECLAIM_GUARD: AtomicBool = AtomicBool::new(false);

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

pub unsafe fn maybe_start_background_reclaimer() {
    use std::sync::atomic::Ordering;

    if TOTAL_CACHED_VA.load(Ordering::Relaxed) < SMALL_TRIM_THRESHOLD
        || RECLAIM_GUARD.load(Ordering::Relaxed) == true
    {
        return;
    }

    if RECLAIM_GUARD
        .compare_exchange(false, true, Ordering::Release, Ordering::Acquire)
        .is_ok()
    {
        if !spawn(background_reclaimer_main) {
            RECLAIM_GUARD.store(false, Ordering::Relaxed);
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
pub static DISABLE_SEGMENTED_BITMAP: AtomicBool = AtomicBool::new(false);
pub static mut SEGMENTED_BITMAP_DISABLE_PERCENTAGE: usize = 85;
pub static mut SEGMENTED_BITMAP_ENABLE_PERCENTAGE: usize = 80;
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

    if pressure >= SEGMENTED_BITMAP_DISABLE_PERCENTAGE && !DISABLE_SEGMENTED_BITMAP.load(Relaxed) {
        DISABLE_SEGMENTED_BITMAP.store(true, Relaxed);
        UNDER_AFTER.store(0, Relaxed);
        SEGMENTED_BITMAP_BACKEND.trim(0);

        return;
    }

    if DISABLE_SEGMENTED_BITMAP.load(Relaxed) && pressure <= SEGMENTED_BITMAP_ENABLE_PERCENTAGE {
        let under = UNDER_AFTER.fetch_add(1, Relaxed) + 1;

        if under >= ENABLE_AFTER {
            DISABLE_SEGMENTED_BITMAP.store(false, Relaxed);
            UNDER_AFTER.store(0, Relaxed);
        }
    } else {
        UNDER_AFTER.store(0, Relaxed);
    }
}

#[inline(never)]
pub unsafe fn background_reclaimer_main() -> ! {
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
            use crate::big_allocations::segmented_bitmap::SEGMENTED_BITMAP_BACKEND;
            latest_stamp = stamp;

            SLAB_CACHE.trim_small(0);
            SEGMENTED_BITMAP_BACKEND.trim_old(0);
        }
    }
}
