#![no_main]

use std::{
    alloc::{GlobalAlloc, Layout},
    collections::BTreeSet,
    sync::{Arc, Barrier, Mutex, OnceLock},
};

use arbitrary::Arbitrary;
use libfuzzer_sys::fuzz_target;
use rsmalloc::RSMalloc;

#[global_allocator]
static ALLOC: RSMalloc = RSMalloc::new_default();

const MAX_THREADS: usize = 12;
const MAX_OPS: usize = 4096;

static WORKERS: OnceLock<rayon::ThreadPool> = OnceLock::new();

fn workers() -> &'static rayon::ThreadPool {
    WORKERS.get_or_init(|| {
        rayon::ThreadPoolBuilder::new()
            .num_threads(MAX_THREADS)
            .thread_name(|index| format!("rsmalloc-fuzz-{index}"))
            .build()
            .expect("failed to create the concurrent fuzz worker pool")
    })
}

#[derive(Debug, Arbitrary)]
struct Input {
    thread_count: u8,
    ops: Vec<Op>,
}

#[derive(Debug, Arbitrary)]
enum Op {
    Alloc { size: u32, align_shift: u8 },
    AllocZeroed { size: u32, align_shift: u8 },
    Dealloc { index: u16 },
    Realloc { index: u16, new_size: u32 },
}

struct Live {
    ptr: *mut u8,
    layout: Layout,
    pattern: u8,
}

unsafe impl Send for Live {}

#[derive(Default)]
struct LivePointers(Mutex<BTreeSet<usize>>);

impl LivePointers {
    fn insert(&self, ptr: *mut u8) {
        assert!(
            self.0.lock().unwrap().insert(ptr as usize),
            "allocator returned a pointer that is already live"
        );
    }

    fn replace(&self, old_ptr: *mut u8, new_ptr: *mut u8) {
        let mut pointers = self.0.lock().unwrap();
        assert!(pointers.remove(&(old_ptr as usize)));
        assert!(
            pointers.insert(new_ptr as usize),
            "realloc returned a pointer owned by another live allocation"
        );
    }

    fn remove(&self, ptr: *mut u8) {
        assert!(self.0.lock().unwrap().remove(&(ptr as usize)));
    }

    fn is_empty(&self) -> bool {
        self.0.lock().unwrap().is_empty()
    }
}

unsafe fn check(live: &Live) {
    let slice = unsafe { std::slice::from_raw_parts(live.ptr, live.layout.size()) };
    for &byte in slice {
        assert_eq!(byte, live.pattern, "corrupted live allocation detected");
    }
}

unsafe fn fill(ptr: *mut u8, len: usize, pattern: u8) {
    unsafe { std::ptr::write_bytes(ptr, pattern, len) };
}

fn allocation_size(value: u32) -> usize {
    (value as usize).max(1).min(1 << 18)
}

fn next_pattern(thread_id: usize, sequence: &mut u8) -> u8 {
    *sequence = sequence.wrapping_add(1);
    (thread_id as u8)
        .wrapping_mul(67)
        .wrapping_add(*sequence)
        .max(1)
}

fn run_lane(
    thread_id: usize,
    thread_count: usize,
    ops: &[Op],
    barrier: &Barrier,
    pointers: &LivePointers,
) -> Vec<Live> {
    let mut live = Vec::new();
    let mut sequence = 0;

    barrier.wait();

    for op in ops.iter().skip(thread_id).step_by(thread_count) {
        match *op {
            Op::Alloc { size, align_shift } => {
                let size = allocation_size(size);
                let align = 1usize << (align_shift % 13); // 1..=4096
                let layout = Layout::from_size_align(size, align).unwrap();
                let ptr = unsafe { ALLOC.alloc(layout) };
                if ptr.is_null() {
                    continue;
                }

                assert_eq!(ptr as usize % align, 0, "misaligned allocation");
                pointers.insert(ptr);

                let pattern = next_pattern(thread_id, &mut sequence);
                unsafe { fill(ptr, size, pattern) };
                live.push(Live {
                    ptr,
                    layout,
                    pattern,
                });
            }
            Op::AllocZeroed { size, align_shift } => {
                let size = allocation_size(size);
                let align = 1usize << (align_shift % 13);
                let layout = Layout::from_size_align(size, align).unwrap();
                let ptr = unsafe { ALLOC.alloc_zeroed(layout) };
                if ptr.is_null() {
                    continue;
                }

                assert_eq!(ptr as usize % align, 0, "misaligned allocation");
                let slice = unsafe { std::slice::from_raw_parts(ptr, size) };
                assert!(
                    slice.iter().all(|&byte| byte == 0),
                    "alloc_zeroed returned non-zeroed memory"
                );
                pointers.insert(ptr);

                let pattern = next_pattern(thread_id, &mut sequence);
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

                let entry = live.swap_remove(index as usize % live.len());
                unsafe { check(&entry) };
                pointers.remove(entry.ptr);
                unsafe { ALLOC.dealloc(entry.ptr, entry.layout) };
            }
            Op::Realloc { index, new_size } => {
                if live.is_empty() {
                    continue;
                }

                let index = index as usize % live.len();
                let new_size = allocation_size(new_size);
                unsafe { check(&live[index]) };

                let old_ptr = live[index].ptr;
                let old_layout = live[index].layout;
                let old_pattern = live[index].pattern;
                let new_ptr = unsafe { ALLOC.realloc(old_ptr, old_layout, new_size) };
                if new_ptr.is_null() {
                    unsafe { check(&live[index]) };
                    continue;
                }

                let preserved = old_layout.size().min(new_size);
                let prefix = unsafe { std::slice::from_raw_parts(new_ptr, preserved) };
                assert!(
                    prefix.iter().all(|&byte| byte == old_pattern),
                    "realloc failed to preserve prefix"
                );
                pointers.replace(old_ptr, new_ptr);

                let pattern = next_pattern(thread_id, &mut sequence);
                unsafe { fill(new_ptr, new_size, pattern) };
                live[index] = Live {
                    ptr: new_ptr,
                    layout: Layout::from_size_align(new_size, old_layout.align()).unwrap(),
                    pattern,
                };
            }
        }
    }

    live
}

fuzz_target!(|input: Input| {
    let thread_count = 2 + input.thread_count as usize % (MAX_THREADS - 1);
    let ops = &input.ops[..input.ops.len().min(MAX_OPS)];
    let barrier = Arc::new(Barrier::new(thread_count));
    let pointers = Arc::new(LivePointers::default());

    let live_sets = Mutex::new(
        (0..thread_count)
            .map(|_| None)
            .collect::<Vec<Option<Vec<Live>>>>(),
    );

    workers().scope(|scope| {
        for thread_id in 0..thread_count {
            let barrier = Arc::clone(&barrier);
            let pointers = Arc::clone(&pointers);
            let live_sets = &live_sets;
            scope.spawn(move |_| {
                let live = run_lane(thread_id, thread_count, ops, &barrier, &pointers);
                live_sets.lock().unwrap()[thread_id] = Some(live);
            });
        }
    });
    let live_sets = live_sets
        .into_inner()
        .unwrap()
        .into_iter()
        .map(Option::unwrap)
        .collect::<Vec<_>>();

    let cleanup_barrier = Arc::new(Barrier::new(live_sets.len()));
    workers().scope(|scope| {
        for live in live_sets {
            let pointers = Arc::clone(&pointers);
            let cleanup_barrier = Arc::clone(&cleanup_barrier);
            scope.spawn(move |_| {
                cleanup_barrier.wait();
                for entry in live {
                    unsafe { check(&entry) };
                    pointers.remove(entry.ptr);
                    unsafe { ALLOC.dealloc(entry.ptr, entry.layout) };
                }
            });
        }
    });

    assert!(pointers.is_empty());
});
