// This test written by CODEX
//
// Good enough, leave it as is

#[cfg(not(feature = "preload"))]
#[cfg(test)]
mod tests {
    use std::alloc::{GlobalAlloc, Layout};

    use rsmalloc::v2::alloc::{RSMalloc, RSMallocRaw, RawInterface};

    #[global_allocator]
    static GLOBAL: RSMalloc = RSMalloc::new_default();
    static RAW: RSMallocRaw = GLOBAL.raw();

    #[test]
    fn global_allocator_handles_standard_collections() {
        let mut values = Vec::new();
        for i in 0..10_000usize {
            values.push(i);
        }

        assert_eq!(values.len(), 10_000);
        assert_eq!(values[1234], 1234);

        let text = String::from("rsmalloc global allocator smoke test");
        assert!(text.contains("rsmalloc"));
    }

    #[test]
    fn alloc_zeroed_returns_zeroed_memory() {
        let layout = Layout::from_size_align(4096, 16).unwrap();
        let ptr = unsafe { GLOBAL.alloc_zeroed(layout) };
        assert!(!ptr.is_null());

        unsafe {
            for i in 0..layout.size() {
                assert_eq!(*ptr.add(i), 0, "non-zero byte at offset {i}");
            }
            GLOBAL.dealloc(ptr, layout);
        }
    }

    #[test]
    fn over_aligned_realloc_preserves_alignment_and_prefix() {
        let old_layout = Layout::from_size_align(256, 128).unwrap();
        let ptr = unsafe { GLOBAL.alloc(old_layout) };
        assert!(!ptr.is_null());
        assert_eq!((ptr as usize) % old_layout.align(), 0);

        unsafe {
            for i in 0..old_layout.size() {
                *ptr.add(i) = i as u8;
            }
        }

        let new_size = 4096;
        let new_ptr = unsafe { GLOBAL.realloc(ptr, old_layout, new_size) };
        assert!(!new_ptr.is_null());
        assert_eq!((new_ptr as usize) % old_layout.align(), 0);

        unsafe {
            for i in 0..old_layout.size() {
                assert_eq!(*new_ptr.add(i), i as u8, "byte mismatch at offset {i}");
            }

            let new_layout = Layout::from_size_align(new_size, old_layout.align()).unwrap();
            GLOBAL.dealloc(new_ptr, new_layout);
        }
    }

    #[test]
    fn realloc_reuses_pointer_for_compatible_alignment_changes() {
        unsafe {
            let ptr = RAW.rs_aligned(256, 1024);
            assert!(!ptr.is_null());
            ptr.write_bytes(0x5a, 1024);
            let shrunk = RAW.rs_realloc(ptr, 128, Some(32));
            assert_eq!(shrunk, ptr);
            let grown = RAW.rs_realloc(shrunk, 768, Some(256));
            assert_eq!(grown, ptr);
            assert!(
                std::slice::from_raw_parts(grown, 128)
                    .iter()
                    .all(|&b| b == 0x5a)
            );
            RAW.rs_free(grown);
        }
    }

    #[test]
    fn realloc_moves_when_pointer_does_not_meet_new_alignment() {
        unsafe {
            for new_size in [32, 8192] {
                let ptr = RAW.rs_alloc(128);
                assert!(!ptr.is_null());
                ptr.write_bytes(0x3c, 128);
                let alignment = 1usize << ((ptr as usize).trailing_zeros() + 1);
                let resized = RAW.rs_realloc(ptr, new_size, Some(alignment));
                assert!(!resized.is_null());
                assert_ne!(resized, ptr);
                assert_eq!(resized as usize % alignment, 0);
                assert!(
                    std::slice::from_raw_parts(resized, new_size.min(128))
                        .iter()
                        .all(|&b| b == 0x3c)
                );
                RAW.rs_free(resized);
            }
        }
    }

    #[test]
    fn realloc_none_preserves_observed_alignment_on_growth() {
        unsafe {
            let ptr = RAW.rs_aligned(256, 128);
            assert!(!ptr.is_null());
            let alignment = 1usize << (ptr as usize).trailing_zeros();
            ptr.write_bytes(0x7e, 128);
            let resized = RAW.rs_realloc(ptr, 8192, None);
            assert!(!resized.is_null());
            assert_eq!(resized as usize % alignment, 0);
            assert!(
                std::slice::from_raw_parts(resized, 128)
                    .iter()
                    .all(|&b| b == 0x7e)
            );
            RAW.rs_free(resized);
        }
    }

    #[test]
    fn realloc_invalid_alignment_and_failure_preserve_allocation() {
        unsafe {
            let ptr = RAW.rs_aligned(64, 128);
            assert!(!ptr.is_null());
            ptr.write_bytes(0x91, 128);
            for alignment in [0, 3] {
                assert!(RAW.rs_realloc(ptr, 256, Some(alignment)).is_null());
            }
            assert!(RAW.rs_realloc(ptr, usize::MAX, Some(256)).is_null());
            assert!(
                std::slice::from_raw_parts(ptr, 128)
                    .iter()
                    .all(|&b| b == 0x91)
            );
            RAW.rs_free(ptr);
        }
    }

    #[test]
    fn realloc_null_honors_optional_alignment() {
        unsafe {
            for alignment in [None, Some(256)] {
                let ptr = RAW.rs_realloc(std::ptr::null_mut(), 128, alignment);
                assert!(!ptr.is_null());
                assert_eq!(ptr as usize % alignment.unwrap_or(16), 0);
                assert!(RAW.rs_realloc(ptr, 0, alignment).is_null());
            }
        }
    }

    #[test]
    fn allocation_api_reallocate_keeps_its_alignment_preserving_interface() {
        use rsmalloc::v2::allocation_api::{AllocationAPI, AllocationSize};
        unsafe {
            let ptr = GLOBAL
                .allocate_aligned(AllocationSize::from_bytes(128), 256)
                .unwrap();
            ptr.as_ptr().write_bytes(0x48, 128);
            let resized = GLOBAL
                .reallocate(ptr, AllocationSize::from_bytes(8192))
                .unwrap();
            assert_eq!(resized.as_ptr() as usize % 256, 0);
            assert!(
                std::slice::from_raw_parts(resized.as_ptr(), 128)
                    .iter()
                    .all(|&b| b == 0x48)
            );
            GLOBAL.deallocate(resized);
        }
    }

    #[test]
    fn direct_rsmalloc_helpers_work() {
        unsafe {
            let ptr = RAW.rs_alloc(256);
            assert!(!ptr.is_null());
            for i in 0..256 {
                *ptr.add(i) = i as u8;
            }

            let ptr = RAW.rs_realloc(ptr, 512, None);
            assert!(!ptr.is_null());
            for i in 0..256 {
                assert_eq!(*ptr.add(i), i as u8, "byte mismatch at offset {i}");
            }
            RAW.rs_free(ptr);

            let aligned = RAW.rs_aligned(128, 512);
            assert!(!aligned.is_null());
            assert_eq!((aligned as usize) % 128, 0);
            RAW.rs_free(aligned);
        }
    }
}
