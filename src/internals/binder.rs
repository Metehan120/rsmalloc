use std::os::raw::c_void;

use syscalls::{Sysno, syscall6};

const MPOL_PREFERRED: i32 = 1;
const MPOL_F_STATIC_NODES: usize = 1 << 15;

#[inline(always)]
unsafe fn node_mask_word(node_id: u16) -> usize {
    1usize.wrapping_shl(node_id as u32)
}

pub unsafe fn prefer_node(ptr: *mut c_void, len: usize, node_id: u16) -> bool {
    if node_id as usize >= usize::BITS as usize {
        return false;
    }

    let mask = node_mask_word(node_id);
    let maxnode = node_id as usize + 1;

    syscall6(
        Sysno::mbind,
        ptr as usize,
        len,
        MPOL_PREFERRED as usize,
        &mask as *const usize as usize,
        maxnode,
        MPOL_F_STATIC_NODES,
    )
    .is_ok()
}
