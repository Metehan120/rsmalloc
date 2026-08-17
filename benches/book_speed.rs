// WARNING: This benchmark only measures the speed bookkeeping speed of the allocator, not the real performance of the allocator.

use std::{
    hint::black_box,
    os::raw::c_void,
    sync::{Arc, Barrier},
    thread,
    time::Instant,
};

use criterion::{BenchmarkId, Criterion, criterion_group, criterion_main};
use rand::RngExt;
use rustix::system::sysinfo;

unsafe extern "C" {
    fn malloc(size: usize) -> *mut c_void;
    fn free(ptr: *mut c_void);
}

fn check_memory_pressure() -> usize {
    let info = sysinfo();

    let unit = info.mem_unit as usize;
    let total_ram = (info.totalram as usize).saturating_mul(unit);
    let free_ram = (info.freeram as usize).saturating_mul(unit);
    let total_swap = (info.totalswap as usize).saturating_mul(unit);
    let free_swap = (info.freeswap as usize).saturating_mul(unit);

    let total_available = free_ram + free_swap;
    let total_memory = total_ram + total_swap;

    if total_memory == 0 {
        return 50;
    }

    let used = total_memory.saturating_sub(total_available);
    (used * 100) / total_memory
}

fn bench_alloc_free(c: &mut Criterion) {
    let mut group = c.benchmark_group("alloc_free");

    pub const SIZE_CLASSES: [usize; 34] = [
        // Tiny (16-128) - 16 Byte steps
        16, 32, 48, 64, 80, 96, 128, // Small (160-512) - 32/64 Byte steps
        160, 192, 256, 320, 384, 512, // Medium (768-3072) - Large steps
        768, 1024, 1280, 1536, 1792, 2048, 2560, 3072, // Large (3840-24KB)
        3840, 4096, 8192, 12288, 16384, 24576, // Very Large (32KB+)
        32768, 65536, 131072, 262144, 524288, 1048576, 2097152,
    ];

    pub const NUM_SIZE_CLASSES: usize = SIZE_CLASSES.len();

    pub const SIZE_LUT: [u8; 256] = {
        let mut lut = [0u8; 256];
        let mut i = 0;
        while i < 256 {
            let size = (i + 1) * 16;
            let mut class = 0;
            while class < NUM_SIZE_CLASSES && SIZE_CLASSES[class] < size {
                class += 1;
            }
            lut[i] = class as u8;
            i += 1;
        }
        lut
    };
    const LARGE_SIZE_LUT: [u8; 8] = [0, 23, 24, 25, 26, 26, 27, 27];

    #[inline(always)]
    pub fn match_size_class(size: usize) -> Option<usize> {
        unsafe {
            if size == 0 {
                return Some(0);
            } else if size <= 4096 {
                let index = (size - 1) >> 4;
                return Some(*SIZE_LUT.get_unchecked(index) as usize);
            }

            if size > 2097152 {
                return None;
            }

            if size <= 32768 {
                let index = (size - 1) >> 12;
                return Some(*LARGE_SIZE_LUT.get_unchecked(index) as usize);
            }

            let exponent = usize::BITS as usize - (size - 1).leading_zeros() as usize;
            Some(exponent + 12)
        }
    }

    // 64B
    group.bench_function("32B", |b| {
        b.iter(|| unsafe {
            let ptr = black_box(malloc(32));
            black_box(free(ptr));
        });
    });

    // 4KB
    group.bench_function("4KB", |b| {
        b.iter(|| unsafe {
            let ptr = black_box(malloc(4096));
            black_box(free(ptr));
        });
    });

    // 1MB
    group.bench_function("1MB", |b| {
        b.iter(|| unsafe {
            let ptr = black_box(malloc(1024 * 1024));
            black_box(free(ptr));
        });
    });

    // 3MB
    group.bench_function("3MB", |b| {
        b.iter(|| unsafe {
            let ptr = black_box(malloc(3 * 1024 * 1024));
            black_box(free(ptr));
        });
    });

    group.bench_function("syscall", |b| {
        b.iter(|| black_box(check_memory_pressure()));
    });

    for i in [64, 128, 4096, 8192, 16384, 32768, 1048576] {
        group.bench_function(format!("size_class_matching_({})", i), |b| {
            b.iter(|| {
                black_box(match_size_class(i));
            });
        });
    }

    group.bench_function("size_class_matching_random", |b| {
        b.iter_custom(|iters| {
            let mut sizes = Vec::with_capacity(iters as usize);

            for _ in 0..iters {
                let size = black_box(rand::rng().random_range(0..=2097152));
                sizes.push(black_box(size));
            }

            let start = Instant::now();
            for i in 0..iters {
                black_box(match_size_class(sizes[i as usize]));
            }

            start.elapsed()
        });
    });

    group.finish();
}

fn bench_malloc_only(c: &mut Criterion) {
    let mut group = c.benchmark_group("malloc_only");

    for (name, size) in [
        ("32B", 32usize),
        ("4KB", 4096),
        ("1MB", 1024 * 1024),
        ("3MB", 3 * 1024 * 1024),
    ] {
        group.bench_function(name, |b| {
            b.iter_custom(|iters| {
                let mut ptrs = Vec::with_capacity(iters as usize);

                let start = Instant::now();
                for _ in 0..iters {
                    unsafe { ptrs.push(black_box(malloc(size))) };
                }
                let elapsed = start.elapsed();

                // untimed: give the memory back so repeated samples don't pile up
                for ptr in ptrs {
                    unsafe { free(ptr) };
                }

                elapsed
            });
        });
    }

    group.finish();
}

fn bench_free_only(c: &mut Criterion) {
    let mut group = c.benchmark_group("free_only");

    for (name, size) in [
        ("32B", 32usize),
        ("4KB", 4096),
        ("1MB", 1024 * 1024),
        ("3MB", 3 * 1024 * 1024),
    ] {
        group.bench_function(name, |b| {
            b.iter_custom(|iters| {
                // untimed: pre-allocate everything this sample will free
                let ptrs: Vec<_> = (0..iters).map(|_| unsafe { malloc(size) }).collect();

                let start = Instant::now();
                for ptr in ptrs {
                    unsafe { black_box(free(ptr)) };
                }
                start.elapsed()
            });
        });
    }

    group.finish();
}

fn bench_alloc_free_mt(c: &mut Criterion) {
    let mut group = c.benchmark_group("alloc_free_mt");

    let cpus = thread::available_parallelism().map_or(4, |n| n.get());
    let mut thread_counts = vec![2, 4, 8];
    thread_counts.retain(|&n| n <= cpus);
    if !thread_counts.contains(&cpus) {
        thread_counts.push(cpus);
    }

    for size in [64usize, 4096] {
        for &threads in &thread_counts {
            group.bench_with_input(
                BenchmarkId::new(format!("{}B", size), threads),
                &threads,
                |b, &threads| {
                    b.iter_custom(|iters| {
                        let barrier = Arc::new(Barrier::new(threads));

                        let handles: Vec<_> = (0..threads)
                            .map(|_| {
                                let barrier = Arc::clone(&barrier);
                                thread::spawn(move || {
                                    barrier.wait();
                                    let start = Instant::now();
                                    for _ in 0..iters {
                                        unsafe {
                                            let ptr = black_box(malloc(size));
                                            black_box(free(ptr));
                                        }
                                    }
                                    start.elapsed()
                                })
                            })
                            .collect();

                        handles
                            .into_iter()
                            .map(|h| h.join().expect("worker panicked"))
                            .max()
                            .unwrap()
                    });
                },
            );
        }
    }

    group.finish();
}

criterion_group!(
    benches,
    bench_alloc_free,
    bench_malloc_only,
    bench_free_only,
    bench_alloc_free_mt,
);

criterion_main!(benches);
