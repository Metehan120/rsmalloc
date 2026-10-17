#![cfg(all(not(feature = "preload"), feature = "native-allocation-api"))]

use std::{num::NonZero, ptr};

use rsmalloc::v2::{
    alloc::{RSMalloc, RSMallocCoreAPI},
    allocation_api::{AllocationAPI, AllocationError, AllocationSize},
    helpers::{Alignment, HelperErrors, need_align_of},
};

// Intentionally not installed as the global allocator.
static ALLOCATOR: RSMalloc = RSMalloc::new_default();
const SLAB_LIMIT: usize = 2 * 1024 * 1024;

fn size(bytes: usize) -> AllocationSize {
    AllocationSize::from_bytes(bytes)
}

fn alignment(bytes: usize) -> NonZero<usize> {
    NonZero::new(bytes).unwrap()
}

unsafe fn fill(pointer: *mut u8, count: usize) {
    for offset in 0..count {
        unsafe {
            pointer
                .add(offset)
                .write((offset as u8).wrapping_mul(37).wrapping_add(19))
        };
    }
}

unsafe fn check(pointer: *mut u8, count: usize) {
    for offset in 0..count {
        assert_eq!(
            unsafe { pointer.add(offset).read() },
            (offset as u8).wrapping_mul(37).wrapping_add(19),
            "offset {offset} of {count}"
        );
    }
}

#[test]
fn native_allocations_work_without_being_the_global_allocator() {
    for bytes in [
        1,
        15,
        16,
        17,
        127,
        128,
        129,
        4095,
        4096,
        4097,
        SLAB_LIMIT,
        SLAB_LIMIT + 1,
    ] {
        let pointer = ALLOCATOR.allocate(size(bytes)).unwrap();
        assert_eq!(pointer.as_ptr() as usize % 16, 0);
        unsafe {
            assert!(ALLOCATOR.usable_size(pointer).unwrap() >= bytes);
            assert!(ALLOCATOR.rs_usable_size(pointer).unwrap() >= bytes);
            fill(pointer.as_ptr(), bytes);
            check(pointer.as_ptr(), bytes);
            ALLOCATOR.deallocate(pointer.as_ptr());
        }
    }
}

#[test]
fn aligned_allocations_cover_small_and_large_boundaries() {
    for bytes in [1, 129, 4097, SLAB_LIMIT + 1] {
        for align in [1, 16, 64, 256, 4096] {
            let pointer = ALLOCATOR
                .allocate_aligned(size(bytes), alignment(align))
                .unwrap();
            assert_eq!(pointer.as_ptr() as usize % align, 0);
            unsafe {
                fill(pointer.as_ptr(), bytes);
                check(pointer.as_ptr(), bytes);
                assert!(ALLOCATOR.usable_size(pointer).unwrap() >= bytes);
                ALLOCATOR.deallocate(pointer.as_ptr());
            }
        }
    }
}

#[test]
fn zeroed_allocations_clear_requested_bytes_after_dirty_allocations() {
    for bytes in [1, 63, 4097, SLAB_LIMIT + 1] {
        let dirty = ALLOCATOR.allocate(size(bytes)).unwrap();
        unsafe {
            dirty.as_ptr().write_bytes(0xa5, bytes);
            ALLOCATOR.deallocate(dirty.as_ptr());
        }
        let zeroed = ALLOCATOR.allocate_zeroed(size(bytes)).unwrap();
        unsafe {
            assert!(
                std::slice::from_raw_parts(zeroed.as_ptr(), bytes)
                    .iter()
                    .all(|&b| b == 0)
            );
            ALLOCATOR.deallocate(zeroed.as_ptr());
        }
    }
    for (count, element) in [(3, 17), (17, 257), (64, 4096)] {
        let pointer = ALLOCATOR
            .allocate_zeroed_nmem(size(count), size(element))
            .unwrap();
        unsafe {
            assert!(
                std::slice::from_raw_parts(pointer.as_ptr(), count * element)
                    .iter()
                    .all(|&b| b == 0)
            );
            ALLOCATOR.deallocate(pointer.as_ptr());
        }
    }
}

#[test]
fn size_tokens_and_counted_allocations_reject_overflow() {
    assert_eq!(AllocationSize::array_bytes::<u64>(17).unwrap().bytes(), 136);
    assert_eq!(
        AllocationSize::array_bytes::<()>(usize::MAX)
            .unwrap()
            .bytes(),
        0
    );
    assert_eq!(
        AllocationSize::array_bytes::<u64>(usize::MAX),
        Err(AllocationError::SizeOverflow)
    );
    assert!(
        ALLOCATOR
            .allocate_zeroed_nmem(size(usize::MAX), size(2))
            .is_err()
    );
    assert_eq!(
        ALLOCATOR.allocate(size(usize::MAX)),
        Err(AllocationError::OutOfMemory)
    );
}

#[test]
fn reallocation_preserves_initialized_prefix_across_backends() {
    for (old_bytes, new_bytes) in [
        (63, 257),
        (4097, 127),
        (4097, SLAB_LIMIT + 1),
        (SLAB_LIMIT + 1, 4097),
    ] {
        let pointer = ALLOCATOR
            .allocate_aligned(size(old_bytes), alignment(256))
            .unwrap();
        unsafe {
            fill(pointer.as_ptr(), old_bytes);
            let grown = ALLOCATOR
                .reallocate(pointer.as_ptr(), size(new_bytes))
                .unwrap();
            assert!(!grown.is_null());
            assert_eq!(grown as usize % 256, 0);
            check(grown, old_bytes.min(new_bytes));
            fill(grown, new_bytes);
            check(grown, new_bytes);
            ALLOCATOR.deallocate(grown);
        }
    }
}

#[test]
fn explicit_reallocation_alignment_is_honored() {
    for (bytes, align) in [(32, 4096), (128, 256), (8192, 64), (SLAB_LIMIT + 1, 4096)] {
        let pointer = ALLOCATOR.allocate(size(128)).unwrap();
        unsafe {
            fill(pointer.as_ptr(), 128);
            let resized = ALLOCATOR
                .aligned_reallocate(pointer.as_ptr(), size(bytes), alignment(align))
                .unwrap();
            assert!(!resized.is_null());
            assert_eq!(resized as usize % align, 0);
            check(resized, bytes.min(128));
            ALLOCATOR.deallocate(resized);
        }
    }
}

#[test]
fn failed_requests_leave_the_original_allocation_live() {
    let pointer = ALLOCATOR
        .allocate_aligned(size(128), alignment(64))
        .unwrap();
    unsafe {
        fill(pointer.as_ptr(), 128);
        for align in [3, 6, 24] {
            assert_eq!(
                ALLOCATOR.allocate_aligned(size(128), alignment(align)),
                Err(AllocationError::InvalidAlignment)
            );
            for bytes in [0, 256] {
                assert_eq!(
                    ALLOCATOR.aligned_reallocate(pointer.as_ptr(), size(bytes), alignment(align)),
                    Err(AllocationError::InvalidAlignment)
                );
                check(pointer.as_ptr(), 128);
            }
        }
        assert_eq!(
            ALLOCATOR.reallocate(pointer.as_ptr(), size(usize::MAX)),
            Err(AllocationError::OutOfMemory)
        );
        check(pointer.as_ptr(), 128);
        assert_eq!(
            ALLOCATOR.aligned_reallocate(pointer.as_ptr(), size(usize::MAX), alignment(256)),
            Err(AllocationError::OutOfMemory)
        );
        check(pointer.as_ptr(), 128);
        fill(pointer.as_ptr(), 128);
        ALLOCATOR.deallocate(pointer.as_ptr());
    }
}

#[test]
fn null_and_zero_size_reallocation_follow_native_contract() {
    unsafe {
        ALLOCATOR.deallocate(ptr::null_mut());
        assert_eq!(
            ALLOCATOR.reallocate(ptr::null_mut(), size(0)),
            Err(AllocationError::NotSupported)
        );
        assert_eq!(
            ALLOCATOR.aligned_reallocate(ptr::null_mut(), size(0), alignment(64)),
            Err(AllocationError::NotSupported)
        );
        let pointer = ALLOCATOR.reallocate(ptr::null_mut(), size(129)).unwrap();
        assert!(!pointer.is_null());
        fill(pointer, 129);
        assert!(ALLOCATOR.reallocate(pointer, size(0)).unwrap().is_null());

        let pointer = ALLOCATOR
            .aligned_reallocate(ptr::null_mut(), size(129), alignment(256))
            .unwrap();
        assert!(!pointer.is_null());
        assert_eq!(pointer as usize % 256, 0);
        assert!(
            ALLOCATOR
                .aligned_reallocate(pointer, size(0), alignment(256))
                .unwrap()
                .is_null()
        );
    }
}

#[test]
fn successful_zero_size_allocations_are_deallocatable() {
    for result in [
        ALLOCATOR.allocate(size(0)),
        ALLOCATOR.allocate_zeroed(size(0)),
        ALLOCATOR.allocate_aligned(size(0), alignment(256)),
    ] {
        match result {
            Ok(pointer) => unsafe { ALLOCATOR.deallocate(pointer.as_ptr()) },
            Err(error) => assert_eq!(error, AllocationError::OutOfMemory),
        }
    }
}

#[test]
fn alignment_helpers_accept_powers_of_two_and_reject_other_values() {
    for align in [1, 2, 16, 64, 4096] {
        assert_eq!(Alignment::new_align(align).unwrap().get(), align);
    }
    for align in [0, 3, 6, 24] {
        assert!(
            matches!(Alignment::new_align(align), Err(HelperErrors::InvalidAlignment { alignment }) if alignment == align)
        );
        assert!(need_align_of::<u64>(align).is_err());
    }
    #[repr(align(256))]
    struct Aligned;
    assert_eq!(Alignment::align_of::<Aligned>().unwrap().get(), 256);
    assert!(need_align_of::<Aligned>(16).unwrap());
    assert!(!need_align_of::<Aligned>(256).unwrap());
}
