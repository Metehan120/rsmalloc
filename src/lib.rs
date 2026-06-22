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
//!     BuddyTHP, CacheLimit, EMASettings, EmaAlpha, ForeignPointerSettings,
//!     RSMalloc, RSMallocConfig, THP, THPSettings,
//! };
//!
//! const CONFIG: RSMallocConfig = RSMallocConfig::DEFAULT
//!     .with_thp_settings(THPSettings::new(THP::Enable, BuddyTHP::Force))
//!     .with_ema_settings(EMASettings::new(EmaAlpha::Fast, 16))
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
#![feature(linkage)]
#![allow(binary_asm_labels, unsafe_op_in_unsafe_fn, static_mut_refs)]

#[cfg(feature = "canary")]
use std::os::raw::c_void;
use std::{
    fmt::Debug,
    process::abort,
    sync::atomic::{AtomicUsize, Ordering},
};

use crate::{core_prim::wrappers::UnsafePointer, rseq_core::rseq_main::rseq};

#[cfg(not(target_arch = "x86_64"))]
compile_error!(
    "rsmalloc is currently only supporting x86-64 due to assembly use, future updates may add arm64 support"
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
#[cfg(feature = "legacy-glibc-support")]
pub(crate) static mut IS_RSEQ_INTERNAL: bool = false;
pub(crate) static mut BUDDY_INIT: bool = false;
pub(crate) static mut BUDDY_MAX_CACHE: usize = 0;
pub(crate) static mut BUDDY_ATTEMPT_HUGE: bool = false;
pub(crate) static mut MAGIC_DISABLE: bool = false;
#[cfg(not(feature = "preload"))]
pub(crate) static mut FOREIGN_POINTER_ABORT: bool = false;
pub(crate) static mut ALIGN_TAG: usize = usize::from_le_bytes(*b"RSMALIGN");
#[cfg(not(feature = "extended-header"))]
pub(crate) static mut RANDOMC_CANARY_CONSTANT: u8 = 0;
#[cfg(feature = "extended-header")]
pub(crate) static mut RANDOMC_CANARY_CONSTANT: u64 = 0;
#[cfg(feature = "preload")]
pub(crate) static mut ENABLE_TRIM: bool = false;

pub(crate) static TOTAL_CACHED_VA: AtomicUsize = AtomicUsize::new(0);

/// Showing the total cached virtual address space, does not mirror actual memory usage.
pub fn get_total_cached_va() -> usize {
    TOTAL_CACHED_VA.load(Ordering::Relaxed)
}

#[cfg(feature = "debug")]
pub(crate) static REFILL_UNDER_PREDICTS: AtomicUsize = AtomicUsize::new(0);
#[cfg(feature = "debug")]
pub(crate) static REFILL_OVER_PREDICTS: AtomicUsize = AtomicUsize::new(0);
#[cfg(feature = "debug")]
pub(crate) static TOTAL_REFILL_CALLS: AtomicUsize = AtomicUsize::new(0);
#[cfg(feature = "debug")]
pub(crate) static ABORTS: AtomicUsize = AtomicUsize::new(0);
#[cfg(feature = "debug-exact")]
pub(crate) static GLOBAL_LOCKS: AtomicUsize = AtomicUsize::new(0);
#[cfg(feature = "debug-exact")]
pub(crate) static GLOBAL_LOCK_RETRIES: AtomicUsize = AtomicUsize::new(0);

pub(crate) const OFFSET_SIZE: usize = size_of::<usize>();
pub(crate) const TAG_SIZE: usize = OFFSET_SIZE * 2;

#[cfg(feature = "preload")]
pub(crate) mod abi;
pub(crate) mod big_allocations;
pub(crate) mod core_prim;
#[cfg(not(feature = "preload"))]
pub(crate) mod global_alloc;
pub(crate) mod inner;
pub(crate) mod internals;
pub(crate) mod rseq_core;
pub(crate) mod utility;

#[cfg(not(feature = "preload"))]
pub use global_alloc::{
    BuddyTHP, DisableMagic, EMASettings, EmaAlpha, ExperimentalFeatures, ForeignPointerPolicy,
    ForeignPointerSettings, MagicSafety, MagicSafetyDisable, PerCacheLimit, RSMALLOC_VERSION,
    RSMalloc, RSMallocCapabilities, RSMallocConfig, RSMallocTrim, RSTrimStatus, Size, THP,
    THPSettings,
};

#[cfg(any(all(feature = "debug-exact", not(feature = "preload")), doc))]
pub use global_alloc::RSMallocExactStats;
#[cfg(any(all(feature = "debug", not(feature = "preload")), doc))]
pub use global_alloc::RSMallocStats;

pub(crate) enum Err {
    OutOfMemory,
}

#[repr(C, align(16))]
pub(crate) struct MetaData {
    pub start: usize,
    pub end: usize,
    pub next: usize,
}

#[repr(C, align(16))]
#[derive(Copy, Clone, Default)]
pub(crate) struct BigAllocMeta {
    pub next: *mut BigAllocMeta,
    pub size: usize,
    pub order: usize,
    pub aligned: bool,
}

// DO NOT TOUCH HEADER POSITIONING, RSEQ DEPENDS ON IT
#[cfg(not(feature = "extended-header"))]
#[repr(C, align(16))]
pub(crate) struct Header {
    pub next: *mut Header,
    pub canary: u8,
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
    pub life_time: u32,
    pub class: u8,
    pub canary: u64,
}

#[cfg(not(feature = "extended-header"))]
const _: () = assert!(size_of::<Header>() == 16);

#[cfg(feature = "extended-header")]
const _: () = assert!(size_of::<Header>() == 32);

impl Header {
    pub const SIZE: usize = size_of::<Self>();

    #[cfg(not(feature = "extended-header"))]
    pub fn expected_canary(&self, addr: *mut Header) -> u8 {
        ((addr as u8 >> 4) ^ self.class as u8 ^ unsafe { RANDOMC_CANARY_CONSTANT }).rotate_left(1)
    }

    #[cfg(feature = "extended-header")]
    pub fn expected_canary(&self, addr: *mut Header) -> u64 {
        ((addr as u64 >> 4) ^ self.class as u64 ^ unsafe { RANDOMC_CANARY_CONSTANT }).rotate_left(1)
    }
    pub fn compute_canary(&mut self, addr: *mut Header) {
        self.canary = self.expected_canary(addr);
    }

    #[cfg(feature = "canary")]
    pub fn canary_mismatch(&self, addr: *mut Header) {
        if self.expected_canary(addr) != self.canary {
            RSMallocError::CanaryMismatch.log_and_abort(
                addr as *mut c_void,
                "canary mismatch, security violation",
                None,
            )
        }
    }
}

#[repr(u32)]
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
    #[cfg(feature = "canary")]
    CanaryMismatch,
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
            #[cfg(feature = "canary")]
            Self::CanaryMismatch => write!(f, "CanaryMismatch"),
        }
    }
}

impl RSMallocError {
    #[inline(never)]
    pub fn log_and_abort(&self, ptr: *mut std::ffi::c_void, extra: &str, errno: Option<i32>) -> ! {
        if let Some(errno) = errno {
            eprintln!(
                "[RSMALLOC FATAL] {:?} at ptr={:p} | {} | errno({})",
                self, ptr, extra, errno
            );
        } else {
            eprintln!("[RSMALLOC FATAL] {:?} at ptr={:p} | {}", self, ptr, extra);
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
    ) -> usize;
    unsafe fn push_tailed(
        &self,
        list_ptr: *mut *mut Header,
        rseq: &rseq,
        cpu_id: usize,
        header: *mut Header,
        tail: *mut Header,
    ) -> usize;
    unsafe fn pop(&self, list_ptr: *mut *mut Header, rseq: &rseq, cpu_id: usize) -> *mut Header;
}

#[cfg(feature = "debug-print")]
#[used]
#[unsafe(link_section = ".fini_array")]
static RSMALLOC_FINI: unsafe extern "C" fn() = rsmalloc_fini;

#[cfg(feature = "debug-print")]
unsafe extern "C" fn rsmalloc_fini() {
    use std::sync::atomic::Ordering::Relaxed;

    let total = TOTAL_REFILL_CALLS.load(Relaxed);
    let overpredicts = REFILL_OVER_PREDICTS.load(Relaxed);
    let underpredicts = REFILL_UNDER_PREDICTS.load(Relaxed);
    let total_cached_va = TOTAL_CACHED_VA.load(Relaxed);

    let aborts = ABORTS.load(Relaxed);

    let percentage = ((overpredicts + underpredicts) as f64 / total as f64) * 100.0;

    #[cfg(not(feature = "debug-exact"))]
    eprintln!(
        "rsmalloc: total refill calls={}, overpredicts={}, underpredicts={}, total cached VA={}, RSEQ Aborts={}, miss percentage={:.2}%",
        total, overpredicts, underpredicts, total_cached_va, aborts, percentage,
    );

    #[cfg(feature = "debug-exact")]
    eprintln!(
        "rsmalloc: total refill calls={}, overpredicts={}, underpredicts={}, total cached VA={}, RSEQ Aborts={}, miss percentage={:.4}%, total lock count={}, total lock retries={}",
        total,
        overpredicts,
        underpredicts,
        total_cached_va,
        aborts,
        percentage,
        GLOBAL_LOCKS.load(Relaxed),
        GLOBAL_LOCK_RETRIES.load(Relaxed),
    );
}
