use std::{
    sync::atomic::{AtomicU32, AtomicUsize, Ordering},
    time::Instant,
};

use crate::internals::{lock::SpinLock, oncelock::OnceLock};

#[cfg(not(feature = "extended-header"))]
pub static mut MAGIC: u16 = u16::from_le_bytes(*b"RS");
#[cfg(not(feature = "extended-header"))]
pub static mut FREED_MAGIC: u16 = u16::from_le_bytes(*b"RM");
#[cfg(not(feature = "extended-header"))]
pub static mut BIG_MAGIC: u16 = u16::from_le_bytes(*b"RB");

#[cfg(feature = "extended-header")]
pub static mut MAGIC: u64 = u64::from_le_bytes(*b"RSMAGICS");
#[cfg(feature = "extended-header")]
pub static mut FREED_MAGIC: u64 = u64::from_le_bytes(*b"RMMAGICF");
#[cfg(feature = "extended-header")]
pub static mut BIG_MAGIC: u64 = u64::from_le_bytes(*b"RBMAGICB");

pub static mut RS_DISABLE_THP: bool = false;
pub static mut BUDDY_INIT: bool = false;
pub static mut BUDDY_MAX_CACHE: usize = 0;
pub static mut BUDDY_ATTEMPT_HUGE: bool = false;
#[cfg(not(feature = "preload"))]
pub static mut FOREIGN_POINTER_ABORT: bool = false;
pub static mut ALIGN_TAG: usize = usize::from_le_bytes(*b"RSMALIGN");
pub static mut DISABLE_TRIM_THREAD: bool = false;
pub static mut TRIM_THRESHOLD: usize = 1024 * 1024 * 10;

pub static TOTAL_CACHED_VA: AtomicUsize = AtomicUsize::new(0);
#[cfg(feature = "debug")]
pub static HIGH_WATER_SLAB_CACHED_VA: AtomicUsize = AtomicUsize::new(0);
#[cfg(feature = "debug")]
pub static HIGH_WATER_BUDDY_CACHED_VA: AtomicUsize = AtomicUsize::new(0);
#[cfg(feature = "debug")]
pub static HIGH_WATER_TOTAL_CACHED_VA: AtomicUsize = AtomicUsize::new(0);

#[cfg(feature = "debug")]
#[inline(always)]
pub fn update_high_water(max: &AtomicUsize, value: usize) {
    let mut current = max.load(Ordering::Relaxed);
    while value > current {
        match max.compare_exchange_weak(current, value, Ordering::Relaxed, Ordering::Relaxed) {
            Ok(_) => break,
            Err(observed) => current = observed,
        }
    }
}

#[inline(always)]
pub fn add_slab_cached_va(bytes: usize) {
    let _slab = TOTAL_CACHED_VA
        .fetch_add(bytes, Ordering::Relaxed)
        .saturating_add(bytes);

    #[cfg(feature = "debug")]
    {
        let buddy = crate::big_allocations::buddy::BUDDY_TOTAL_CACHED_VA.load(Ordering::Relaxed);
        update_high_water(&HIGH_WATER_SLAB_CACHED_VA, _slab);
        update_high_water(&HIGH_WATER_TOTAL_CACHED_VA, _slab.saturating_add(buddy));
    }
}

#[inline(always)]
pub fn add_buddy_cached_va(bytes: usize) {
    let _buddy = crate::big_allocations::buddy::BUDDY_TOTAL_CACHED_VA
        .fetch_add(bytes, Ordering::Relaxed)
        .saturating_add(bytes);

    #[cfg(feature = "debug")]
    {
        let slab = TOTAL_CACHED_VA.load(Ordering::Relaxed);
        update_high_water(&HIGH_WATER_BUDDY_CACHED_VA, _buddy);
        update_high_water(&HIGH_WATER_TOTAL_CACHED_VA, slab.saturating_add(_buddy));
    }
}

#[cfg(feature = "debug")]
pub static REFILL_UNDER_PREDICTS: AtomicUsize = AtomicUsize::new(0);
#[cfg(feature = "debug")]
pub static REFILL_OVER_PREDICTS: AtomicUsize = AtomicUsize::new(0);
#[cfg(feature = "debug")]
pub static TOTAL_REFILL_CALLS: AtomicUsize = AtomicUsize::new(0);
#[cfg(feature = "debug")]
pub static REFILLS_BY_CLASS: [AtomicUsize; crate::utility::NUM_SIZE_CLASSES] =
    [const { AtomicUsize::new(0) }; crate::utility::NUM_SIZE_CLASSES];

#[cfg(feature = "debug")]
pub static ABORTS: AtomicUsize = AtomicUsize::new(0);
#[cfg(feature = "debug")]
pub static TOTAL_MMAP_CALLS: AtomicUsize = AtomicUsize::new(0);
#[cfg(feature = "debug")]
pub static TOTAL_MMAP_BYTES: AtomicUsize = AtomicUsize::new(0);

#[inline(always)]
pub fn record_mmap_call(bytes: usize) {
    #[cfg(not(feature = "debug"))]
    let _ = bytes;

    #[cfg(feature = "debug")]
    {
        TOTAL_MMAP_CALLS.fetch_add(1, Ordering::Relaxed);
        TOTAL_MMAP_BYTES.fetch_add(bytes, Ordering::Relaxed);
    }
}

#[cfg(feature = "debug-exact")]
pub static GLOBAL_LOCKS: AtomicUsize = AtomicUsize::new(0);
#[cfg(feature = "debug-exact")]
pub static GLOBAL_LOCK_RETRIES: AtomicUsize = AtomicUsize::new(0);
#[cfg(feature = "debug-exact")]
pub static GLOBAL_TRY_LOCKS: AtomicUsize = AtomicUsize::new(0);
#[cfg(feature = "debug-exact")]
pub static GLOBAL_TRY_LOCK_MISSES: AtomicUsize = AtomicUsize::new(0);
#[cfg(feature = "debug-exact")]
pub static GLOBAL_SPIN_WAITS: AtomicUsize = AtomicUsize::new(0);

pub static TIME_STAMP: OnceLock<Instant> = OnceLock::new();
pub static CURRENT_STAMP: AtomicU32 = AtomicU32::new(0);
pub static AVERAGE_BLOCK_TIMES: AtomicU32 = AtomicU32::new(10);
pub static BUDDY_AVERAGE_BLOCK_TIMES: AtomicU32 = AtomicU32::new(100);
pub static GLOBAL_TRIM_LOCK: SpinLock<()> = SpinLock::new(());
pub static mut NCPU: usize = 0;

#[cfg(feature = "transfer-debug")]
pub static TOTAL_TRANSFER_STEALS: AtomicUsize = AtomicUsize::new(0);
#[cfg(feature = "transfer-debug")]
pub static TOTAL_TRANSFER_RETRIES: AtomicUsize = AtomicUsize::new(0);
#[cfg(feature = "transfer-debug")]
pub static DRY_TRANSFER_STEALS: AtomicUsize = AtomicUsize::new(0);
#[cfg(feature = "transfer-debug-exact")]
pub static TOTAL_TRANSFER_POP_CALLS: AtomicUsize = AtomicUsize::new(0);
#[cfg(feature = "transfer-debug-exact")]
pub static TOTAL_TRANSFER_PUSH_CALLS: AtomicUsize = AtomicUsize::new(0);

#[cfg(feature = "debug")]
pub static mut START_TIME: Option<Instant> = None;

pub fn get_clock() -> &'static Instant {
    TIME_STAMP.get_or_init(|| {
        let current = Instant::now();
        CURRENT_STAMP.store(
            (current.elapsed().as_millis() / 100) as u32,
            Ordering::Relaxed,
        );
        current
    })
}

pub const OFFSET_SIZE: usize = size_of::<usize>();
pub const TAG_SIZE: usize = OFFSET_SIZE * 2;

struct RseqResultConst;

impl RseqResultConst {
    pub const FAILED: usize = usize::MAX;
    pub const SUCCESS: usize = 1;
}

#[repr(transparent)]
#[derive(Debug, PartialEq)]
pub struct RseqResult(pub usize);

impl RseqResult {
    #[inline(always)]
    pub const fn is_success(&self) -> bool {
        self.0 == RseqResultConst::SUCCESS
    }

    #[inline(always)]
    pub const fn is_failed(&self) -> bool {
        self.0 == RseqResultConst::FAILED
    }
}
