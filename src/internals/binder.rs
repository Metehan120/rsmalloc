use std::os::raw::{c_int, c_ulong, c_void};

use libc::{SYS_mbind, syscall};

const MPOL_PREFERRED: c_int = 1;
const MPOL_F_STATIC_NODES: c_ulong = 1 << 15;

#[inline(always)]
unsafe fn node_mask_word(node_id: u16) -> c_ulong {
    1usize.wrapping_shl(node_id as u32) as c_ulong
}

pub unsafe fn prefer_node(ptr: *mut c_void, len: usize, node_id: u16) -> bool {
    if node_id as usize >= usize::BITS as usize {
        return false;
    }

    let mask = node_mask_word(node_id);
    let maxnode = node_id as c_ulong + 1;

    syscall(
        SYS_mbind,
        ptr,
        len,
        MPOL_PREFERRED,
        &mask as *const c_ulong,
        maxnode,
        MPOL_F_STATIC_NODES,
    ) == 0
}
