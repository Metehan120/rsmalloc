use std::{fmt::Debug, mem::transmute, process::abort};

use crate::Header;

#[repr(u32)]
#[derive(PartialEq, Eq)]
pub enum RSMallocError {
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
