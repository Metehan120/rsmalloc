#![allow(unused)]

use std::sync::atomic;

pub enum AtomicOrdering {
    TransferFetchOp,
    TransferFetchLoad,
    Acquire,
    Release,
    Relaxed,
    SeqCst,
}

impl AtomicOrdering {
    #[inline(always)]
    const fn to_atomic_ordering<const IS_FETCH_OP: bool>(self) -> atomic::Ordering {
        match self {
            AtomicOrdering::TransferFetchOp => {
                if !IS_FETCH_OP {
                    panic!("a FetchOp ordering in non-RMW operation")
                }

                #[cfg(target_arch = "aarch64")]
                {
                    return atomic::Ordering::AcqRel;
                }
                #[cfg(target_arch = "x86_64")]
                {
                    return atomic::Ordering::Relaxed;
                }
            }
            AtomicOrdering::TransferFetchLoad => {
                #[cfg(target_arch = "aarch64")]
                {
                    return atomic::Ordering::Acquire;
                }
                #[cfg(target_arch = "x86_64")]
                {
                    return atomic::Ordering::Relaxed;
                }
            }
            AtomicOrdering::Acquire => atomic::Ordering::Acquire,
            AtomicOrdering::Release => atomic::Ordering::Release,
            AtomicOrdering::Relaxed => atomic::Ordering::Relaxed,
            AtomicOrdering::SeqCst => atomic::Ordering::SeqCst,
        }
    }
}

mod atomicusize;
pub use atomicusize::*;
