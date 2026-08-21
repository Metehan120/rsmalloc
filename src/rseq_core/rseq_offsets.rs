use std::arch::asm;

use rsmalloc_macro::stable_api_surface;

#[cfg(feature = "abort-on-rseq-failure")]
use crate::RSMallocError;
#[cfg(feature = "abort-on-rseq-failure")]
use std::{hint::unlikely, ptr::null_mut};

#[repr(C, align(32))]
#[derive(Debug, Clone, Copy)]
pub struct rseq {
    pub cpu_id_start: u32,
    pub cpu_id: u32,
    pub rseq_cs: u64,
    pub flags: u32,
    pub node_id: u32,
    pub mm_cid: u32,
}

unsafe extern "C" {
    pub static __rseq_offset: isize;
    pub static __rseq_size: u32;
}

#[stable_api_surface]
#[inline(always)]
pub unsafe fn get_rseq() -> &'static rseq {
    let rseq_ptr: *mut rseq;

    #[cfg(target_arch = "x86_64")]
    asm!(
        "mov {tp}, fs:[0]",
        "add {tp}, {offset}",
        tp = out(reg) rseq_ptr,
        offset = in(reg) __rseq_offset,
        options(pure, nomem, nostack, preserves_flags)
    );

    #[cfg(target_arch = "aarch64")]
    {
        let tp: usize;
        asm!(
            "mrs {tp}, tpidr_el0",
            "add {rseq_ptr}, {tp}, {offset}",
            tp = out(reg) tp,
            offset = in(reg) __rseq_offset,
            rseq_ptr = out(reg) rseq_ptr,
            options(pure, nomem, nostack, preserves_flags)
        );
    }

    let pointer = &*rseq_ptr;
    #[cfg(feature = "abort-on-rseq-failure")]
    if unlikely(pointer.cpu_id == u32::MAX) {
        RSMallocError::RseqCeasedToExist.log_and_abort(
            null_mut(),
            "RSEQ reported CPU ID (UINT_MAX/u32::MAX). This indicates a kernel or hardware failure. Please report this issue. If your system uses ECC memory, inspect corrected/uncorrected memory error logs.",
            None,
        );
    }

    pointer
}

#[inline(always)]
pub unsafe fn get_cs_ptr(rseq: &rseq) -> *mut usize {
    core::ptr::addr_of!((*rseq).rseq_cs) as *mut usize
}
