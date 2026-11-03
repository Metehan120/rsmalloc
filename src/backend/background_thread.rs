use std::{
    sync::atomic::{AtomicBool, AtomicUsize, Ordering::Relaxed},
    thread::{Thread, park_timeout},
};

use syscalls::{
    Sysno::{self},
    syscall,
};

use crate::{
    AVERAGE_BLOCK_TIMES, CURRENT_STAMP, DISABLE_TRIM_THREAD, Messages,
    big_allocations::segmented_bitmap::{
        SEGMENTED_BITMAP_BACKEND, SEGMENTED_BITMAP_TOTAL_CACHED_VA,
    },
    global_vals::{
        BIG_TRIM_THRESHOLD, SEGMENTED_BITMAP_AVERAGE_BLOCK_TIMES, SMALL_TRIM_THRESHOLD,
        TOTAL_CACHED_VA,
    },
    internals::lock::{LockGuard, SpinLock},
    rseq_core::slab_cache::SLAB_CACHE,
    traits::Lock,
};

pub struct BackgroundThread {
    pub thread: SpinLock<Option<Thread>>,
    pub message: Messages,
}

pub static BACKGROUND_THREAD: BackgroundThread = BackgroundThread {
    thread: SpinLock::new(None),
    message: Messages::Trim {
        requested: AtomicBool::new(false),
        requested_size: AtomicUsize::new(0),
    },
};

pub static RECLAIM_GUARD: AtomicBool = AtomicBool::new(false);

const SIG_BLOCK: usize = 0;
const SIG_SETMASK: usize = 2;
// Linux x86-64 uses an 8-byte kernel signal set.
const GLIBC_INTERNAL_SIGNALS: u64 = (1 << (32 - 1)) | (1 << (33 - 1));
const WORKER_SIGNAL_MASK: u64 = !GLIBC_INTERNAL_SIGNALS;

struct SignalMaskGuard(u64);

impl SignalMaskGuard {
    fn block() -> Option<Self> {
        let signals = WORKER_SIGNAL_MASK;
        let mut previous = 0u64;
        unsafe {
            syscall!(
                Sysno::rt_sigprocmask,
                SIG_BLOCK,
                &signals as *const u64 as usize,
                &mut previous as *mut u64 as usize,
                size_of::<u64>()
            )
            .ok()?;
        }
        Some(Self(previous))
    }
}

impl Drop for SignalMaskGuard {
    fn drop(&mut self) {
        unsafe {
            let _ = syscall!(
                Sysno::rt_sigprocmask,
                SIG_SETMASK,
                &self.0 as *const u64 as usize,
                0usize,
                size_of::<u64>()
            );
        }
    }
}

#[cold]
#[inline(never)]
pub unsafe fn spawn(entry: unsafe fn() -> !) -> bool {
    let Some(_mask) = SignalMaskGuard::block() else {
        return false;
    };

    let thread = std::thread::Builder::new()
        .name("rs-maintenance".into())
        .stack_size(64 * 1024)
        .spawn(move || unsafe {
            entry();
        });

    let is_ok = thread.is_ok();
    if let Ok(thread) = thread {
        let mut lock = BACKGROUND_THREAD.thread.lock();
        *lock = Some(thread.thread().clone());
    }
    is_ok
}

pub fn maybe_unpark() -> bool {
    let LockGuard::Free(thread) = BACKGROUND_THREAD.thread.try_lock() else {
        return false;
    };

    if let Some(thread) = &*thread {
        thread.unpark();
        true
    } else {
        false
    }
}

pub unsafe fn maybe_start_background_reclaimer() {
    use std::sync::atomic::Ordering;

    if (TOTAL_CACHED_VA.load(Ordering::Relaxed) < SMALL_TRIM_THRESHOLD
        && SEGMENTED_BITMAP_TOTAL_CACHED_VA.load(Ordering::Relaxed) < BIG_TRIM_THRESHOLD)
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

#[cfg(feature = "debug")]
pub static TOTAL_TRIM_CALLS: AtomicUsize = AtomicUsize::new(0);
#[cfg(feature = "debug")]
pub static TOTAL_TRIMMED_VA: AtomicUsize = AtomicUsize::new(0);
#[cfg(feature = "debug-exact")]
pub static TOTAL_TRIMMED_BLOCKS: AtomicUsize = AtomicUsize::new(0);
#[cfg(feature = "debug-exact")]
pub static TOTAL_TRIMMED_TIME: AtomicUsize = AtomicUsize::new(0);

#[inline(never)]
pub unsafe fn background_reclaimer_main() -> ! {
    let mut latest_slab_stamp = 0;
    let mut latest_segmented_bitmap_stamp = 0;

    loop {
        use crate::get_clock;
        use std::time::Duration;

        park_timeout(Duration::from_millis(100));

        let trim = BACKGROUND_THREAD.message.get_trim();
        let trim_requested = trim.0.swap(false, Relaxed);
        let size = trim.1.swap(0, Relaxed);

        let stamp = (get_clock().elapsed().as_millis() / 100) as u32;
        CURRENT_STAMP.store(stamp, Relaxed);

        let mut trimmed = 0;
        if (stamp.saturating_sub(latest_segmented_bitmap_stamp)
            > SEGMENTED_BITMAP_AVERAGE_BLOCK_TIMES.load(Relaxed)
            && !DISABLE_TRIM_THREAD)
            || trim_requested
        {
            latest_segmented_bitmap_stamp = stamp;
            trimmed = SEGMENTED_BITMAP_BACKEND.trim_old(0);
        }

        if (stamp.saturating_sub(latest_slab_stamp) > AVERAGE_BLOCK_TIMES.load(Relaxed).max(30)
            && !DISABLE_TRIM_THREAD)
            || (trim_requested && (size == 0 || trimmed < size))
        {
            latest_slab_stamp = stamp;
            SLAB_CACHE.trim_small(0);
        }
    }
}
