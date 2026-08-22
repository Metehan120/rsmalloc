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

use std::{fmt::Debug, sync::atomic::Ordering};

#[cfg(not(target_arch = "x86_64"))]
compile_error!(
    "RSMalloc is currently only supporting x86-64 due to assembly use, future updates may add arm64 support"
);

#[cfg(not(target_os = "linux"))]
compile_error!(
    "RSMalloc is only supported on Linux; RSEQ is a Linux-only syscall feature and cannot be replicated on other OSes. RSMalloc will only support Linux until other OSes support RSEQ-like syscall."
);

pub fn get_total_cached_va() -> usize {
    TOTAL_CACHED_VA.load(Ordering::Relaxed)
}

mod backend;
mod big_allocations;
mod core_prim;
#[cfg(feature = "debug-print")]
mod debug_exit_printer;
#[cfg(feature = "debug-printer-thread")]
mod debug_printer_thread;
mod frontend;
mod global_vals;
mod inner;
mod internals;
mod result_handling;
mod rseq_core;
mod traits;
mod utility;

use global_vals::*;
use result_handling::*;

#[cfg(any(all(feature = "debug-exact", not(feature = "preload")), doc))]
pub use frontend::global_alloc::RSMallocExactStats;
#[cfg(any(all(feature = "debug", not(feature = "preload")), doc))]
pub use frontend::global_alloc::RSMallocStats;

#[cfg(not(feature = "preload"))]
pub use frontend::global_alloc::*;
use rsmalloc_macro::assert_sizes;

#[repr(u8)]
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Flags {
    NotAllocated = 2,
    Allocated = 4,
    Trimmed = 8,
    BigAlloc = 16,
}

#[repr(C, align(16))]
struct MetaData {
    pub next_page: *mut MetaData,
    pub start: usize,
    pub end: usize,
    pub next: usize,
    pub node_id: u16,
}

#[repr(C, align(16))]
#[derive(Copy, Clone, Default)]
struct BigAllocMeta {
    pub next: *mut BigAllocMeta,
    pub size: usize,
    pub order: usize,
    pub buddy_region: usize,
    pub aligned: bool,
}

// DO NOT TOUCH HEADER POSITIONING, RSEQ DEPENDS ON IT
#[cfg(not(feature = "extended-header"))]
#[repr(C, align(16))]
#[assert_sizes(16)]
struct Header {
    pub next: *mut Header,
    pub flags: Flags,
    pub class: u8,
    pub magic: u16,
    pub life_time: u32,
}

// DO NOT TOUCH HEADER POSITIONING, RSEQ DEPENDS ON IT
#[cfg(feature = "extended-header")]
#[repr(C, align(16))]
#[assert_sizes(32)]
struct Header {
    pub next: *mut Header,
    pub magic: u64,
    pub flags: Flags,
    pub life_time: u32,
    pub class: u8,
}

impl Header {
    pub const SIZE: usize = size_of::<Self>();
}
