use std::arch::asm;
#[cfg(feature = "legacy-glibc-support")]
use std::hint::unlikely;

#[cfg(feature = "legacy-glibc-support")]
use crate::{
    IS_RSEQ_INTERNAL, RSMallocError,
    core_prim::rseq_register::{RSMALLOC_RSEQ, register_rseq_raw},
};

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
    // Weak libc rseq symbols are declared as pointers so a missing symbol
    // resolves to null. When present, the pointer references the actual
    // libc storage for the offset/size value.
    #[linkage = "weak"]
    pub static __rseq_offset: *const isize;
    #[linkage = "weak"]
    pub static __rseq_size: *const u32;
}

#[inline(always)]
pub unsafe fn get_rseq() -> &'static rseq {
    #[cfg(feature = "legacy-glibc-support")]
    if IS_RSEQ_INTERNAL {
        if unlikely(RSMALLOC_RSEQ.cpu_id == u32::MAX) {
            let ret = register_rseq_raw();
            if unlikely(ret != 0 || RSMALLOC_RSEQ.cpu_id == u32::MAX) {
                RSMallocError::RSEQRegFailed.log_and_abort(
                    core::ptr::null_mut(),
                    "thread-local RSEQ registration failed",
                    None,
                );
            }
        }
        return &RSMALLOC_RSEQ;
    }

    let offset_val = *__rseq_offset;
    let rseq_ptr: *mut rseq;

    #[cfg(target_arch = "x86_64")]
    asm!(
        "mov {tp}, fs:[0]",
        "add {tp}, {offset}",
        tp = out(reg) rseq_ptr,
        offset = in(reg) offset_val,
        options(pure, nomem, nostack, preserves_flags)
    );

    #[cfg(target_arch = "aarch64")]
    {
        let tp: usize;
        asm!(
            "mrs {tp}, tpidr_el0",
            "add {rseq_ptr}, {tp}, {offset}",
            tp = out(reg) tp,
            offset = in(reg) offset_val,
            rseq_ptr = out(reg) rseq_ptr,
            options(pure, nomem, nostack, preserves_flags)
        );
    }

    &*rseq_ptr
}

#[inline(always)]
pub unsafe fn get_cs_ptr(rseq: &rseq) -> *mut usize {
    core::ptr::addr_of!((*rseq).rseq_cs) as *mut usize
}
