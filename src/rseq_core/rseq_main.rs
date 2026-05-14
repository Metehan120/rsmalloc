use std::arch::asm;

#[repr(C, align(32))]
pub struct rseq {
    pub cpu_id_start: u32,
    pub cpu_id: u32,
    pub rseq_cs: u64,
    pub flags: u32,
    pub node_id: u32,
    pub mm_cid: u32,
}

unsafe extern "C" {
    #[link_name = "__rseq_offset"]
    static __rseq_offset: isize;
    #[link_name = "__rseq_size"]
    static __rseq_size: u32;
}

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

    &*rseq_ptr
}

#[inline(always)]
pub unsafe fn get_cs_ptr(rseq: &rseq) -> *mut usize {
    core::ptr::addr_of!((*rseq).rseq_cs) as *mut usize
}
