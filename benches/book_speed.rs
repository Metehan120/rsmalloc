// WARNING: This benchmark only measures the speed bookkeeping speed of the allocator, not the real performance of the allocator.

use std::{
    hint::black_box,
    os::raw::c_void,
    sync::{Arc, Barrier},
    thread,
    time::Instant,
};

use criterion::{BenchmarkId, Criterion, criterion_group, criterion_main};
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
