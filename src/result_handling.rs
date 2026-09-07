use std::{fmt::Debug, io::Error, mem::transmute, process::abort};

use thiserror::Error;

use crate::Header;

#[derive(Debug, Error)]
pub enum RSMallocError {
    #[error("out of memory while allocating {size} bytes in {subsystem}")]
    OutOfMemory {
        subsystem: &'static str,
        size: usize,
        errno: Option<i32>,
    },
    #[error("double free at {ptr:p}")]
    DoubleFree { ptr: *mut u8 },
    #[error("allocator metadata corruption at {ptr:p}: {reason}")]
    Corruption { ptr: *mut u8, reason: &'static str },
    #[error("invalid pointer {ptr:p}")]
    InvalidPointer { ptr: *mut u8 },
    #[error("rseq unavailable")]
    RseqUnavailable,
    #[cfg(not(feature = "preload"))]
    #[error("foreign pointer {ptr:p}")]
    ForeignPointer { ptr: *mut u8 },
    #[error("security violation, reason: {reason}")]
    SecurityViolation {
        reason: &'static str,
        errno: Option<i32>,
    },
}

impl RSMallocError {
    #[inline(never)]
    pub fn log_and_abort(&self) -> ! {
        match self {
            Self::OutOfMemory {
                errno: Some(errno), ..
            }
            | Self::SecurityViolation {
                errno: Some(errno), ..
            } => eprintln!(
                "[RSMALLOC FATAL] {self} | os_err: {} | errno({errno})",
                Error::from_raw_os_error(*errno),
            ),
            _ => eprintln!("[RSMALLOC FATAL] {self}"),
        }

        abort();
    }
}

struct RseqResultConst;

impl RseqResultConst {
    pub const FAILED: usize = usize::MAX;
    pub const SUCCESS: usize = 1;
}

#[repr(transparent)]
#[derive(Debug, PartialEq)]
pub struct RseqResult(usize);

impl RseqResult {
    #[inline(always)]
    pub const unsafe fn new(value: usize) -> Self {
        // # SAFETY:
        // RseqResult is repr(transparent) which means it is the same layout as usize
        // safe to exploit rust's repr structure with transmute here;
        // guarantees 0 overhead in generated code, just in case transmute
        // normal RseqResult(res) will also compile same as transmute
        transmute::<usize, RseqResult>(value)
    }

    #[inline(always)]
    pub const unsafe fn new_header(value: *mut Header) -> RseqResult {
        // # SAFETY:
        // RseqResult is repr(transparent) which means it is the same layout as usize
        // safe to exploit rust's repr structure with transmute here;
        // guarantees 0 overhead in generated code, just in case transmute
        // normal RseqResult(res) will also compile same as transmute
        transmute::<*mut Header, RseqResult>(value)
    }

    #[inline(always)]
    pub const fn get(&self) -> *mut Header {
        self.0 as *mut Header
    }

    #[inline(always)]
    pub const fn is_success(&self) -> bool {
        self.0 == RseqResultConst::SUCCESS
    }

    #[inline(always)]
    pub const fn is_failed(&self) -> bool {
        self.0 == RseqResultConst::FAILED
    }
}
