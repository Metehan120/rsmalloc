use rsmalloc_macro::stable_api_surface;
use rustix::rand::{GetRandomFlags, getrandom};

use crate::{ALIGN_TAG, BIG_MAGIC, FREED_MAGIC, MAGIC, RSMallocError};

#[stable_api_surface]
#[inline(never)]
pub unsafe fn init_magic() {
    #[cfg(not(feature = "extended-header"))]
    {
        let mut main = 0u16.to_le_bytes();
        let mut freed = 0u16.to_le_bytes();
        let mut big = 0u16.to_le_bytes();

        if let Some(err) = getrandom(&mut main, GetRandomFlags::empty()).err() {
            RSMallocError::SecurityViolation {
                reason: "calling getrandom failed, cannot initialize freed magic",
                errno: Some(err.raw_os_error()),
            }
            .log_and_abort();
        }

        if let Some(err) = getrandom(&mut freed, GetRandomFlags::empty()).err() {
            RSMallocError::SecurityViolation {
                reason: "calling getrandom failed, cannot initialize freed magic",
                errno: Some(err.raw_os_error()),
            }
            .log_and_abort();
        }

        if let Some(err) = getrandom(&mut big, GetRandomFlags::empty()).err() {
            RSMallocError::SecurityViolation {
                reason: "calling getrandom failed, cannot initialize freed magic",
                errno: Some(err.raw_os_error()),
            }
            .log_and_abort();
        }

        while freed == main {
            if let Some(err) = getrandom(&mut freed, GetRandomFlags::empty()).err() {
                RSMallocError::SecurityViolation {
                    reason: "calling getrandom failed, cannot initialize freed magic",
                    errno: Some(err.raw_os_error()),
                }
                .log_and_abort();
            }
        }

        while big == freed || big == main {
            if let Some(err) = getrandom(&mut big, GetRandomFlags::empty()).err() {
                RSMallocError::SecurityViolation {
                    reason: "calling getrandom failed, cannot initialize freed magic",
                    errno: Some(err.raw_os_error()),
                }
                .log_and_abort();
            }
        }

        MAGIC = u16::from_le_bytes(main);
        FREED_MAGIC = u16::from_le_bytes(freed);
        BIG_MAGIC = u16::from_le_bytes(big);
    }

    #[cfg(feature = "extended-header")]
    {
        let mut main = 0u64.to_le_bytes();
        let mut freed = 0u64.to_le_bytes();
        let mut big = 0u64.to_le_bytes();

        if let Some(err) = getrandom(&mut main, GetRandomFlags::empty()).err() {
            RSMallocError::SecurityViolation {
                reason: "calling getrandom failed, cannot initialize magic",
                errno: Some(err.raw_os_error()),
            }
            .log_and_abort();
        }

        if let Some(err) = getrandom(&mut freed, GetRandomFlags::empty()).err() {
            RSMallocError::SecurityViolation {
                reason: "calling getrandom failed, cannot initialize freed magic",
                errno: Some(err.raw_os_error()),
            }
            .log_and_abort();
        }

        if let Some(err) = getrandom(&mut big, GetRandomFlags::empty()).err() {
            RSMallocError::SecurityViolation {
                reason: "calling getrandom failed, cannot initialize big magic",
                errno: Some(err.raw_os_error()),
            }
            .log_and_abort();
        }

        while freed == main {
            if let Some(err) = getrandom(&mut freed, GetRandomFlags::empty()).err() {
                RSMallocError::SecurityViolation {
                    reason: "calling getrandom failed, cannot initialize freed magic",
                    errno: Some(err.raw_os_error()),
                }
                .log_and_abort();
            }
        }

        while big == freed || big == main {
            if let Some(err) = getrandom(&mut big, GetRandomFlags::empty()).err() {
                RSMallocError::SecurityViolation {
                    reason: "calling getrandom failed, cannot initialize big magic",
                    errno: Some(err.raw_os_error()),
                }
                .log_and_abort();
            }
        }

        MAGIC = u64::from_le_bytes(main);
        FREED_MAGIC = u64::from_le_bytes(freed);
        BIG_MAGIC = u64::from_le_bytes(big);
    }
}

#[stable_api_surface]
#[inline(never)]
pub unsafe fn init_align() {
    let mut main = 0usize.to_le_bytes();
    if let Err(err) = getrandom(&mut main, GetRandomFlags::empty()) {
        RSMallocError::SecurityViolation {
            reason: "calling getrandom failed, cannot initialize freed magic",
            errno: Some(err.raw_os_error()),
        }
        .log_and_abort();
    }
    ALIGN_TAG = usize::from_le_bytes(main);
}
