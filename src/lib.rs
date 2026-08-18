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

use std::{fmt::Debug, process::abort, sync::atomic::Ordering};

#[cfg(not(target_arch = "x86_64"))]
compile_error!(
    "RSMalloc is currently only supporting x86-64 due to assembly use, future updates may add arm64 support"
);

#[cfg(not(target_os = "linux"))]
compile_error!(
    "RSMalloc is only supported on Linux; RSEQ is a Linux-only syscall feature and cannot be replicated on other OSes. RSMalloc will only support Linux until other OSes support RSEQ-like syscall."
);

pub(crate) use global_vals::*;

pub fn get_total_cached_va() -> usize {
    TOTAL_CACHED_VA.load(Ordering::Relaxed)
}

pub(crate) mod backend;
pub(crate) mod big_allocations;
pub(crate) mod core_prim;
#[cfg(feature = "debug-print")]
mod debug_exit_printer;
#[cfg(feature = "debug-printer-thread")]
mod debug_printer_thread;
pub(crate) mod frontend;
pub(crate) mod global_vals;
pub(crate) mod inner;
pub(crate) mod internals;
pub(crate) mod rseq_core;
pub(crate) mod traits;
pub(crate) mod utility;

#[cfg(any(all(feature = "debug-exact", not(feature = "preload")), doc))]
pub use frontend::global_alloc::RSMallocExactStats;
#[cfg(any(all(feature = "debug", not(feature = "preload")), doc))]
pub use frontend::global_alloc::RSMallocStats;

#[cfg(not(feature = "preload"))]
pub use frontend::global_alloc::*;

#[repr(u8)]
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Flags {
    Zero = 1,
    Allocated = 2,
    Trimmed = 3,
    Big = 4,
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
    pub flags: Flags,
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
    pub flags: Flags,
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
