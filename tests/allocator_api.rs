// Written by CODEX
//
// Good enough, leave it as is

#![cfg(all(not(feature = "preload"), feature = "allocator-api"))]
#![feature(allocator_api)]

use std::alloc::{Allocator, Layout};

use rsmalloc::v2::alloc::RSMalloc;

#[test]
fn allocator_aware_collections_work() {
    let allocator = RSMalloc::new_default();
    let mut values = Vec::new_in(&allocator);
    values.extend(0..2048usize);
    assert_eq!(values[1234], 1234);
    values.truncate(17);
    values.shrink_to_fit();
    assert_eq!(&values[..], &(0..17).collect::<Vec<_>>());

    #[repr(align(256))]
    struct Aligned(u64);
    let value = Box::new_in(Aligned(42), &allocator);
    assert_eq!(value.0, 42);
    assert_eq!((&*value as *const Aligned as usize) % 256, 0);

    let mut zero_sized = Vec::new_in(&allocator);
    zero_sized.resize(100, ());
    assert_eq!(zero_sized.len(), 100);
}

#[test]
fn allocations_report_requested_size_and_alignment() {
    let allocator = RSMalloc::new_default();
    for size in [0, 1, 63, 4096] {
        for alignment in [1, 16, 64, 4096] {
            let layout = Layout::from_size_align(size, alignment).unwrap();
            let block = allocator.allocate(layout).unwrap();
            assert_eq!(block.len(), size);
            assert_eq!(block.cast::<u8>().as_ptr() as usize % alignment, 0);
            unsafe { allocator.deallocate(block.cast(), layout) };
        }
    }
}

#[test]
fn boundary_allocations_remain_isolated_while_live() {
    let allocator = RSMalloc::new_default();
    // Representative class edges and the 2 MiB slab/large-allocation cutoff.
    for boundary in [16, 128, 4096, 2 * 1024 * 1024] {
        for alignment in [16, 256] {
            let blocks = [boundary - 1, boundary, boundary + 1].map(|size| {
                let layout = Layout::from_size_align(size, alignment).unwrap();
                let block = allocator.allocate(layout).unwrap();
                assert_eq!(block.len(), size);
                assert_eq!(block.cast::<u8>().as_ptr() as usize % alignment, 0);
                (block, layout)
            });
            unsafe {
                for (index, (block, layout)) in blocks.iter().enumerate() {
                    block
                        .cast::<u8>()
                        .as_ptr()
                        .write_bytes(0x51 + index as u8, layout.size());
                    let start = block.cast::<u8>().as_ptr() as usize;
                    let end = start + layout.size();
                    for (other, other_layout) in &blocks[..index] {
                        let other_start = other.cast::<u8>().as_ptr() as usize;
                        assert!(end <= other_start || other_start + other_layout.size() <= start);
                    }
                }
                for (index, (block, _)) in blocks.iter().enumerate() {
                    assert!(block.as_ref().iter().all(|&b| b == 0x51 + index as u8));
                }
                let (middle, layout) = blocks[1];
                allocator.deallocate(middle.cast(), layout);
                let replacement = allocator.allocate_zeroed(layout).unwrap();
                assert_eq!(replacement.len(), layout.size());
                assert_eq!(replacement.cast::<u8>().as_ptr() as usize % alignment, 0);
                assert!(replacement.as_ref().iter().all(|&b| b == 0));
                for index in [0, 2] {
                    let (block, layout) = blocks[index];
                    assert!(block.as_ref().iter().all(|&b| b == 0x51 + index as u8));
                    allocator.deallocate(block.cast(), layout);
                }
                allocator.deallocate(replacement.cast(), layout);
            }
        }
    }
}

#[test]
fn zeroed_allocations_cover_the_reported_block() {
    let allocator = RSMalloc::new_default();
    for alignment in [1, 16, 256] {
        for size in [0, 63, 8192] {
            let layout = Layout::from_size_align(size, alignment).unwrap();
            let dirty = allocator.allocate(layout).unwrap();
            unsafe {
                dirty.cast::<u8>().as_ptr().write_bytes(0xa5, size);
                allocator.deallocate(dirty.cast(), layout);
            }
            let block = allocator.allocate_zeroed(layout).unwrap();
            assert_eq!(block.len(), size);
            assert!(unsafe { block.as_ref() }.iter().all(|&b| b == 0));
            unsafe { allocator.deallocate(block.cast(), layout) };
        }
    }
}

#[test]
fn grow_and_shrink_reuse_suitably_aligned_backing() {
    let allocator = RSMalloc::new_default();
    let original = Layout::from_size_align(1024, 256).unwrap();
    let small = Layout::from_size_align(128, 32).unwrap();
    let large = Layout::from_size_align(768, 256).unwrap();
    unsafe {
        let block = allocator.allocate(original).unwrap();
        let pointer = block.cast::<u8>();
        pointer.as_ptr().write_bytes(0x5a, original.size());
        let shrunk = allocator.shrink(pointer, original, small).unwrap();
        assert_eq!(shrunk.cast::<u8>(), pointer);
        assert_eq!(shrunk.len(), small.size());
        let grown = allocator.grow(shrunk.cast(), small, large).unwrap();
        assert_eq!(grown.cast::<u8>(), pointer);
        assert_eq!(grown.len(), large.size());
        assert!(grown.as_ref()[..small.size()].iter().all(|&b| b == 0x5a));
        allocator.deallocate(grown.cast(), large);
    }
}

#[test]
fn resize_moves_for_incompatible_alignment_including_equal_size() {
    let allocator = RSMalloc::new_default();
    let old_layout = Layout::from_size_align(128, 16).unwrap();
    for new_size in [32, 128, 4096] {
        unsafe {
            let block = allocator.allocate(old_layout).unwrap();
            let ptr = block.cast::<u8>();
            ptr.as_ptr().write_bytes(0x39, old_layout.size());
            let alignment = 1usize << ((ptr.as_ptr() as usize).trailing_zeros() + 1);
            let new_layout = Layout::from_size_align(new_size, alignment).unwrap();
            let resized = if new_size < old_layout.size() {
                allocator.shrink(ptr, old_layout, new_layout)
            } else {
                allocator.grow(ptr, old_layout, new_layout)
            }
            .unwrap();
            assert_ne!(resized.cast::<u8>(), ptr);
            assert_eq!(resized.cast::<u8>().as_ptr() as usize % alignment, 0);
            assert!(
                resized.as_ref()[..new_size.min(old_layout.size())]
                    .iter()
                    .all(|&b| b == 0x39)
            );
            allocator.deallocate(resized.cast(), new_layout);
        }
    }
}

#[test]
fn grow_zeroed_clears_retained_capacity_from_old_logical_size() {
    let allocator = RSMalloc::new_default();
    for (backing_alignment, small_alignment) in [(16, 16), (256, 32), (4096, 256)] {
        let original = Layout::from_size_align(512, backing_alignment).unwrap();
        let small = Layout::from_size_align(63, small_alignment).unwrap();
        let grown_layout = Layout::from_size_align(257, backing_alignment).unwrap();
        unsafe {
            let block = allocator.allocate(original).unwrap();
            block
                .cast::<u8>()
                .as_ptr()
                .write_bytes(0xac, original.size());
            let shrunk = allocator.shrink(block.cast(), original, small).unwrap();
            let grown = allocator
                .grow_zeroed(shrunk.cast(), small, grown_layout)
                .unwrap();
            assert_eq!(grown.cast::<u8>(), shrunk.cast());
            assert_eq!(grown.len(), grown_layout.size());
            assert_eq!(grown.cast::<u8>().as_ptr() as usize % backing_alignment, 0);
            assert!(grown.as_ref()[..small.size()].iter().all(|&b| b == 0xac));
            assert!(grown.as_ref()[small.size()..].iter().all(|&b| b == 0));
            allocator.deallocate(grown.cast(), grown_layout);
        }
    }
}

#[test]
fn grow_zeroed_preserves_prefix_when_moving() {
    let allocator = RSMalloc::new_default();
    for (old_alignment, new_size, new_alignment) in [
        (16, 8192, None),
        (16, 2 * 1024 * 1024 + 1, Some(256)),
        (4096, 2 * 1024 * 1024 + 1, Some(16)),
    ] {
        let old_layout = Layout::from_size_align(63, old_alignment).unwrap();
        unsafe {
            let block = allocator.allocate(old_layout).unwrap();
            let ptr = block.cast::<u8>();
            ptr.as_ptr().write_bytes(0x71, old_layout.size());
            let alignment = 1usize << ((ptr.as_ptr() as usize).trailing_zeros() + 1);
            let alignment = new_alignment.unwrap_or(alignment);
            let new_layout = Layout::from_size_align(new_size, alignment).unwrap();
            let grown = allocator.grow_zeroed(ptr, old_layout, new_layout).unwrap();
            assert_ne!(grown.cast::<u8>(), ptr);
            assert_eq!(grown.len(), new_layout.size());
            assert_eq!(grown.cast::<u8>().as_ptr() as usize % alignment, 0);
            assert!(
                grown.as_ref()[..old_layout.size()]
                    .iter()
                    .all(|&b| b == 0x71)
            );
            assert!(grown.as_ref()[old_layout.size()..].iter().all(|&b| b == 0));
            allocator.deallocate(grown.cast(), new_layout);
        }
    }
}

#[test]
fn zero_sized_blocks_can_be_resized_and_deallocated() {
    let allocator = RSMalloc::new_default();
    let empty = Layout::from_size_align(0, 64).unwrap();
    let full = Layout::from_size_align(128, 128).unwrap();
    unsafe {
        let block = allocator.allocate(empty).unwrap();
        let grown = allocator.grow_zeroed(block.cast(), empty, full).unwrap();
        assert!(grown.as_ref().iter().all(|&b| b == 0));
        let alignment = 1usize << ((grown.cast::<u8>().as_ptr() as usize).trailing_zeros() + 1);
        let aligned_empty = Layout::from_size_align(0, alignment).unwrap();
        let shrunk = allocator.shrink(grown.cast(), full, aligned_empty).unwrap();
        assert_eq!(shrunk.len(), 0);
        assert_eq!(shrunk.cast::<u8>().as_ptr() as usize % alignment, 0);
        let empty_again = allocator.grow(shrunk.cast(), aligned_empty, empty).unwrap();
        assert_eq!(empty_again.len(), 0);
        let regrown = allocator
            .grow_zeroed(empty_again.cast(), empty, full)
            .unwrap();
        assert!(regrown.as_ref().iter().all(|&b| b == 0));
        let shrunk = allocator.shrink(regrown.cast(), full, empty).unwrap();
        assert_eq!(shrunk.cast::<u8>(), regrown.cast());
        allocator.deallocate(shrunk.cast(), empty);
    }
}

#[test]
fn failed_growth_preserves_original_block() {
    let allocator = RSMalloc::new_default();
    let old_layout = Layout::from_size_align(128, 64).unwrap();
    let impossible = Layout::from_size_align(isize::MAX as usize, 1).unwrap();
    unsafe {
        let block = allocator.allocate(old_layout).unwrap();
        block
            .cast::<u8>()
            .as_ptr()
            .write_bytes(0xd3, old_layout.size());
        assert!(
            allocator
                .grow(block.cast(), old_layout, impossible)
                .is_err()
        );
        assert!(block.as_ref().iter().all(|&b| b == 0xd3));
        block
            .cast::<u8>()
            .as_ptr()
            .write_bytes(0x6b, old_layout.size());
        assert!(
            allocator
                .grow_zeroed(block.cast(), old_layout, impossible)
                .is_err()
        );
        assert!(block.as_ref().iter().all(|&b| b == 0x6b));
        allocator.deallocate(block.cast(), old_layout);
    }
}
