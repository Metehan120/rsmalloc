use std::arch::asm;

use crate::rseq_core::rseq_main::rseq;

#[thread_local]
pub static mut RSMALLOC_RSEQ: rseq = rseq {
    cpu_id_start: 0,
    cpu_id: u32::MAX,
    rseq_cs: 0,
    flags: 0,
    node_id: 0,
    mm_cid: 0,
};

const RSEQ_SIGNATURE: u32 = 0x53053053;

#[inline(never)]
pub unsafe fn register_rseq_raw() -> isize {
    let rseq_ptr = &raw mut RSMALLOC_RSEQ;
    let rseq_len = size_of::<rseq>() as usize;
    let flags: usize = 0;
    let sig: usize = RSEQ_SIGNATURE as usize;

    let ret: isize;

    #[cfg(target_arch = "x86_64")]
    asm!(
        "syscall",
        in("rax") 334,
        in("rdi") rseq_ptr,
        in("rsi") rseq_len,
        in("rdx") flags,
        in("r10") sig,
        lateout("rax") ret,
        lateout("rcx") _,
        lateout("r11") _,
        options(nostack),
    );
    #[cfg(target_arch = "aarch64")]
    asm!(
        "svc #0",
        in("x8") 293,
        in("x0") rseq_ptr,
        in("x1") rseq_len,
        in("x2") flags,
        in("x3") sig,
        lateout("x0") ret,
        options(nostack, preserves_flags)
    );

    ret
}
