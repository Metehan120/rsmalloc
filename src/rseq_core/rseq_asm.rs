// ! DO NOT TOUCH, CHANGE OR BREATHE NEAR ASSEMBLY unless you know how rseq or assembly works !
//
// rseq_cs intentionally remains installed after every exit. Linux only requires
// clearing it before reclaiming the descriptor or referenced code; both have
// process lifetime here. Every new operation still installs its descriptor.

use std::{arch::asm, ptr::addr_of};

use rsmalloc_macro::stable_api_surface;

use crate::{
    Header, RseqResult,
    rseq_core::rseq_offsets::{get_cs_ptr, rseq},
    traits::RseqCoreTrait,
};

pub struct RseqCore;

impl RseqCoreTrait for RseqCore {
    #[stable_api_surface]
    #[inline(always)]
    unsafe fn push_tailed(
        &self,
        list_ptr: *mut *mut Header,
        rseq: &rseq,
        cpu_id: usize,
        header: *mut Header,
        tail: *mut Header,
        usage_ptr: *mut usize,
        batch_total: usize,
    ) -> RseqResult {
        let res: usize;
        let cs = get_cs_ptr(rseq);
        let cpu_id_start = addr_of!(rseq.cpu_id_start);

        asm!(
            ".pushsection .data.rel.ro,\"aw\",@progbits",
            ".balign 32",
            "4:",
            ".long 0, 0",
            ".quad 1f",
            ".quad 2f - 1f",
            ".quad 3f",
            ".popsection",

            "lea {tmp}, [rip + 4b]",
            "mov [{cs_ptr}], {tmp}",

            "1:",
            // Test cpu_id_start against cpu_id before entering critical section.
            "cmp [{cpu_id_start}], {cpu_id:e}",
            "jne 3f",

            "mov {tmp}, [{list}]",
            "mov [{tail}], {tmp}",
            "mov [{list}], {header}",

            "2:",
            "lock add qword ptr [{usage}], {batch_total}",
            "mov {res}, 1",
            "jmp 5f",

            ".balign 4",
            ".byte 0x0f, 0x1f, 0x05",
            // RSEQ abort signature, matches glibc/linux rseq convention.
            ".long 0x53053053",
            "3:",
            "mov {res}, -1",

            "5:",

            cs_ptr = in(reg) cs,
            tmp = out(reg) _,
            list = in(reg) list_ptr,
            header = in(reg) header,
            res = lateout(reg) res,
            tail = in(reg) tail,
            cpu_id_start = in(reg) cpu_id_start,
            cpu_id = in(reg) cpu_id,
            usage = in(reg) usage_ptr,
            batch_total = in(reg) batch_total,
            options(nostack),
        );

        RseqResult::new(res)
    }

    #[stable_api_surface]
    #[inline(always)]
    unsafe fn push(
        &self,
        list_ptr: *mut *mut Header,
        rseq: &rseq,
        cpu_id: usize,
        header: *mut Header,
        usage_ptr: *mut usize,
    ) -> RseqResult {
        let res: usize;
        let cs = get_cs_ptr(rseq);
        let cpu_id_start = addr_of!(rseq.cpu_id_start);

        asm!(
            ".pushsection .data.rel.ro,\"aw\",@progbits",
            ".balign 32",
            "4:",
            ".long 0, 0",
            ".quad 1f",
            ".quad 2f - 1f",
            ".quad 3f",
            ".popsection",

            "lea {tmp}, [rip + 4b]",
            "mov [{cs_ptr}], {tmp}",

            "1:",
            // Test cpu_id_start against cpu_id before entering critical section.
            "cmp [{cpu_id_start}], {cpu_id:e}",
            "jne 3f",

            "mov {tmp}, [{list}]",
            "mov [{header}], {tmp}",
            "mov [{list}], {header}",

            "2:",
            "lock inc qword ptr [{usage}]",
            "mov {res}, 1",
            "jmp 5f",

            ".balign 4",
            ".byte 0x0f, 0x1f, 0x05",
            // RSEQ abort signature, matches glibc/linux rseq convention.
            ".long 0x53053053",
            "3:",
            "mov {res}, -1",

            "5:",

            cs_ptr = in(reg) cs,
            tmp = out(reg) _,
            list = in(reg) list_ptr,
            header = in(reg) header,
            res = lateout(reg) res,
            cpu_id_start = in(reg) cpu_id_start,
            cpu_id = in(reg) cpu_id,
            usage = in(reg) usage_ptr,
            options(nostack),
        );

        RseqResult::new(res)
    }

    #[stable_api_surface]
    #[inline(always)]
    unsafe fn pop(
        &self,
        list_ptr: *mut *mut Header,
        rseq: &rseq,
        cpu_id: usize,
        usage_ptr: *mut usize,
    ) -> RseqResult {
        let res: *mut Header;

        asm!(
            ".pushsection .data.rel.ro,\"aw\",@progbits",
            ".balign 32",
            "4:",
            ".long 0, 0",
            ".quad 1f",
            ".quad 2f - 1f",
            ".quad 3f",
            ".popsection",

            "lea {res}, [rip + 4b]",
            "mov [{rseq} + {cs_offset}], {res}",
            "1:",
            "cmp dword ptr [{rseq} + {cpu_offset}], {cpu_id:e}",
            "jne 3f",
            "mov {res}, [{list}]",
            "test {res}, {res}",
            "jz 6f",
            "mov {next}, [{res}]",
            // The head store is the commit: label 2 must immediately follow it.
            "mov [{list}], {next}",

            "2:",
            "lock dec qword ptr [{usage}]",
            "jmp 5f",

            "6:",
            // res already holds null; an empty pop must not decrement usage.
            "jmp 5f",

            ".balign 4",
            ".byte 0x0f, 0x1f, 0x05",
            ".long 0x53053053",
            "3:",
            "mov {res}, -1",
            "5:",

            rseq = in(reg) rseq,
            cs_offset = const std::mem::offset_of!(rseq, rseq_cs),
            cpu_offset = const std::mem::offset_of!(rseq, cpu_id),
            list = in(reg) list_ptr,
            res = out(reg) res,
            next = out(reg) _,
            cpu_id = in(reg) cpu_id,
            usage = in(reg) usage_ptr,
            options(nostack),
        );

        RseqResult::new_header(res)
    }
}
