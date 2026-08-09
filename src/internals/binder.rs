use std::os::raw::c_void;

use syscalls::{Sysno, syscall};

const MPOL_PREFERRED: usize = 1;
const MPOL_BIND: usize = 2;
const MPOL_F_STATIC_NODES: usize = 1 << 15;

#[inline(always)]
unsafe fn node_mask_word(node_id: u16) -> usize {
    1usize.wrapping_shl(node_id as u32)
}

#[inline(always)]
unsafe fn mbind_node(ptr: *mut c_void, len: usize, node_id: u16, mode: usize) -> bool {
    if node_id as usize >= usize::BITS as usize {
        return false;
    }

    let mask = node_mask_word(node_id);
    let maxnode = node_id as usize + 1;

    syscall!(
        Sysno::mbind,
        ptr as usize,
        len,
        mode,
        &mask as *const usize as usize,
        maxnode,
        MPOL_F_STATIC_NODES
    )
    .is_ok()
}

#[inline(always)]
pub unsafe fn prefer_node(ptr: *mut c_void, len: usize, node_id: u16) -> bool {
    mbind_node(ptr, len, node_id, MPOL_PREFERRED)
}

#[inline(always)]
pub unsafe fn bind_node(ptr: *mut c_void, len: usize, node_id: u16) -> bool {
    mbind_node(ptr, len, node_id, MPOL_BIND)
}
