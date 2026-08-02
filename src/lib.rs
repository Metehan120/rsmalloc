//! rsmalloc is an experimental RSEQ-based Rust memory allocator built for
//! low-overhead concurrent allocation in real applications, not just
//! benchmark-shaped workloads.
//!
//! # Quick Start
//!
//! Use [`RSMalloc`] as the process-wide global allocator:
//!
//! ```rust
//! use rsmalloc::RSMalloc;
//!
//! #[global_allocator]
//! static GLOBAL: RSMalloc = RSMalloc::new_default();
//! ```
//!
//! Configure it explicitly when the defaults are not enough:
//!
//! ```rust
//! use rsmalloc::{
//!     BuddyTHP, CacheLimit, ForeignPointerSettings, RefillPredictorSettings,
//!     RSMalloc, RSMallocConfig, THP, THPSettings,
//! };
//!
//! const CONFIG: RSMallocConfig = RSMallocConfig::DEFAULT
//!     .with_thp_settings(THPSettings::new(THP::Enable, BuddyTHP::Force))
//!     .with_refill_predictor_settings(RefillPredictorSettings::new(16))
//!     .with_max_refill_retries(4)
//!     .with_max_per_buddy_cache(CacheLimit::Bytes(512 * 1024 * 1024))
//!     .with_foreign_pointer(ForeignPointerSettings::DEFAULT);
//!
//! #[global_allocator]
//! static GLOBAL: RSMalloc = RSMalloc::new_with_config(CONFIG);
//! ```
//!
//! rsmalloc also supports `LD_PRELOAD`-style use for C applications. See the
//! README for preload build and runtime details.
//!
//! This crate currently targets nightly Rust, Linux, and `x86_64`.

#![feature(likely_unlikely)]
#![feature(thread_local)]
#![allow(binary_asm_labels, unsafe_op_in_unsafe_fn, static_mut_refs)]

use std::{
    fmt::Debug,
    process::abort,
    sync::{
        OnceLock,
        atomic::{AtomicU32, AtomicUsize, Ordering},
    },
    time::Instant,
};

use crate::{
    core_prim::wrappers::UnsafePointer, internals::lock::SpinLock, rseq_core::rseq_offsets::rseq,
};

#[cfg(not(target_arch = "x86_64"))]
compile_error!(
    "RSMalloc is currently only supporting x86-64 due to assembly use, future updates may add arm64 support"
);

#[cfg(not(target_os = "linux"))]
compile_error!(
    "RSMalloc is only supported on Linux; RSEQ is a Linux-only syscall feature and cannot be replicated on other OSes. RSMalloc will only support Linux until other OSes support RSEQ-like syscall."
);

#[cfg(not(feature = "extended-header"))]
pub(crate) static mut MAGIC: u16 = u16::from_le_bytes(*b"RS");
#[cfg(not(feature = "extended-header"))]
pub(crate) static mut FREED_MAGIC: u16 = u16::from_le_bytes(*b"RM");
#[cfg(not(feature = "extended-header"))]
pub(crate) static mut BIG_MAGIC: u16 = u16::from_le_bytes(*b"RB");

#[cfg(feature = "extended-header")]
pub(crate) static mut MAGIC: u64 = u64::from_le_bytes(*b"RSMAGICS");
#[cfg(feature = "extended-header")]
pub(crate) static mut FREED_MAGIC: u64 = u64::from_le_bytes(*b"RMMAGICF");
#[cfg(feature = "extended-header")]
pub(crate) static mut BIG_MAGIC: u64 = u64::from_le_bytes(*b"RBMAGICB");

pub(crate) static mut RS_DISABLE_THP: bool = false;
pub(crate) static mut BUDDY_INIT: bool = false;
pub(crate) static mut BUDDY_MAX_CACHE: usize = 0;
pub(crate) static mut BUDDY_ATTEMPT_HUGE: bool = false;
#[cfg(not(feature = "preload"))]
pub(crate) static mut FOREIGN_POINTER_ABORT: bool = false;
pub(crate) static mut ALIGN_TAG: usize = usize::from_le_bytes(*b"RSMALIGN");
pub(crate) static mut DISABLE_TRIM_THREAD: bool = false;
pub(crate) static mut TRIM_THRESHOLD: usize = 1024 * 1024 * 10;

pub(crate) static TOTAL_CACHED_VA: AtomicUsize = AtomicUsize::new(0);
#[cfg(feature = "debug")]
pub(crate) static HIGH_WATER_SLAB_CACHED_VA: AtomicUsize = AtomicUsize::new(0);
#[cfg(feature = "debug")]
pub(crate) static HIGH_WATER_BUDDY_CACHED_VA: AtomicUsize = AtomicUsize::new(0);
#[cfg(feature = "debug")]
pub(crate) static HIGH_WATER_TOTAL_CACHED_VA: AtomicUsize = AtomicUsize::new(0);

pub fn get_total_cached_va() -> usize {
    TOTAL_CACHED_VA.load(Ordering::Relaxed)
}

#[cfg(feature = "debug")]
#[inline(always)]
pub(crate) fn update_high_water(max: &AtomicUsize, value: usize) {
    let mut current = max.load(Ordering::Relaxed);
    while value > current {
        match max.compare_exchange_weak(current, value, Ordering::Relaxed, Ordering::Relaxed) {
            Ok(_) => break,
            Err(observed) => current = observed,
        }
    }
}

#[inline(always)]
pub(crate) fn add_slab_cached_va(bytes: usize) {
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
pub(crate) fn add_buddy_cached_va(bytes: usize) {
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
pub(crate) static REFILL_UNDER_PREDICTS: AtomicUsize = AtomicUsize::new(0);
#[cfg(feature = "debug")]
pub(crate) static REFILL_OVER_PREDICTS: AtomicUsize = AtomicUsize::new(0);
#[cfg(feature = "debug")]
pub(crate) static TOTAL_REFILL_CALLS: AtomicUsize = AtomicUsize::new(0);
#[cfg(feature = "debug")]
pub(crate) static REFILLS_BY_CLASS: [AtomicUsize; utility::NUM_SIZE_CLASSES] =
    [const { AtomicUsize::new(0) }; utility::NUM_SIZE_CLASSES];
#[cfg(feature = "debug")]
pub(crate) static ABORTS: AtomicUsize = AtomicUsize::new(0);
#[cfg(feature = "debug")]
pub(crate) static TOTAL_MMAP_CALLS: AtomicUsize = AtomicUsize::new(0);
#[cfg(feature = "debug")]
pub(crate) static TOTAL_MMAP_BYTES: AtomicUsize = AtomicUsize::new(0);

#[inline(always)]
pub(crate) fn record_mmap_call(bytes: usize) {
    #[cfg(not(feature = "debug"))]
    let _ = bytes;

    #[cfg(feature = "debug")]
    {
        TOTAL_MMAP_CALLS.fetch_add(1, Ordering::Relaxed);
        TOTAL_MMAP_BYTES.fetch_add(bytes, Ordering::Relaxed);
    }
}

#[cfg(feature = "debug-exact")]
pub(crate) static GLOBAL_LOCKS: AtomicUsize = AtomicUsize::new(0);
#[cfg(feature = "debug-exact")]
pub(crate) static GLOBAL_LOCK_RETRIES: AtomicUsize = AtomicUsize::new(0);
#[cfg(feature = "debug-exact")]
pub(crate) static GLOBAL_TRY_LOCKS: AtomicUsize = AtomicUsize::new(0);
#[cfg(feature = "debug-exact")]
pub(crate) static GLOBAL_TRY_LOCK_MISSES: AtomicUsize = AtomicUsize::new(0);
#[cfg(feature = "debug-exact")]
pub(crate) static GLOBAL_SPIN_WAITS: AtomicUsize = AtomicUsize::new(0);

pub(crate) static TIME_STAMP: OnceLock<Instant> = OnceLock::new();
pub(crate) static CURRENT_STAMP: AtomicU32 = AtomicU32::new(0);
pub(crate) static AVERAGE_BLOCK_TIMES: AtomicU32 = AtomicU32::new(1000);
pub(crate) static BUDDY_AVERAGE_BLOCK_TIMES: AtomicU32 = AtomicU32::new(1000);
pub(crate) static GLOBAL_TRIM_LOCK: SpinLock = SpinLock::new();
pub(crate) static mut NCPU: usize = 0;

#[cfg(feature = "transfer-debug")]
pub(crate) static TOTAL_TRANSFER_STEALS: AtomicUsize = AtomicUsize::new(0);
#[cfg(feature = "transfer-debug")]
pub(crate) static TOTAL_TRANSFER_RETRIES: AtomicUsize = AtomicUsize::new(0);
#[cfg(feature = "transfer-debug")]
pub(crate) static DRY_TRANSFER_STEALS: AtomicUsize = AtomicUsize::new(0);
#[cfg(feature = "transfer-debug-exact")]
pub(crate) static TOTAL_TRANSFER_POP_CALLS: AtomicUsize = AtomicUsize::new(0);
#[cfg(feature = "transfer-debug-exact")]
pub(crate) static TOTAL_TRANSFER_PUSH_CALLS: AtomicUsize = AtomicUsize::new(0);

#[cfg(feature = "debug")]
pub(crate) static mut START_TIME: Option<Instant> = None;

pub(crate) fn get_clock() -> &'static Instant {
    TIME_STAMP.get_or_init(|| {
        let current = Instant::now();
        CURRENT_STAMP.store(current.elapsed().as_millis() as u32, Ordering::Relaxed);
        current
    })
}

pub(crate) const OFFSET_SIZE: usize = size_of::<usize>();
pub(crate) const TAG_SIZE: usize = OFFSET_SIZE * 2;

pub(crate) const ZERO_FLAG: u8 = 1;
pub(crate) const ALLOCATED_FLAG: u8 = 2;
pub(crate) const TRIMMED_FLAG: u8 = 3;

#[cfg(feature = "preload")]
pub(crate) mod abi;
pub(crate) mod backend;
pub(crate) mod big_allocations;
pub(crate) mod core_prim;
#[cfg(feature = "debug-print")]
mod debug_exit_printer;
#[cfg(feature = "debug-printer-thread")]
mod debug_printer_thread;
#[cfg(not(feature = "preload"))]
pub(crate) mod global_alloc;
pub(crate) mod inner;
pub(crate) mod internals;
pub(crate) mod rseq_core;
pub(crate) mod trim;
pub(crate) mod utility;

#[cfg(any(all(feature = "debug-exact", not(feature = "preload")), doc))]
pub use global_alloc::RSMallocExactStats;
#[cfg(any(all(feature = "debug", not(feature = "preload")), doc))]
pub use global_alloc::RSMallocStats;

#[cfg(not(feature = "preload"))]
pub use global_alloc::*;

pub(crate) enum Err {
    OutOfMemory,
}

#[repr(C, align(16))]
pub(crate) struct MetaData {
    pub next_page: *mut MetaData,
    pub start: usize,
    pub end: usize,
    pub next: usize,
    pub node_id: u16,
}

#[repr(C, align(16))]
#[derive(Copy, Clone, Default)]
pub(crate) struct BigAllocMeta {
    pub next: *mut BigAllocMeta,
    pub size: usize,
    pub order: usize,
    pub buddy_region: usize,
    pub aligned: bool,
}

// DO NOT TOUCH HEADER POSITIONING, RSEQ DEPENDS ON IT
#[cfg(not(feature = "extended-header"))]
#[repr(C, align(16))]
pub(crate) struct Header {
    pub next: *mut Header,
    pub flags: u8,
    pub class: u8,
    pub magic: u16,
    pub life_time: u32,
}

// DO NOT TOUCH HEADER POSITIONING, RSEQ DEPENDS ON IT
#[cfg(feature = "extended-header")]
#[repr(C, align(16))]
pub(crate) struct Header {
    pub next: *mut Header,
    pub magic: u64,
    pub flags: u8,
    pub life_time: u32,
    pub class: u8,
}

#[cfg(not(feature = "extended-header"))]
const _: () = assert!(size_of::<Header>() == 16);

#[cfg(feature = "extended-header")]
const _: () = assert!(size_of::<Header>() == 32);

impl Header {
    pub const SIZE: usize = size_of::<Self>();
}

#[repr(u32)]
#[derive(PartialEq, Eq)]
pub(crate) enum RSMallocError {
    DoubleFree = 0x1000,
    MemoryCorruption = 0x1001,
    OutOfMemory = 0x1003,
    VAIinitFailed = 0x1005,
    AttackOrCorruption = 0x100B,
    SecurityViolation = 0x100C,
    RSEQRegFailed = 0x100D,
    #[cfg(not(feature = "preload"))]
    ForeignPointer = 0x100E,
    InvalidPointer = 0x100F,
    #[cfg(feature = "abort-on-rseq-failure")]
    RseqCeasedToExist = 0x1010,
}

impl Debug for RSMallocError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::DoubleFree => write!(f, "DoubleFree (0x1000)"),
            Self::MemoryCorruption => write!(f, "MemoryCorruption (0x1001)"),
            Self::OutOfMemory => write!(f, "OutOfMemory (0x1003)"),
            Self::VAIinitFailed => write!(f, "VAIinitFailed (0x1005)"),
            Self::AttackOrCorruption => write!(f, "AttackOrCorruption (0x100B)"),
            Self::SecurityViolation => write!(f, "SecurityViolation (0x100C)"),
            Self::RSEQRegFailed => write!(f, "RSEQRegFailed (0x100D)"),
            #[cfg(not(feature = "preload"))]
            Self::ForeignPointer => write!(f, "ForeignPointer (0x100E)"),
            Self::InvalidPointer => write!(f, "InvalidPointer (0x100F)"),
            #[cfg(feature = "abort-on-rseq-failure")]
            Self::RseqCeasedToExist => write!(f, "RseqCeasedToExist (0x1010)"),
        }
    }
}

impl RSMallocError {
    #[inline(never)]
    pub fn log_and_abort(&self, ptr: *mut std::ffi::c_void, extra: &str, errno: Option<i32>) -> ! {
        #[cfg(feature = "print-cpu-on-double-free")]
        let current_cpu = unsafe {
            use crate::rseq_core::rseq_offsets::get_rseq;
            get_rseq().cpu_id
        };

        if let Some(errno) = errno {
            eprintln!(
                "[RSMALLOC FATAL] {:?} at ptr={:p} | {} | errno({})",
                self, ptr, extra, errno
            );
        } else {
            eprintln!("[RSMALLOC FATAL] {:?} at ptr={:p} | {}", self, ptr, extra);
        }

        #[cfg(feature = "print-cpu-on-double-free")]
        if *self == Self::DoubleFree {
            eprintln!("[RSMALLOC] Double free on CPU {}", current_cpu)
        }

        abort();
    }
}

pub(crate) trait GenericCache {
    unsafe fn push(&self, class: usize, header: *mut Header);
    unsafe fn pop(&self, class: usize) -> UnsafePointer<Header>;
    unsafe fn push_tailed(
        &self,
        class: usize,
        header: *mut Header,
        tail: *mut Header,
        batch_size: usize,
    );
}

pub(crate) trait RseqCoreTrait {
    unsafe fn push(
        &self,
        list_ptr: *mut *mut Header,
        rseq: &rseq,
        cpu_id: usize,
        header: *mut Header,
        usage_ptr: *mut usize,
    ) -> usize;
    unsafe fn push_tailed(
        &self,
        list_ptr: *mut *mut Header,
        rseq: &rseq,
        cpu_id: usize,
        header: *mut Header,
        tail: *mut Header,
        usage_ptr: *mut usize,
        batch_total: usize,
    ) -> usize;
    unsafe fn pop(
        &self,
        list_ptr: *mut *mut Header,
        rseq: &rseq,
        cpu_id: usize,
        usage_ptr: *mut usize,
    ) -> *mut Header;
}
