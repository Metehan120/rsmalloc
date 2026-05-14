#![feature(likely_unlikely)]
#![feature(thread_local)]
#![allow(binary_asm_labels, unsafe_op_in_unsafe_fn, static_mut_refs)]

use std::{fmt::Debug, process::abort};

use crate::{core_prim::wrappers::UnsafePointer, rseq_core::rseq_main::rseq};

pub(crate) static mut MAGIC: u16 = 0;
pub(crate) static mut FREED_MAGIC: u16 = 1;
pub(crate) const ALIGN_TAG: usize = usize::from_le_bytes(*b"RSMALIGN");
pub(crate) const OFFSET_SIZE: usize = size_of::<usize>();
pub(crate) const TAG_SIZE: usize = OFFSET_SIZE * 2;

pub mod big_allocation;
pub mod core_prim;
pub mod inner;
pub mod internals;
pub mod rseq_core;
pub mod utility;

pub enum Err {
    OutOfReservation,
    OutOfMemory,
}

#[repr(C, align(16))]
pub struct MetaData {
    pub start: usize,
    pub end: usize,
    pub next: usize,
}

impl MetaData {
    pub const fn new() -> MetaData {
        MetaData {
            start: 0,
            end: 0,
            next: 0,
        }
    }
}

#[repr(C, align(16))]
#[derive(Copy, Clone, Default)]
pub struct BigAllocMeta {
    pub next: *mut BigAllocMeta,
    pub size: usize,
}

impl BigAllocMeta {
    pub const SIZE: usize = size_of::<Self>();
}

#[repr(C, align(16))]
pub struct Header {
    pub next: *mut Header,
    pub class: u8,
    pub magic: u16,
    pub life_time: u32,
    pub _padding: u8,
}

impl Header {
    pub const SIZE: usize = size_of::<Self>();
}

#[repr(u32)]
pub(crate) enum RSMallocError {
    DoubleFree = 0x1000,
    MemoryCorruption = 0x1001,
    OutOfMemory = 0x1003,
    VAIinitFailed = 0x1005,
    SecurityViolation = 0x100A,
    AttackOrCorruption = 0x100B,
    ICCFailedToInitialize = 0x100C,
}

impl Debug for RSMallocError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::DoubleFree => write!(f, "DoubleFree (0x1000)"),
            Self::MemoryCorruption => write!(f, "MemoryCorruption (0x1001)"),
            Self::OutOfMemory => write!(f, "OutOfMemory (0x1003)"),
            Self::VAIinitFailed => write!(f, "VAIinitFailed (0x1005)"),
            Self::SecurityViolation => write!(f, "SecurityViolation (0x100A)"),
            Self::AttackOrCorruption => write!(f, "AttackOrCorruption (0x100B)"),
            Self::ICCFailedToInitialize => write!(f, "ICCFailedToInitialize (0x100C)"),
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

pub trait GenericCache {
    unsafe fn push(&self, class: usize, header: *mut Header);
    unsafe fn pop(&self, class: usize) -> UnsafePointer<Header>;
    unsafe fn pop_non_inline(&self, class: usize) -> UnsafePointer<Header>;
    unsafe fn push_tailed(
        &self,
        class: usize,
        header: *mut Header,
        tail: *mut Header,
        batch_size: usize,
    );
}

pub trait RseqCoreTrait {
    unsafe fn push(
        &self,
        list_ptr: *mut *mut Header,
        usage_ptr: *mut usize,
        rseq: &rseq,
        header: *mut Header,
    ) -> usize;
    unsafe fn push_tailed(
        &self,
        list_ptr: *mut *mut Header,
        usage_ptr: *mut usize,
        rseq: &rseq,
        header: *mut Header,
        tail: *mut Header,
        batch_size: usize,
    ) -> usize;
    unsafe fn pop(
        &self,
        list_ptr: *mut *mut Header,
        usage_ptr: *mut usize,
        rseq: &rseq,
    ) -> *mut Header;
}
