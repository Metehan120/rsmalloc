use std::ptr::null_mut;

use rustix::rand::{GetRandomFlags, getrandom};

#[cfg(not(feature = "legacy-glibc-support"))]
use crate::rseq_core::rseq_main::__rseq_offset;
use crate::{
    ALIGN_TAG, BIG_MAGIC, BUDDY_ATTEMPT_HUGE, BUDDY_MAX_CACHE, ENABLE_TRIM, FREED_MAGIC, MAGIC,
    RS_DISABLE_THP, RSMallocError,
    big_allocations::buddy::BIG_BUDDY_ALLOCATOR,
    core_prim::predictor::{DEFAULT_BATCH, EMA_ALPHA, PREDICTOR_INIT_BATCH},
    inner::alloc::MAX_REFILL_RETRIES,
    internals::{
        env::{get_env_f32, get_env_usize},
        l3_main_radix::{L3_RADIX, RadixTree},
    },
    rseq_core::{rseq_cache::RSEQ_CACHE, rseq_main::__rseq_size},
};

#[cfg(feature = "legacy-glibc-support")]
use crate::rseq_core::rseq_main::__rseq_offset;

#[inline(never)]
unsafe fn init_magic() {
    #[cfg(not(feature = "extended-header"))]
    {
        let mut main = 0u16.to_le_bytes();

        match getrandom(&mut main, GetRandomFlags::empty()) {
            Ok(_) => {
                let small = u16::from_le_bytes(main);
                let medium = small.wrapping_sub(1);
                let large = medium.wrapping_sub(1);

                MAGIC = small;
                FREED_MAGIC = medium;
                BIG_MAGIC = large;
            }
            Err(err) => RSMallocError::SecurityViolation.log_and_abort(
                null_mut(),
                "calling getrandom failed, cannot initialize magic",
                Some(err.raw_os_error()),
            ),
        };
    }

    #[cfg(feature = "extended-header")]
    {
        let mut main = 0u64.to_le_bytes();
        match getrandom(&mut main, GetRandomFlags::empty()) {
            Ok(_) => {
                let small = u64::from_le_bytes(main);
                let medium = small.wrapping_sub(1);
                let large = medium.wrapping_sub(1);

                MAGIC = small;
                FREED_MAGIC = medium;
                BIG_MAGIC = large;
            }
            Err(err) => RSMallocError::SecurityViolation.log_and_abort(
                null_mut(),
                "calling getrandom failed, cannot initialize magic",
                Some(err.raw_os_error()),
            ),
        };
    }
}

unsafe fn init_align() {
    let mut main = 0usize.to_le_bytes();
    match getrandom(&mut main, GetRandomFlags::empty()) {
        Ok(_) => {
            ALIGN_TAG = usize::from_le_bytes(main);
        }
        Err(err) => RSMallocError::SecurityViolation.log_and_abort(
            null_mut(),
            "calling getrandom failed, cannot initialize align tag",
            Some(err.raw_os_error()),
        ),
    };
}

#[cfg(feature = "canary")]
unsafe fn init_canary() {
    #[cfg(not(feature = "extended-header"))]
    {
        let mut main = 0u8.to_le_bytes();
        match getrandom(&mut main, GetRandomFlags::empty()) {
            Ok(_) => {
                crate::RANDOMC_CANARY_CONSTANT = u8::from_le_bytes(main);
            }
            Err(err) => RSMallocError::SecurityViolation.log_and_abort(
                null_mut(),
                "calling getrandom failed, cannot initialize canary",
                Some(err.raw_os_error()),
            ),
        };
    }

    #[cfg(feature = "extended-header")]
    {
        let mut main = 0u64.to_le_bytes();
        match getrandom(&mut main, GetRandomFlags::empty()) {
            Ok(_) => {
                crate::RANDOMC_CANARY_CONSTANT = u64::from_le_bytes(main);
            }
            Err(err) => RSMallocError::SecurityViolation.log_and_abort(
                null_mut(),
                "calling getrandom failed, cannot initialize canary",
                Some(err.raw_os_error()),
            ),
        };
    }
}

#[inline(never)]
pub unsafe fn bootstrap() {
    #[cfg(feature = "legacy-glibc-support")]
    if __rseq_offset.is_null() || __rseq_size.is_null() {
        use crate::core_prim::rseq_register::register_rseq_raw;

        if register_rseq_raw() == 0 {
            crate::IS_RSEQ_INTERNAL = true;
        } else {
            RSMallocError::RSEQRegFailed.log_and_abort(
                null_mut(),
                "RSEQ register failed, cannot initialize rseq cache. No kernel RSEQ support",
                None,
            );
        }
    }

    #[cfg(not(feature = "legacy-glibc-support"))]
    if __rseq_size.is_null() || __rseq_offset.is_null() {
        RSMallocError::RSEQRegFailed.log_and_abort(
            null_mut(),
            "RSEQ register failed, cannot initialize rseq cache.",
            None,
        );
    }

    MAX_REFILL_RETRIES = get_env_usize("RS_MAX_REFILL_RETRIES".as_bytes()).unwrap_or(3);

    EMA_ALPHA = get_env_f32("RS_EMA_ALPHA".as_bytes())
        .unwrap_or(0.15)
        .clamp(0.05, 0.25);

    let predictor = get_env_usize("RS_PREDICTOR_INIT_BATCH".as_bytes()).unwrap_or(DEFAULT_BATCH);
    PREDICTOR_INIT_BATCH = predictor;

    L3_RADIX = RadixTree::new();
    RSEQ_CACHE.ensure_cache();
    BUDDY_MAX_CACHE = get_env_usize("RS_BUDDY_PER_CACHE_SIZE".as_bytes())
        .unwrap_or(1024 * 1024 * 256)
        .clamp(1024 * 1024 * 256, 2 << 46)
        .next_power_of_two();

    BUDDY_ATTEMPT_HUGE = get_env_usize("RS_BUDDY_ATTEMPT_HUGEPAGE".as_bytes()).unwrap_or(0) != 0;
    ENABLE_TRIM = get_env_usize("RS_ENABLE_TRIM".as_bytes()).unwrap_or(0) != 0;

    let disable_thp = get_env_usize("RS_DISABLE_THP".as_bytes()).unwrap_or(0);
    RS_DISABLE_THP = disable_thp == 1;

    BIG_BUDDY_ALLOCATOR.init(BUDDY_MAX_CACHE, BUDDY_ATTEMPT_HUGE && !RS_DISABLE_THP);

    crate::core_prim::fork::register_fork_handlers();

    let random_magic = get_env_usize("RS_DISABLE_RANDOMIZING".as_bytes()).unwrap_or(0) == 0;
    if random_magic {
        init_magic();
        init_align();

        if ALIGN_TAG == 0 {
            while ALIGN_TAG == 0 {
                init_align();
            }
        }
    };

    #[cfg(feature = "canary")]
    init_canary();
}
