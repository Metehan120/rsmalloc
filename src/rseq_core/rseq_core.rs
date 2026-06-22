// ! DO NOT TOUCH, CHANGE OR BREATHE NEAR ASSEMBLY unless you know how rseq or assembly works !

use std::{arch::asm, ptr::addr_of, usize};

use crate::{
    Header, RseqCoreTrait,
    rseq_core::rseq_main::{get_cs_ptr, rseq},
};

pub struct RseqCore;

impl RseqCoreTrait for RseqCore {
    #[inline(always)]
    unsafe fn push_tailed(
        &self,
        list_ptr: *mut *mut Header,
        rseq: &rseq,
        cpu_id: usize,
        header: *mut Header,
        tail: *mut Header,
    ) -> usize {
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
            "mov qword ptr [{cs_ptr}], 0",
            "mov {res}, 1",
            "jmp 5f",

            ".balign 4",
            ".byte 0x0f, 0x1f, 0x05",
            // RSEQ abort signature, matches glibc/linux rseq convention.
            ".long 0x53053053",
            "3:",
            "mov qword ptr [{cs_ptr}], 0",
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
            options(nostack),
        );

        res
    }

    #[inline(always)]
    unsafe fn push(
        &self,
        list_ptr: *mut *mut Header,
        rseq: &rseq,
        cpu_id: usize,
        header: *mut Header,
    ) -> usize {
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
            "mov qword ptr [{cs_ptr}], 0",
            "mov {res}, 1",
            "jmp 5f",

            ".balign 4",
            ".byte 0x0f, 0x1f, 0x05",
            // RSEQ abort signature, matches glibc/linux rseq convention.
            ".long 0x53053053",
            "3:",
            "mov qword ptr [{cs_ptr}], 0",
            "mov {res}, -1",

            "5:",

            cs_ptr = in(reg) cs,
            tmp = out(reg) _,
            list = in(reg) list_ptr,
            header = in(reg) header,
            res = lateout(reg) res,
            cpu_id_start = in(reg) cpu_id_start,
            cpu_id = in(reg) cpu_id,
            options(nostack),
        );

        res
    }

    #[inline(always)]
    unsafe fn pop(&self, list_ptr: *mut *mut Header, rseq: &rseq, cpu_id: usize) -> *mut Header {
        let res: *mut Header;
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
            "test {tmp}, {tmp}",
            "jz 6f",

            "mov {next}, [{tmp}]",
            // register to register test, test and jz should be fused into single uop.
            "test {next}, {next}",
            "jz 7f",
            // prefetcht0 next caller likely touches the next header/cacheline soon.
            "prefetcht0 [{next}]",
            "7:",
            "mov [{list}], {next}",

            "2:",
            "mov qword ptr [{cs_ptr}], 0",
            "mov {res}, {tmp}",
            "jmp 5f",

            "6:",
            "mov qword ptr [{cs_ptr}], 0",
            // xor reg, reg: zero-idiom; smaller and breaks dependency chain.
            "xor {res}, {res}",
            "jmp 5f",

            ".balign 4",
            ".byte 0x0f, 0x1f, 0x05",
            // RSEQ abort signature, matches glibc/linux rseq convention.
            ".long 0x53053053",
            "3:",
            "mov qword ptr [{cs_ptr}], 0",
            "mov {res}, -1",

            "5:",

            cs_ptr = in(reg) cs,
            tmp = out(reg) _,
            list = in(reg) list_ptr,
            res = lateout(reg) res,
            next = out(reg) _,
            cpu_id_start = in(reg) cpu_id_start,
            cpu_id = in(reg) cpu_id,
            options(nostack),
        );

        res
    }
}
