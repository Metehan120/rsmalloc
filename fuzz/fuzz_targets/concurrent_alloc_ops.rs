#![no_main]

use std::{
    alloc::{GlobalAlloc, Layout},
    env,
    sync::{Arc, Barrier, OnceLock},
};

use arbitrary::Arbitrary;
use libfuzzer_sys::fuzz_target;
use rsmalloc::v2::alloc::RSMalloc;

#[global_allocator]
static ALLOC: RSMalloc = RSMalloc::new_default();

const MAX_THREADS: usize = 12;

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

struct LaneOutput(OnceLock<Vec<Live>>);

unsafe impl Sync for LaneOutput {}

impl LaneOutput {
    const fn new() -> Self {
        Self(OnceLock::new())
    }

    fn publish(&self, live: Vec<Live>) {
        assert!(self.0.set(live).is_ok());
    }

    fn into_live(self) -> Vec<Live> {
        self.0.into_inner().unwrap()
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

pub static TYPE: OnceLock<usize> = OnceLock::new();

pub fn get_fuzz_type() -> usize {
    *TYPE.get_or_init(|| {
        env::var("RS_FUZZ_TYPE")
            .unwrap_or("1".to_string())
            .parse()
            .expect("Wrong fuzz type. Fuzz types: 1 (small), 2 (big), 3 (mixed), 4 (full)")
    })
}

fn allocation_size(value: u32) -> usize {
    match get_fuzz_type() {
        1 => 1 + value as usize % (256 * 1024),
        2 => {
            const MIN: usize = 4 * 1024 * 1024;
            const MAX: usize = 64 * 1024 * 1024;
            MIN + value as usize % (MAX - MIN + 1)
        }
        3 => 1 + value as usize % ((2 * 1024 * 1024) + 1024 * 256),
        4 => 1 + value as usize % (8 * 1024 * 1024),
        _ => panic!("Wrong fuzz type. Fuzz types: 1 (small), 2 (big), 3 (mixed), 4 (full)"),
    }
}

fn next_pattern(thread_id: usize, sequence: &mut u8) -> u8 {
    *sequence = sequence.wrapping_add(1);
    (thread_id as u8)
        .wrapping_mul(67)
        .wrapping_add(*sequence)
        .max(1)
}

fn run_lane(thread_id: usize, thread_count: usize, ops: &[Op], barrier: &Barrier) -> Vec<Live> {
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

fn assert_disjoint(live_sets: &[Vec<Live>]) {
    let mut ranges = live_sets
        .iter()
        .flatten()
        .map(|live| {
            let start = live.ptr as usize;
            let end = start
                .checked_add(live.layout.size())
                .expect("live allocation range overflowed usize");
            (start, end)
        })
        .collect::<Vec<_>>();

    ranges.sort_unstable_by_key(|range| range.0);
    for pair in ranges.windows(2) {
        assert!(
            pair[0].1 <= pair[1].0,
            "allocator returned overlapping live allocations: {:#x}..{:#x} and {:#x}..{:#x}",
            pair[0].0,
            pair[0].1,
            pair[1].0,
            pair[1].1,
        );
    }
}

fuzz_target!(|input: Input| {
    let thread_count = 2 + input.thread_count as usize % (MAX_THREADS - 1);
    let ops = &input.ops[..input.ops.len()];
    if ops.len() < 2 {
        return;
    }
    let barrier = Arc::new(Barrier::new(thread_count));
    let live_sets = (0..thread_count)
        .map(|_| LaneOutput::new())
        .collect::<Vec<LaneOutput>>();

    workers().scope(|scope| {
        for thread_id in 0..thread_count {
            let barrier = Arc::clone(&barrier);
            let live_sets = &live_sets;
            scope.spawn(move |_| {
                let live = run_lane(thread_id, thread_count, ops, &barrier);
                live_sets[thread_id].publish(live);
            });
        }
    });

    let live_sets = live_sets
        .into_iter()
        .map(LaneOutput::into_live)
        .collect::<Vec<_>>();

    assert_disjoint(&live_sets);

    let cleanup_barrier = Arc::new(Barrier::new(live_sets.len()));
    workers().scope(|scope| {
        for live in live_sets {
            let cleanup_barrier = Arc::clone(&cleanup_barrier);
            scope.spawn(move |_| {
                cleanup_barrier.wait();
                for entry in live {
                    unsafe { check(&entry) };
                    unsafe { ALLOC.dealloc(entry.ptr, entry.layout) };
                }
            });
        }
    });
});
