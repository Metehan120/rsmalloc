// Bounded correctness checks for GlobalAlloc and RawInterface.
// Native AllocationAPI coverage belongs in native_api.rs.

#[cfg(not(feature = "preload"))]
#[cfg(test)]
mod tests {
    use std::alloc::{GlobalAlloc, Layout};
    use std::num::NonZero;

    use rsmalloc::v2::alloc::{
        AdvancedTrimSize, RSMalloc, RSMallocCoreAPI, RSMallocRaw, RawInterface, SimpleTrimSize,
    };

    #[global_allocator]
    static GLOBAL: RSMalloc = RSMalloc::new_default();
    static RAW: RSMallocRaw = GLOBAL.raw();

    // Selected size-class edges and 4 KiB page multiples, not an exhaustive sweep.
    const SIZES: &[usize] = &[
        1, 15, 16, 17, 31, 32, 33, 127, 128, 129, 255, 256, 257, 511, 512, 513, 1023, 1024, 1025,
        3840, 4095, 4096, 4097, 8191, 8192, 8193,
    ];
    const ALIGNMENTS: &[usize] = &[1, 16, 64, 256, 4096];

    fn pattern(offset: usize, seed: u8) -> u8 {
        (offset as u8).wrapping_mul(37).wrapping_add(seed)
    }

    // Callers supply a live, writable range of at least `size` bytes.
    unsafe fn fill_pattern(ptr: *mut u8, size: usize, seed: u8) {
        for offset in 0..size {
            unsafe { ptr.add(offset).write(pattern(offset, seed)) };
        }
    }

    // Only initialized prefixes may be passed here, including after realloc growth.
    unsafe fn check_pattern(ptr: *mut u8, size: usize, seed: u8) {
        for offset in 0..size {
            assert_eq!(
                unsafe { ptr.add(offset).read() },
                pattern(offset, seed),
                "pattern mismatch at offset {offset}, size {size}, seed {seed}"
            );
        }
    }

    unsafe fn check_zeroed(ptr: *mut u8, size: usize) {
        for offset in 0..size {
            assert_eq!(
                unsafe { ptr.add(offset).read() },
                0,
                "nonzero byte at offset {offset}, size {size}"
            );
        }
    }

    #[test]
    fn global_alloc_size_and_alignment_boundaries() {
        for &size in SIZES {
            for &align in ALIGNMENTS {
                let layout = Layout::from_size_align(size, align).unwrap();
                unsafe {
                    let ptr = GLOBAL.alloc(layout);
                    assert!(!ptr.is_null(), "size {size}, alignment {align}");
                    assert_eq!(ptr as usize % align, 0);
                    fill_pattern(ptr, size, 0x27);
                    check_pattern(ptr, size, 0x27);
                    GLOBAL.dealloc(ptr, layout);
                }
            }
        }
    }

    #[test]
    fn raw_alloc_and_aligned_size_boundaries() {
        for &size in SIZES {
            unsafe {
                let ptr = RAW.rs_alloc(size);
                assert!(!ptr.is_null(), "size {size}");
                assert_eq!(ptr as usize % 16, 0);
                fill_pattern(ptr, size, 0x49);
                check_pattern(ptr, size, 0x49);
                RAW.rs_free(ptr);
                for &align in ALIGNMENTS {
                    let ptr = RAW.rs_aligned(align, size);
                    assert!(!ptr.is_null(), "size {size}, alignment {align}");
                    assert_eq!(ptr as usize % align, 0);
                    fill_pattern(ptr, size, 0xb3);
                    check_pattern(ptr, size, 0xb3);
                    RAW.rs_free(ptr);
                }
            }
        }
    }

    #[test]
    fn global_realloc_grow_shrink_and_same_size_preserve_prefix() {
        for &align in ALIGNMENTS {
            let mut layout = Layout::from_size_align(17, align).unwrap();
            unsafe {
                let mut ptr = GLOBAL.alloc(layout);
                assert!(!ptr.is_null());
                fill_pattern(ptr, layout.size(), 0x63);
                for size in [17, 33, 129, 4097, 8193, 4096, 128, 1] {
                    let resized = GLOBAL.realloc(ptr, layout, size);
                    assert!(!resized.is_null());
                    assert_eq!(resized as usize % align, 0);
                    check_pattern(resized, layout.size().min(size), 0x63);
                    // Growth leaves the tail uninitialized; initialize before the next check.
                    fill_pattern(resized, size, 0x63);
                    ptr = resized;
                    layout = Layout::from_size_align(size, align).unwrap();
                }
                GLOBAL.dealloc(ptr, layout);
            }
        }
    }

    #[test]
    fn global_allocator_handles_standard_collections() {
        let mut values = Vec::new();
        for i in 0..256usize {
            values.push(i);
        }

        assert_eq!(values.len(), 256);
        assert!(
            values
                .iter()
                .enumerate()
                .all(|(index, &value)| index == value)
        );

        let text = String::from("rsmalloc global allocator smoke test");
        assert!(text.contains("rsmalloc"));
    }

    #[test]
    fn global_alloc_zeroed_after_dirty_free() {
        for size in [17, 128, 513, 4096, 4097, 8193] {
            for align in [16, 256, 4096] {
                let layout = Layout::from_size_align(size, align).unwrap();
                // Repeated same-layout requests give cached dirty blocks reuse opportunities.
                // Exact address reuse is not a public allocation guarantee.
                for _ in 0..4 {
                    unsafe {
                        let dirty = GLOBAL.alloc(layout);
                        assert!(!dirty.is_null());
                        dirty.write_bytes(0xd7, size);
                        GLOBAL.dealloc(dirty, layout);
                        let zeroed = GLOBAL.alloc_zeroed(layout);
                        assert!(!zeroed.is_null());
                        assert_eq!(zeroed as usize % align, 0);
                        check_zeroed(zeroed, size);
                        zeroed.write_bytes(0xe9, size);
                        GLOBAL.dealloc(zeroed, layout);
                    }
                }
            }
        }
    }

    #[test]
    fn raw_zeroed_after_dirty_free() {
        for &size in SIZES {
            for _ in 0..4 {
                unsafe {
                    let dirty = RAW.rs_alloc(size);
                    assert!(!dirty.is_null());
                    dirty.write_bytes(0xa5, size);
                    RAW.rs_free(dirty);
                    let zeroed = RAW.rs_zeroed(size);
                    assert!(!zeroed.is_null());
                    assert_eq!(zeroed as usize % 16, 0);
                    check_zeroed(zeroed, size);
                    zeroed.write_bytes(0x5a, size);
                    RAW.rs_free(zeroed);
                }
            }
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
    fn simultaneous_live_ranges_are_disjoint_and_survive_trim() {
        let sizes = [
            1, 16, 17, 128, 129, 512, 513, 3840, 4095, 4096, 4097, 8193, 32769, 65537, 131073,
            2_097_153,
        ];
        let mut live: Vec<(*mut u8, Layout, u8, bool)> = Vec::with_capacity(sizes.len());
        unsafe {
            for (index, size) in sizes.into_iter().enumerate() {
                let layout =
                    Layout::from_size_align(size, ALIGNMENTS[index % ALIGNMENTS.len()]).unwrap();
                let raw = index % 3 == 0;
                let ptr = if raw {
                    RAW.rs_aligned(layout.align(), size)
                } else {
                    GLOBAL.alloc(layout)
                };
                assert!(!ptr.is_null());
                assert_eq!(ptr as usize % layout.align(), 0);
                let start = ptr as usize;
                let end = start.checked_add(size).unwrap();
                for &(other, other_layout, _, _) in &live {
                    let other_start = other as usize;
                    let other_end = other_start.checked_add(other_layout.size()).unwrap();
                    assert!(
                        end <= other_start || other_end <= start,
                        "overlapping live allocations: {start:#x}..{end:#x} and \
                         {other_start:#x}..{other_end:#x}"
                    );
                }
                let seed = (index as u8).wrapping_mul(13);
                fill_pattern(ptr, size, seed);
                live.push((ptr, layout, seed, raw));
            }

            // Each block was initialized independently; subsequent allocations must
            // not have overwritten any earlier block, even before trimming.
            for &(ptr, layout, seed, _) in &live {
                check_pattern(ptr, layout.size(), seed);
            }

            // Free alternating neighbors to provide reclaimable cache candidates.
            // No assumptions about the amount reclaimed or the address reused.
            for index in (0..live.len()).step_by(2).rev() {
                let (ptr, layout, _, raw) = live.remove(index);
                if raw {
                    RAW.rs_free(ptr);
                } else {
                    GLOBAL.dealloc(ptr, layout);
                }
            }
            let _ = GLOBAL.trim(SimpleTrimSize::Bytes(NonZero::new(4096).unwrap()));
            for &(ptr, layout, seed, _) in &live {
                check_pattern(ptr, layout.size(), seed);
            }
            let _ = RAW.rs_trim(AdvancedTrimSize::All);
            for (ptr, layout, seed, raw) in live {
                check_pattern(ptr, layout.size(), seed);
                if raw {
                    RAW.rs_free(ptr);
                } else {
                    GLOBAL.dealloc(ptr, layout);
                }
            }
        }
    }
}
