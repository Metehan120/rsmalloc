// This file is assisated by AI only for audit and debugging, theres shouldnt be any AI hallucinations but better checking in future
//
// Namings were made by me at 3AM (in a sense, not exactly 3AM ofc) do not assume its AI because of clean namings its just me bored ;)
// - Metehan

use std::{os::raw::c_void, ptr::copy_nonoverlapping};

use rustix::mm::{MremapFlags, mremap};

use crate::{
    BIG_MAGIC, BigAllocMeta, Header,
    big_allocations::{
        big_allocation::{big_free, big_malloc, estimate_and_align_2mb},
        buddy::{BIG_BUDDY_ALLOCATOR, BIG_BUDDY_MAX_ORDER},
    },
    core_prim::wrappers::{SafePointer, UnsafePointer},
    inner::{
        fallback::realloc_fallback,
        free::{find_original_ptr, rs_free},
        malloc::{rs_alloc, usable_size},
    },
    internals::{hashmap::BIG_ALLOC_MAP, l3_main_radix::L3_RADIX},
    utility::{SIZE_CLASSES, align_to, match_size_class},
};

// TODO: Check for safety logic bugs
unsafe fn small_realloc(ptr: SafePointer<Header>, new_size: usize) -> UnsafePointer<Header> {
    let payload_ptr = ptr;
    let header_ptr = payload_ptr.get_actual_header();

    let old_class = header_ptr.class as usize;
    let old_payload_size = SIZE_CLASSES[old_class];

    if new_size <= old_payload_size {
        return payload_ptr.apply_unsafe();
    }

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

    let new_ptr = rs_alloc(new_size);
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

// TODO: Check for safety logic bugs
unsafe fn big_realloc(ptr: SafePointer<Header>, new_size: usize) -> UnsafePointer<Header> {
    let old_ptr = ptr.cast_usize();
    let old_mapping = (old_ptr - Header::SIZE) as *mut c_void;
    let old_meta = match BIG_ALLOC_MAP.get(old_ptr) {
        Some(meta) => meta,
        None => return UnsafePointer::NULL,
    };

    let old_mapped_size = 1usize << old_meta.order;
    if new_size <= old_meta.size {
        return ptr.apply_unsafe();
    }

    if match_size_class(new_size).is_some() {
        let new_alloc = rs_alloc(new_size);
        if new_alloc.is_null() {
            return UnsafePointer::NULL;
        }

        copy_nonoverlapping(
            old_ptr as *const u8,
            new_alloc.cast_as_ptr(),
            old_meta.size.min(new_size),
        );
        big_free(old_ptr);

        return new_alloc;
    }

    let old_total = old_mapped_size;
    let aligned_new = estimate_and_align_2mb(new_size + Header::SIZE);

    if !BIG_BUDDY_ALLOCATOR.is_in_pool(old_ptr) {
        if let Ok(new_addr) = mremap(old_mapping, old_total, aligned_new, MremapFlags::MAYMOVE) {
            let new_mapping = new_addr as usize;
            let new_key = new_mapping + Header::SIZE;
            let new_meta = BigAllocMeta {
                next: std::ptr::null_mut(),
                size: new_size,
                order: aligned_new.next_power_of_two().trailing_zeros() as usize,
            };

            if new_key == old_ptr {
                let _ = BIG_ALLOC_MAP.replace(old_ptr, new_meta);
            } else {
                let _ = BIG_ALLOC_MAP.remove(old_ptr);
                BIG_ALLOC_MAP.insert(new_key, new_meta);
                L3_RADIX.set_range(old_mapping as usize, old_total, false);
            }

            L3_RADIX.set_range(new_mapping, aligned_new, true);

            return UnsafePointer::new(new_addr as *mut Header).walk_header();
        }
    } else {
        let mut current_addr = old_ptr;
        let mut current_order = old_meta.order;

        let mut tries = 0;
        while current_order < BIG_BUDDY_MAX_ORDER {
            if aligned_new.next_power_of_two().trailing_zeros() as usize > BIG_BUDDY_MAX_ORDER {
                break;
            }

            if let Some((new_addr, new_order)) =
                BIG_BUDDY_ALLOCATOR.try_grow_inplace(current_addr, current_order)
            {
                current_addr = new_addr;
                current_order = new_order;

                let new_mapped_size = 1usize << new_order;
                if new_mapped_size >= aligned_new {
                    let new_meta = BigAllocMeta {
                        next: std::ptr::null_mut(),
                        size: new_size,
                        order: new_order,
                    };
                    let _ = BIG_ALLOC_MAP.replace(old_ptr, new_meta);

                    return UnsafePointer::new(current_addr as *mut Header).walk_header();
                } else {
                    tries += 1;
                    if tries > 15 {
                        break;
                    }
                }
            } else {
                break;
            }
        }
    }

    let new_alloc = big_malloc(new_size);
    if new_alloc.is_null() {
        return UnsafePointer::NULL;
    }

    copy_nonoverlapping(
        old_ptr as *const u8,
        new_alloc.cast_as_ptr(),
        old_meta.size.min(new_size),
    );
    big_free(old_ptr);

    new_alloc.cast()
}

#[inline(always)]
pub unsafe fn rs_realloc(ptr: UnsafePointer<Header>, new_size: usize) -> UnsafePointer<Header> {
    if ptr.is_null() {
        return rs_alloc(new_size.max(1)).cast();
    }

    if new_size == 0 {
        rs_free(ptr);
        return UnsafePointer::NULL;
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

            let new_ptr = rs_alloc(new_size);
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

        let searched_safe = searched.apply_safe();
        let searched_header = searched_safe.get_actual_header();

        if searched_header.magic == BIG_MAGIC {
            return big_realloc(searched_safe, new_size);
        }

        return small_realloc(searched_safe, new_size);
    }

    UnsafePointer::new(realloc_fallback(ptr.cast_as_ptr() as *mut c_void, new_size) as *mut Header)
}

// Tests generated by AI: CODEX

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

            let z = rs_realloc(UnsafePointer::NULL, 0);
            assert!(!z.is_null());
            rs_free(z);
        }
    }

    #[test]
    fn realloc_zero_frees_and_returns_null() {
        unsafe {
            let p = rs_alloc(128);
            assert!(!p.is_null());

            let out = rs_realloc(p, 0);
            assert!(out.is_null());

            let out2 = rs_realloc(UnsafePointer::NULL, 0);
            assert!(!out2.is_null());
            rs_free(out2);
        }
    }

    #[test]
    fn realloc_small_growth_preserves_prefix() {
        unsafe {
            let old_size = 96usize;
            let new_size = 2048usize;
            let seed = 0x5Au8;

            let p = rs_alloc(old_size);
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

            let p = rs_alloc(old_size);
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

            let p = rs_alloc(old_size);
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

            let p = rs_alloc(old_size);
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
