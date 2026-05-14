use std::{arch::asm, usize};

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
        usage_ptr: *mut usize,
        rseq: &rseq,
        header: *mut Header,
        tail: *mut Header,
        batch_size: usize,
    ) -> usize {
        let res: usize;
        let cs = get_cs_ptr(rseq);

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
            "mov {tmp}, [{list}]",
            "mov [{tail}], {tmp}",
            "add qword ptr [{usage}], {batch_size}",
            "mov [{list}], {header}",

            "2:",
            "mov qword ptr [{cs_ptr}], 0",
            "mov {res}, 1",
            "jmp 5f",

            ".balign 4",
            ".byte 0x0f, 0x1f, 0x05",
            ".long 0x53053053",
            "3:",
            "mov qword ptr [{cs_ptr}], 0",
            "mov {res}, -1",

            "5:",

            cs_ptr = in(reg) cs,
            tmp = out(reg) _,
            list = in(reg) list_ptr,
            header = in(reg) header,
            res = out(reg) res,
            usage = in(reg) usage_ptr,
            tail = in(reg) tail,
            batch_size = in(reg) batch_size,
            options(nostack, preserves_flags),
        );

        res
    }

    #[inline(always)]
    unsafe fn push(
        &self,
        list_ptr: *mut *mut Header,
        usage_ptr: *mut usize,
        rseq: &rseq,
        header: *mut Header,
    ) -> usize {
        let res: usize;
        let cs = get_cs_ptr(rseq);

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
            "mov {tmp}, [{list}]",
            "mov [{header}], {tmp}",
            "inc qword ptr [{usage}]",
            "mov [{list}], {header}",

            "2:",
            "mov qword ptr [{cs_ptr}], 0",
            "mov {res}, 1",
            "jmp 5f",

            ".balign 4",
            ".byte 0x0f, 0x1f, 0x05",
            ".long 0x53053053",
            "3:",
            "mov qword ptr [{cs_ptr}], 0",
            "mov {res}, -1",

            "5:",

            cs_ptr = in(reg) cs,
            tmp = out(reg) _,
            list = in(reg) list_ptr,
            header = in(reg) header,
            res = out(reg) res,
            usage = in(reg) usage_ptr,
            options(nostack, preserves_flags),
        );

        res
    }

    #[inline(always)]
    unsafe fn pop(
        &self,
        list_ptr: *mut *mut Header,
        usage_ptr: *mut usize,
        rseq: &rseq,
    ) -> *mut Header {
        let res: *mut Header;
        let cs = get_cs_ptr(rseq);

        asm!(
            ".pushsection .data.rel.ro,\"aw\",@progbits",
            ".balign 32",
            "4:",
            ".long 0, 0",
            ".quad 1f",
            ".quad 2f - 1f",
            ".quad 3f",
            ".popsection",

            "test {list}, {list}",
            "jz 7f",

            "lea {tmp}, [rip + 4b]",
            "mov [{cs_ptr}], {tmp}",

            "1:",
            "mov {tmp}, [{list}]",
            "test {tmp}, {tmp}",
            "jz 6f",

            "mov {next}, [{tmp}]",
            "dec qword ptr [{usage}]",
            "mov [{list}], {next}",

            "2:",
            "mov qword ptr [{cs_ptr}], 0",
            "mov {res}, {tmp}",
            "jmp 5f",

            "6:",
            "mov qword ptr [{cs_ptr}], 0",
            "mov {res}, 0",
            "jmp 5f",

            "7:",
            "mov {res}, 0",
            "jmp 5f",

            ".balign 4",
            ".byte 0x0f, 0x1f, 0x05",
            ".long 0x53053053",
            "3:",
            "mov qword ptr [{cs_ptr}], 0",
            "mov {res}, -1",

            "5:",

            cs_ptr = in(reg) cs,
            tmp = out(reg) _,
            list = in(reg) list_ptr,
            res = out(reg) res,
            usage = in(reg) usage_ptr,
            next = out(reg) _,
            options(nostack, preserves_flags),
        );

        res
    }
}
