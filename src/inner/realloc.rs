use std::{os::raw::c_void, ptr::copy_nonoverlapping};

use rustix::mm::{MremapFlags, mremap};

use crate::{
    BigAllocMeta, Header,
    big_allocation::{big_free, big_malloc},
    core_prim::wrappers::{SafePointer, UnsafePointer},
    inner::{
        fallback::realloc_fallback,
        free::{find_original_ptr, rs_free},
        malloc::{alloc, usable_size},
    },
    internals::{hashmap::BIG_ALLOC_MAP, l3_main_radix::L3_RADIX},
    utility::{SIZE_CLASSES, align_to, match_size_class},
};

#[inline(always)]
fn estimate_big_mapping_size(size: usize) -> usize {
    const TWO_MB: usize = 1024 * 1024 * 2;

    let remainder = size % TWO_MB;
    if remainder > 0 && (TWO_MB - remainder) <= 1024 * 64 {
        align_to(size, TWO_MB)
    } else {
        align_to(size, 4096)
    }
}

// Audited and partially assisted by: GPT-Codex
// TODO: Check for safety logic bugs
unsafe fn small_realloc(ptr: SafePointer<Header>, new_size: usize) -> UnsafePointer<Header> {
    let payload_ptr = ptr;
    let header_ptr = payload_ptr.get_actual_header();

    let old_class = header_ptr.class as usize;
    let old_payload_size = SIZE_CLASSES[old_class];

    let new_class = match match_size_class(new_size) {
        Some(class) => class,
        None => {
            let new = big_malloc(new_size);
            if new.is_null() {
                return UnsafePointer::NULL;
            }

            copy_nonoverlapping(
                payload_ptr.cast_as_ptr() as *const u8,
                new.cast_as_ptr(),
                old_payload_size.min(new_size),
            );

            rs_free(payload_ptr.apply_unsafe());
            return new.cast();
        }
    };

    if old_class == new_class {
        return payload_ptr.apply_unsafe();
    }

    if old_payload_size > 1024 * 128
        && SIZE_CLASSES[new_class] > 1024 * 128
        && (header_ptr.cast_usize() & 4095) == 0
    {
        let old_total = align_to(old_payload_size + Header::SIZE, 4096);
        let new_total = align_to(SIZE_CLASSES[new_class] + Header::SIZE, 4096);

        if let Ok(new_addr) = mremap(
            header_ptr.cast_as_ptr() as *mut c_void,
            old_total,
            new_total,
            MremapFlags::MAYMOVE,
        ) {
            let new_addr_usize = new_addr as usize;

            if new_addr_usize != header_ptr.cast_usize() {
                L3_RADIX.set_range(header_ptr.cast_usize(), old_total, false);
            }

            L3_RADIX.set_range(new_addr_usize, new_total, true);

            let mut new_header_ptr = UnsafePointer::new(new_addr as *mut Header).apply_safe();
            new_header_ptr.class = new_class as u8;
            return new_header_ptr.walk_header().apply_unsafe();
        }
    }

    let new_ptr = alloc(new_size);
    if new_ptr.is_null() {
        return UnsafePointer::NULL;
    }

    copy_nonoverlapping(
        payload_ptr.cast_as_ptr() as *const u8,
        new_ptr.cast_as_ptr(),
        old_payload_size.min(new_size),
    );
    rs_free(payload_ptr.apply_unsafe());
    new_ptr
}

// Audited and partially assisted by: GPT-Codex
// TODO: Check for safety logic bugs
unsafe fn big_realloc(ptr: SafePointer<Header>, new_size: usize) -> UnsafePointer<Header> {
    let old_key = ptr.cast_usize();
    let old_mapping = (old_key - Header::SIZE) as *mut c_void;
    let old_meta = match BIG_ALLOC_MAP.get(old_key) {
        Some(meta) => meta,
        None => return UnsafePointer::NULL,
    };

    if new_size <= old_meta.size {
        return ptr.apply_unsafe();
    }

    if match_size_class(new_size).is_some() {
        let new_alloc = alloc(new_size);
        if new_alloc.is_null() {
            return UnsafePointer::NULL;
        }

        copy_nonoverlapping(
            old_key as *const u8,
            new_alloc.cast_as_ptr(),
            old_meta.size.min(new_size),
        );
        big_free(old_key);

        return new_alloc;
    }

    let old_total = estimate_big_mapping_size(old_meta.size + Header::SIZE);
    let aligned_new = estimate_big_mapping_size(new_size + Header::SIZE);

    if let Ok(new_addr) = mremap(old_mapping, old_total, aligned_new, MremapFlags::MAYMOVE) {
        let new_key = (new_addr as usize) + Header::SIZE;
        let new_meta = BigAllocMeta {
            next: std::ptr::null_mut(),
            size: new_size,
        };

        if new_key == old_key {
            let _ = BIG_ALLOC_MAP.replace(old_key, new_meta);
        } else {
            let _ = BIG_ALLOC_MAP.remove(old_key);
            BIG_ALLOC_MAP.insert(new_key, new_meta);
        }

        return UnsafePointer::new(new_addr as *mut Header).walk_header();
    }

    let new_alloc = big_malloc(new_size);
    if new_alloc.is_null() {
        return UnsafePointer::NULL;
    }

    copy_nonoverlapping(
        old_key as *const u8,
        new_alloc.cast_as_ptr(),
        old_meta.size.min(new_size),
    );
    big_free(old_key);

    new_alloc.cast()
}

#[inline(always)]
pub unsafe fn rs_realloc(ptr: UnsafePointer<Header>, new_size: usize) -> UnsafePointer<Header> {
    if new_size == 0 {
        if !ptr.is_null() {
            rs_free(ptr);
        }
        return UnsafePointer::NULL;
    }

    if ptr.is_null() {
        return alloc(new_size).cast();
    }

    let ptr_addr = ptr.cast_usize();

    if L3_RADIX.is_owned(ptr_addr) {
        let ptr_copy_for_search = UnsafePointer::new(ptr.cast_as_ptr::<Header>());
        let searched = find_original_ptr(ptr_copy_for_search);

        if searched.cast_usize() != ptr_addr {
            let ptr_copy_for_usable = UnsafePointer::new(ptr.cast_as_ptr::<Header>());
            let old_usable = usable_size(ptr_copy_for_usable);

            if new_size <= old_usable {
                return ptr;
            }

            let new_ptr = alloc(new_size);
            if new_ptr.is_null() {
                return UnsafePointer::NULL;
            }

            copy_nonoverlapping(
                ptr.cast_as_ptr() as *const u8,
                new_ptr.cast_as_ptr(),
                old_usable.min(new_size),
            );

            let ptr_copy_for_free = UnsafePointer::new(ptr.cast_as_ptr::<Header>());
            rs_free(ptr_copy_for_free);
            return new_ptr;
        }

        return small_realloc(searched.apply_safe(), new_size);
    }

    let ptr_copy_for_search = UnsafePointer::new(ptr.cast_as_ptr::<Header>());
    let searched = find_original_ptr(ptr_copy_for_search);

    if !BIG_ALLOC_MAP.is_ours(searched.cast_usize()) {
        let out = realloc_fallback(ptr.cast_as_ptr(), new_size);
        return UnsafePointer::new(out as *mut Header);
    }

    big_realloc(searched.apply_safe(), new_size)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::inner::align::memalign_inner;

    unsafe fn fill_pattern(ptr: &UnsafePointer<Header>, len: usize, seed: u8) {
        let base = ptr.cast_as_ptr::<u8>();
        for i in 0..len {
            *base.add(i) = seed.wrapping_add((i as u8).wrapping_mul(13));
        }
    }

    unsafe fn assert_pattern(ptr: &UnsafePointer<Header>, len: usize, seed: u8) {
        let base = ptr.cast_as_ptr::<u8>();
        for i in 0..len {
            let expected = seed.wrapping_add((i as u8).wrapping_mul(13));
            assert_eq!(*base.add(i), expected, "byte mismatch at {}", i);
        }
    }

    #[test]
    fn realloc_null_allocates() {
        unsafe {
            let p = rs_realloc(UnsafePointer::NULL, 256);
            assert!(!p.is_null());
            rs_free(p);
        }
    }

    #[test]
    fn realloc_zero_frees_and_returns_null() {
        unsafe {
            let p = alloc(128);
            assert!(!p.is_null());

            let out = rs_realloc(p, 0);
            assert!(out.is_null());

            let out2 = rs_realloc(UnsafePointer::NULL, 0);
            assert!(out2.is_null());
        }
    }

    #[test]
    fn realloc_small_growth_preserves_prefix() {
        unsafe {
            let old_size = 96usize;
            let new_size = 2048usize;
            let seed = 0x5Au8;

            let p = alloc(old_size);
            assert!(!p.is_null());
            fill_pattern(&p, old_size, seed);

            let p2 = rs_realloc(p, new_size);
            assert!(!p2.is_null());
            assert_pattern(&p2, old_size, seed);

            rs_free(p2);
        }
    }

    #[test]
    fn realloc_small_shrink_preserves_prefix() {
        unsafe {
            let old_size = 2048usize;
            let new_size = 96usize;
            let seed = 0x33u8;

            let p = alloc(old_size);
            assert!(!p.is_null());
            fill_pattern(&p, new_size, seed);

            let p2 = rs_realloc(p, new_size);
            assert!(!p2.is_null());
            assert_pattern(&p2, new_size, seed);

            rs_free(p2);
        }
    }

    #[test]
    fn realloc_same_class_keeps_data() {
        unsafe {
            let old_size = 80usize;
            let new_size = 79usize;
            let seed = 0xA7u8;

            let p = alloc(old_size);
            assert!(!p.is_null());
            fill_pattern(&p, old_size, seed);

            let p2 = rs_realloc(p, new_size);
            assert!(!p2.is_null());
            assert_pattern(&p2, old_size, seed);

            rs_free(p2);
        }
    }

    #[test]
    fn realloc_failure_does_not_invalidate_old_ptr() {
        unsafe {
            let old_size = 256usize;
            let huge_size = usize::MAX / 2;
            let seed = 0x11u8;

            let p = alloc(old_size);
            assert!(!p.is_null());
            fill_pattern(&p, old_size, seed);

            let old_addr = p.cast_usize();
            let out = rs_realloc(p, huge_size);
            assert!(out.is_null());

            let old_again = UnsafePointer::new(old_addr as *mut Header);
            assert_pattern(&old_again, old_size, seed);
            rs_free(old_again);
        }
    }

    #[test]
    fn realloc_memalign_growth_preserves_prefix() {
        unsafe {
            let old_size = 128usize;
            let new_size = 1024usize;
            let seed = 0x6Du8;

            let p = memalign_inner(64, old_size);
            assert!(!p.is_null());
            assert_eq!(p.cast_usize() % 64, 0);
            fill_pattern(&p, old_size, seed);

            let p2 = rs_realloc(p, new_size);
            assert!(!p2.is_null());
            assert_pattern(&p2, old_size, seed);

            rs_free(p2);
        }
    }

    #[test]
    fn realloc_memalign_shrink_preserves_prefix() {
        unsafe {
            let old_size = 256usize;
            let new_size = 96usize;
            let seed = 0x2Cu8;

            let p = memalign_inner(128, old_size);
            assert!(!p.is_null());
            assert_eq!(p.cast_usize() % 128, 0);
            fill_pattern(&p, new_size, seed);

            let p2 = rs_realloc(p, new_size);
            assert!(!p2.is_null());
            assert_pattern(&p2, new_size, seed);

            rs_free(p2);
        }
    }
}
