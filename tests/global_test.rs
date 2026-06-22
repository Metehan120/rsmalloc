// This test written by CODEX
//
// Good enough, leave it as is

#[cfg(not(feature = "preload"))]
#[cfg(test)]
mod tests {
    use std::alloc::{GlobalAlloc, Layout};

    use rsmalloc::{RSMalloc, Size};

    #[global_allocator]
    static GLOBAL: RSMalloc = RSMalloc::new_default();

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
    fn usable_size_reports_rsmalloc_allocation() {
        let mut values = Vec::with_capacity(128);
        values.extend(0u8..64);

        let size = unsafe { GLOBAL.rs_usable_size(values.as_mut_ptr()) };
        match size {
            Size::RS(bytes) => assert!(bytes >= values.capacity()),
            Size::NotRS => panic!("Vec allocation was not recognized as rsmalloc-owned"),
        }
    }

    #[test]
    fn big_allocation_usable_size_is_available() {
        let mut values = Vec::<u8>::with_capacity(3 * 1024 * 1024);
        values.resize(1024, 0xA5);

        let size = unsafe { GLOBAL.rs_usable_size(values.as_mut_ptr()) };
        match size {
            Size::RS(bytes) => assert!(bytes >= values.capacity()),
            Size::NotRS => panic!("big Vec allocation was not recognized as rsmalloc-owned"),
        }
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
    fn direct_rsmalloc_helpers_work() {
        unsafe {
            let ptr = GLOBAL.rs_malloc(256);
            assert!(!ptr.is_null());
            for i in 0..256 {
                *ptr.add(i) = i as u8;
            }

            let ptr = GLOBAL.rs_realloc(ptr, 512);
            assert!(!ptr.is_null());
            for i in 0..256 {
                assert_eq!(*ptr.add(i), i as u8, "byte mismatch at offset {i}");
            }
            GLOBAL.rs_free(ptr);

            let zeroed = GLOBAL.rs_calloc(64, 4);
            assert!(!zeroed.is_null());
            for i in 0..256 {
                assert_eq!(*zeroed.add(i), 0, "non-zero calloc byte at offset {i}");
            }
            GLOBAL.rs_free(zeroed);

            let aligned = GLOBAL.rs_memalign(128, 512);
            assert!(!aligned.is_null());
            assert_eq!((aligned as usize) % 128, 0);
            GLOBAL.rs_free(aligned);
        }
    }
}
