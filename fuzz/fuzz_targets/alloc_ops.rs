#![no_main]

use std::alloc::{GlobalAlloc, Layout};

use arbitrary::Arbitrary;
use libfuzzer_sys::fuzz_target;
use rsmalloc::RSMalloc;

#[global_allocator]
static ALLOC: RSMalloc = RSMalloc::new_default();

#[derive(Debug, Arbitrary)]
enum Op {
    Alloc { size: u16, align_shift: u8 },
    AllocZeroed { size: u16, align_shift: u8 },
    Dealloc { index: u8 },
    Realloc { index: u8, new_size: u16 },
}

struct Live {
    ptr: *mut u8,
    layout: Layout,
    pattern: u8,
}

unsafe fn check(live: &Live) {
    unsafe {
        let slice = std::slice::from_raw_parts(live.ptr, live.layout.size());
        for &b in slice {
            assert_eq!(b, live.pattern, "corrupted live allocation detected");
        }
    }
}

unsafe fn fill(ptr: *mut u8, len: usize, pattern: u8) {
    unsafe {
        std::ptr::write_bytes(ptr, pattern, len);
    }
}

fuzz_target!(|ops: Vec<Op>| {
    let mut live: Vec<Live> = Vec::new();
    let mut next_pattern: u8 = 1;

    for op in ops.into_iter().take(4096) {
        match op {
            Op::Alloc { size, align_shift } => {
                let size = (size as usize).max(1).min(1 << 18);
                let align = 1usize << (align_shift % 8); // 1..=128
                let Ok(layout) = Layout::from_size_align(size, align) else {
                    continue;
                };

                let ptr = unsafe { ALLOC.alloc(layout) };
                if ptr.is_null() {
                    continue;
                }
                assert_eq!(ptr as usize % align, 0, "misaligned allocation");

                let pattern = next_pattern;
                next_pattern = next_pattern.wrapping_add(1).max(1);

                unsafe { fill(ptr, size, pattern) };
                live.push(Live {
                    ptr,
                    layout,
                    pattern,
                });
            }
            Op::Dealloc { index } => {
                if live.is_empty() {
                    continue;
                }
                let idx = index as usize % live.len();
                let entry = live.swap_remove(idx);
                unsafe {
                    check(&entry);
                    ALLOC.dealloc(entry.ptr, entry.layout);
                }
            }
            Op::AllocZeroed { size, align_shift } => {
                let size = (size as usize).max(1).min(1 << 18);
                let align = 1usize << (align_shift % 8);
                let Ok(layout) = Layout::from_size_align(size, align) else {
                    continue;
                };

                let ptr = unsafe { ALLOC.alloc_zeroed(layout) };
                if ptr.is_null() {
                    continue;
                }
                assert_eq!(ptr as usize % align, 0, "misaligned allocation");

                let slice = unsafe { std::slice::from_raw_parts(ptr, size) };
                for &b in slice {
                    assert_eq!(b, 0, "alloc_zeroed returned non-zeroed memory");
                }

                live.push(Live {
                    ptr,
                    layout,
                    pattern: 0,
                });
            }
            Op::Realloc { index, new_size } => {
                if live.is_empty() {
                    continue;
                }
                let idx = index as usize % live.len();
                let new_size = (new_size as usize).max(1).min(1 << 18);

                unsafe { check(&live[idx]) };

                let old_layout = live[idx].layout;
                let old_pattern = live[idx].pattern;
                let old_ptr = live[idx].ptr;

                let new_ptr = unsafe { ALLOC.realloc(old_ptr, old_layout, new_size) };
                if new_ptr.is_null() {
                    unsafe { check(&live[idx]) };
                    continue;
                }

                let preserved = old_layout.size().min(new_size);
                let slice = unsafe { std::slice::from_raw_parts(new_ptr, preserved) };
                for &b in slice {
                    assert_eq!(b, old_pattern, "realloc failed to preserve prefix");
                }

                let new_pattern = next_pattern;
                next_pattern = next_pattern.wrapping_add(1).max(1);
                unsafe { fill(new_ptr, new_size, new_pattern) };

                live[idx] = Live {
                    ptr: new_ptr,
                    layout: Layout::from_size_align(new_size, old_layout.align()).unwrap(),
                    pattern: new_pattern,
                };
            }
        }
    }

    for entry in live {
        unsafe {
            check(&entry);
            ALLOC.dealloc(entry.ptr, entry.layout);
        }
    }
});
