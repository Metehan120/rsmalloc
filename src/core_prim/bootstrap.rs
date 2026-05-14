use crate::{
    internals::l3_main_radix::{L3_RADIX, RadixTree},
    rseq_core::rseq_cache::RSEQ_CACHE,
};

#[inline(never)]
pub unsafe fn bootstrap() {
    L3_RADIX = unsafe { RadixTree::new() };
    RSEQ_CACHE.ensure_cache();
}
