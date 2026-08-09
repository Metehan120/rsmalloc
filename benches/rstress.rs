use std::{
    arch::x86_64::{__m256i, _mm256_add_epi8, _mm256_load_si256, _mm256_store_si256},
    fs,
    hint::black_box,
    os::raw::c_void,
    ptr::{null_mut, write_bytes},
    slice, thread,
    time::{Duration, Instant},
};

use criterion::{BenchmarkId, Criterion, Throughput, criterion_group, criterion_main};

fn rss_kb() -> Option<usize> {
    let status = fs::read_to_string("/proc/self/status").ok()?;

    status
        .lines()
        .find(|line| line.starts_with("VmRSS:"))
        .and_then(|line| line.split_whitespace().nth(1))
        .and_then(|v| v.parse().ok())
}

fn print_rss(label: &str) {
    if let Some(rss) = rss_kb() {
        eprintln!("[RSMalloc] {label} final RSS: {rss} KiB");
    }
}

unsafe extern "C" {
    fn malloc(size: usize) -> *mut c_void;
    fn free(ptr: *mut c_void);
    fn realloc(ptr: *mut c_void, size: usize) -> *mut c_void;
    fn aligned_alloc(align: usize, size: usize) -> *mut c_void;
    fn malloc_trim(pad: usize) -> i32;
}

#[derive(Clone, Copy)]
struct SendPtr(*mut u8, usize, u8);

unsafe impl Send for SendPtr {}
unsafe impl Sync for SendPtr {}

#[inline]
fn next_rand(state: &mut u64) -> u64 {
    let mut x = *state;
    x ^= x << 13;
    x ^= x >> 7;
    x ^= x << 17;
    *state = x;
    x
}

#[inline]
fn mixed_size(rand: u64) -> usize {
    match rand % 100 {
        0..=49 => [16, 32, 48, 64, 80, 96, 128][((rand >> 8) as usize) % 7],
        50..=74 => [160, 192, 256, 320, 384, 512][((rand >> 8) as usize) % 6],
        75..=89 => [768, 1024, 1536, 2048, 3072, 4096][((rand >> 8) as usize) % 6],
        90..=97 => [8192, 16384, 32768, 65536, 131072][((rand >> 8) as usize) % 5],
        _ => [3 * 1024 * 1024, 5 * 1024 * 1024][((rand >> 8) as usize) % 2],
    }
}

#[inline]
unsafe fn fill_and_probe(ptr: *mut u8, size: usize, byte: u8) {
    unsafe {
        ptr.write_bytes(byte, size);
        black_box(*ptr);
        black_box(*ptr.add(size - 1));
    }
}

#[inline]
unsafe fn check_prefix(ptr: *mut u8, valid_len: usize, byte: u8) {
    unsafe {
        black_box(*ptr);
        assert_eq!(*ptr, byte);
        assert_eq!(*ptr.add(valid_len - 1), byte);
    }
}

fn run_churn_once(threads: usize, iters_per_thread: usize, retained_per_thread: usize) -> usize {
    let mut handles = Vec::with_capacity(threads);

    for tid in 0..threads {
        handles.push(thread::spawn(move || {
            let mut rng = 0x9E37_79B9_7F4A_7C15u64 ^ tid as u64;
            let mut retained = Vec::with_capacity(retained_per_thread);
            let mut ops = 0usize;

            for iter in 0..iters_per_thread {
                let size = mixed_size(next_rand(&mut rng));
                let byte = next_rand(&mut rng) as u8;

                unsafe {
                    let ptr = malloc(size) as *mut u8;
                    assert!(!ptr.is_null(), "malloc returned null for size {size}");
                    fill_and_probe(ptr, size, byte);
                    ops += 1;

                    if iter % 11 == 0 {
                        let new_size = mixed_size(next_rand(&mut rng)).max(size / 2).max(1);
                        let new_ptr = realloc(ptr as *mut c_void, new_size) as *mut u8;
                        assert!(
                            !new_ptr.is_null(),
                            "realloc returned null for size {size} -> {new_size}"
                        );
                        check_prefix(new_ptr, size.min(new_size), byte);
                        ops += 1;

                        if retained.len() < retained_per_thread && iter % 22 == 0 {
                            retained.push(SendPtr(new_ptr, size.min(new_size), byte));
                        } else {
                            free(new_ptr as *mut c_void);
                            ops += 1;
                        }
                    } else if retained.len() < retained_per_thread && iter % 17 == 0 {
                        retained.push(SendPtr(ptr, size, byte));
                    } else {
                        free(ptr as *mut c_void);
                        ops += 1;
                    }

                    if iter % 29 == 0 {
                        let aligned = aligned_alloc(64, 4096) as *mut u8;
                        assert!(!aligned.is_null(), "aligned_alloc returned null");
                        assert_eq!((aligned as usize) & 63, 0);
                        fill_and_probe(aligned, 4096, byte.wrapping_add(1));
                        free(aligned as *mut c_void);
                        ops += 2;
                    }
                }
            }

            (retained, ops)
        }));
    }

    let mut retained_for_cross_thread_free = Vec::new();
    let mut ops = 0usize;

    for handle in handles {
        let (retained, thread_ops) = handle.join().expect("churn worker panicked");
        retained_for_cross_thread_free.extend(retained);
        ops += thread_ops;
    }

    for SendPtr(ptr, valid_len, byte) in retained_for_cross_thread_free {
        unsafe {
            check_prefix(ptr, valid_len, byte);
            free(ptr as *mut c_void);
            ops += 1;
        }
    }

    ops
}

fn run_remote_free_pressure(threads: usize, allocs_per_thread: usize) -> usize {
    assert!(
        threads >= 2,
        "remote_free_pressure requires at least two threads"
    );

    let mut producers = Vec::with_capacity(threads);

    for tid in 0..threads {
        producers.push(thread::spawn(move || {
            let mut rng = 0xD1B5_4A32_D192_ED03u64 ^ tid as u64;
            let mut ptrs = Vec::with_capacity(allocs_per_thread);

            for _ in 0..allocs_per_thread {
                let size = mixed_size(next_rand(&mut rng));
                let byte = next_rand(&mut rng) as u8;
                unsafe {
                    let ptr = malloc(size) as *mut u8;
                    assert!(!ptr.is_null());
                    fill_and_probe(ptr, size, byte);
                    ptrs.push(SendPtr(ptr, size, byte));
                }
            }

            ptrs
        }));
    }

    let mut buckets: Vec<Vec<SendPtr>> = (0..threads)
        .map(|_| Vec::with_capacity(allocs_per_thread))
        .collect();
    for (producer_id, handle) in producers.into_iter().enumerate() {
        for (i, ptr) in handle
            .join()
            .expect("producer panicked")
            .into_iter()
            .enumerate()
        {
            let offset = 1 + (i % (threads - 1));
            buckets[(producer_id + offset) % threads].push(ptr);
        }
    }

    let mut free_threads = Vec::with_capacity(threads);
    for bucket in buckets {
        free_threads.push(thread::spawn(move || {
            let mut ops = 0usize;
            for SendPtr(ptr, size, byte) in bucket {
                unsafe {
                    check_prefix(ptr, size, byte);
                    free(ptr as *mut c_void);
                    ops += 1;
                }
            }
            ops
        }));
    }

    let mut ops = threads * allocs_per_thread;
    for handle in free_threads {
        ops += handle.join().expect("freer panicked");
    }
    ops
}

fn run_realloc_ping_pong(threads: usize, chains_per_thread: usize) -> usize {
    const SIZES: [usize; 13] = [
        16,
        4096,
        32,
        131072,
        48,
        3 * 1024 * 1024,
        64,
        8192,
        80,
        5 * 1024 * 1024,
        96,
        16384,
        128,
    ];

    let mut handles = Vec::with_capacity(threads);
    for tid in 0..threads {
        handles.push(thread::spawn(move || {
            let mut ops = 0usize;
            for chain in 0..chains_per_thread {
                let byte = (tid as u8).wrapping_mul(31).wrapping_add(chain as u8);
                unsafe {
                    let mut size = SIZES[0];
                    let mut ptr = malloc(size) as *mut u8;
                    assert!(!ptr.is_null());
                    fill_and_probe(ptr, size, byte);
                    ops += 1;

                    for &next_size in &SIZES[1..] {
                        let next = realloc(ptr as *mut c_void, next_size) as *mut u8;
                        assert!(!next.is_null());
                        check_prefix(next, size.min(next_size), byte);
                        fill_and_probe(next, next_size, byte);
                        ptr = next;
                        size = next_size;
                        ops += 1;
                    }

                    free(ptr as *mut c_void);
                    ops += 1;
                }
            }
            ops
        }));
    }

    handles
        .into_iter()
        .map(|handle| handle.join().expect("realloc worker panicked"))
        .sum()
}

fn run_fragmentation_shuffle(threads: usize, rounds: usize, live_set: usize) -> usize {
    let mut handles = Vec::with_capacity(threads);

    for tid in 0..threads {
        handles.push(thread::spawn(move || {
            let mut rng = 0xA24B_AED4_963E_E407u64 ^ tid as u64;
            let mut live = Vec::with_capacity(live_set);
            let mut ops = 0usize;

            for round in 0..rounds {
                while live.len() < live_set {
                    let size = mixed_size(next_rand(&mut rng));
                    let byte = next_rand(&mut rng) as u8;
                    unsafe {
                        let ptr = malloc(size) as *mut u8;
                        assert!(!ptr.is_null());
                        fill_and_probe(ptr, size, byte);
                        live.push(SendPtr(ptr, size, byte));
                    }
                    ops += 1;
                }

                let parity = round & 1;
                let mut i = 0;
                while i < live.len() {
                    if (i & 1) == parity || (next_rand(&mut rng) & 7) == 0 {
                        let SendPtr(ptr, size, byte) = live.swap_remove(i);
                        unsafe {
                            check_prefix(ptr, size, byte);
                            free(ptr as *mut c_void);
                        }
                        ops += 1;
                    } else {
                        i += 1;
                    }
                }
            }

            for SendPtr(ptr, size, byte) in live {
                unsafe {
                    check_prefix(ptr, size, byte);
                    free(ptr as *mut c_void);
                }
                ops += 1;
            }

            ops
        }));
    }

    handles
        .into_iter()
        .map(|handle| handle.join().expect("fragmentation worker panicked"))
        .sum()
}

fn run_aligned_alloc_matrix(threads: usize, iters_per_thread: usize) -> usize {
    const CASES: [(usize, usize); 8] = [
        (16, 16),
        (64, 64),
        (64, 4096),
        (256, 4096),
        (4096, 4096),
        (4096, 65536),
        (2 * 1024 * 1024, 2 * 1024 * 1024),
        (2 * 1024 * 1024, 4 * 1024 * 1024),
    ];

    let mut handles = Vec::with_capacity(threads);
    for tid in 0..threads {
        handles.push(thread::spawn(move || {
            let mut ops = 0usize;
            for i in 0..iters_per_thread {
                let (align, size) = CASES[(tid + i) % CASES.len()];
                let byte = (tid as u8).wrapping_add(i as u8);
                unsafe {
                    let ptr = aligned_alloc(align, size) as *mut u8;
                    assert!(
                        !ptr.is_null(),
                        "aligned_alloc({align}, {size}) returned null"
                    );
                    assert_eq!((ptr as usize) & (align - 1), 0);
                    fill_and_probe(ptr, size, byte);
                    free(ptr as *mut c_void);
                }
                ops += 2;
            }
            ops
        }));
    }

    handles
        .into_iter()
        .map(|handle| handle.join().expect("aligned worker panicked"))
        .sum()
}

fn bench_thread_churn(c: &mut Criterion) {
    let cpus = thread::available_parallelism().map_or(1, |n| n.get());
    let mut group = c.benchmark_group("rstress_thread_churn");
    group.warm_up_time(Duration::from_secs(2));
    group.measurement_time(Duration::from_secs(10));

    let configs = [
        (cpus.max(1), 128usize, 4usize),
        ((cpus * 2).clamp(2, 128), 128usize, 4usize),
        ((cpus * 4).clamp(4, 256), 96usize, 4usize),
    ];

    for (threads, iters, retained) in configs {
        let expected_min_ops = threads * iters;
        group.throughput(Throughput::Elements(expected_min_ops as u64));
        group.bench_with_input(
            BenchmarkId::new("spawn_join_mixed_alloc_realloc_cross_free", threads),
            &(threads, iters, retained),
            |b, &(threads, iters, retained)| {
                b.iter_custom(|runs| {
                    let start = Instant::now();
                    let mut total_ops = 0usize;
                    for _ in 0..runs {
                        total_ops += black_box(run_churn_once(threads, iters, retained));
                    }
                    black_box(total_ops);
                    start.elapsed()
                });
            },
        );
    }

    group.finish();
    print_rss("rstress_thread_churn");
}

fn bench_allocator_edge_cases(c: &mut Criterion) {
    let cpus = thread::available_parallelism().map_or(1, |n| n.get());
    let mut group = c.benchmark_group("rstress_allocator_edge_cases");
    group.warm_up_time(Duration::from_secs(1));
    group.measurement_time(Duration::from_secs(12));

    let remote_threads = (cpus * 4).clamp(4, 256);
    let remote_allocs = 256usize;
    group.throughput(Throughput::Elements(
        (remote_threads * remote_allocs * 2) as u64,
    ));
    group.bench_function(
        BenchmarkId::new("remote_free_pressure_threads", remote_threads),
        |b| {
            b.iter_custom(|runs| {
                let start = Instant::now();
                let mut ops = 0usize;
                for _ in 0..runs {
                    ops += black_box(run_remote_free_pressure(remote_threads, remote_allocs));
                }
                black_box(ops);
                start.elapsed()
            });
        },
    );

    let realloc_threads = (cpus * 2).clamp(2, 128);
    let chains = 96usize;
    group.throughput(Throughput::Elements((realloc_threads * chains * 14) as u64));
    group.bench_function(
        BenchmarkId::new("realloc_ping_pong_size_classes_threads", realloc_threads),
        |b| {
            b.iter_custom(|runs| {
                let start = Instant::now();
                let mut ops = 0usize;
                for _ in 0..runs {
                    ops += black_box(run_realloc_ping_pong(realloc_threads, chains));
                }
                black_box(ops);
                start.elapsed()
            });
        },
    );

    let frag_threads = cpus.clamp(1, 64);
    let rounds = 32usize;
    let live_set = 256usize;
    group.throughput(Throughput::Elements(
        (frag_threads * rounds * live_set) as u64,
    ));
    group.bench_function(
        BenchmarkId::new("fragmentation_shuffle_threads", frag_threads),
        |b| {
            b.iter_custom(|runs| {
                let start = Instant::now();
                let mut ops = 0usize;
                for _ in 0..runs {
                    ops += black_box(run_fragmentation_shuffle(frag_threads, rounds, live_set));
                }
                black_box(ops);
                start.elapsed()
            });
        },
    );

    let aligned_threads = cpus.clamp(1, 64);
    let aligned_iters = 48usize;
    group.throughput(Throughput::Elements(
        (aligned_threads * aligned_iters * 2) as u64,
    ));
    group.bench_function(
        BenchmarkId::new("aligned_alloc_matrix_threads", aligned_threads),
        |b| {
            b.iter_custom(|runs| {
                let start = Instant::now();
                let mut ops = 0usize;
                for _ in 0..runs {
                    ops += black_box(run_aligned_alloc_matrix(aligned_threads, aligned_iters));
                }
                black_box(ops);
                start.elapsed()
            });
        },
    );

    group.finish();
    print_rss("rstress_allocator_edge_cases");
}

#[allow(unsafe_op_in_unsafe_fn)]
unsafe fn run_simd_test(thread_count: usize, iters_per_thread: usize) -> usize {
    let mut threads = Vec::with_capacity(thread_count);

    for _ in 0..thread_count {
        threads.push(std::thread::spawn(move || {
            let mut ops = 0usize;

            for _ in 0..iters_per_thread {
                let load_range = 1024usize;
                let inner_load = aligned_alloc(32, load_range) as *mut u8;
                assert!(!inner_load.is_null());
                assert_eq!((inner_load as usize) & 31, 0);

                write_bytes(inner_load, 1, load_range);

                let slice = slice::from_raw_parts_mut(inner_load, load_range);

                slice.chunks_exact_mut(64).for_each(|chunk| {
                    let loaded = _mm256_load_si256(chunk[..32].as_ptr() as *const __m256i);
                    let loaded2 = _mm256_load_si256(chunk[32..].as_ptr() as *const __m256i);

                    let added = _mm256_add_epi8(loaded, loaded2);
                    let added2 = _mm256_add_epi8(loaded2, added);

                    _mm256_store_si256(chunk[..32].as_mut_ptr() as *mut __m256i, added);
                    _mm256_store_si256(chunk[32..].as_mut_ptr() as *mut __m256i, added2);
                });

                black_box(*inner_load);
                black_box(*inner_load.add(load_range - 1));

                free(inner_load as *mut c_void);
                ops += 1;
            }

            ops
        }));
    }

    threads
        .into_iter()
        .map(|thread| thread.join().expect("simd worker panicked"))
        .sum()
}

fn bench_simd_alloc(c: &mut Criterion) {
    let cpus = thread::available_parallelism().map_or(1, |n| n.get());
    let mut group = c.benchmark_group("sbench_simd_alloc");
    group.warm_up_time(Duration::from_secs(2));
    group.measurement_time(Duration::from_secs(10));

    let configs = [
        (cpus.max(1), 1024usize),
        ((cpus * 2).clamp(2, 128), 1024usize),
        ((cpus * 4).clamp(4, 256), 768usize),
    ];

    for (threads, iters) in configs {
        group.throughput(Throughput::Elements((threads * iters) as u64));

        group.bench_with_input(
            BenchmarkId::new("simd_aligned_alloc_avx2_threads", threads),
            &(threads, iters),
            |b, &(threads, iters)| {
                b.iter_custom(|runs| {
                    let start = Instant::now();
                    let mut total_ops = 0usize;

                    for _ in 0..runs {
                        total_ops += black_box(unsafe { run_simd_test(threads, iters) });
                    }

                    black_box(total_ops);
                    start.elapsed()
                });
            },
        );
    }

    group.finish();
    print_rss("sbench_simd_alloc");
}

fn teardown_test(phases: usize, allocs_per_phase: usize) -> usize {
    let list = [
        16,
        32,
        15,
        64,
        12,
        1,
        1024,
        256,
        1,
        1024,
        256,
        16,
        32,
        64,
        12,
        1024 * 1024,
        1024 * 1024,
        256,
        16,
        16,
        16,
        16,
        16,
    ];

    let mut mem = Vec::with_capacity(allocs_per_phase);
    let mut ops = 0usize;

    for _ in 0..phases {
        for i in 0..allocs_per_phase {
            unsafe {
                let size = list[i % list.len()];
                let memory = malloc(size);
                assert_ne!(memory, null_mut());
                black_box(memory);
                mem.push(memory);
                ops += 1;
            }
        }

        unsafe {
            for memory in mem.drain(..) {
                free(memory);
                ops += 1;
            }
        }
    }

    ops
}

fn run_teardown(c: &mut Criterion) {
    let mut group = c.benchmark_group("bulk_phase_teardown_mixed_sizes");
    group.warm_up_time(Duration::from_secs(2));
    group.measurement_time(Duration::from_secs(10));

    let phases = 8usize;
    let allocs_per_phase = 16 * 1024usize;

    group.throughput(Throughput::Elements((phases * allocs_per_phase * 2) as u64));

    group.bench_function(
        BenchmarkId::new("bulk_phase_teardown_mixed_sizes", allocs_per_phase),
        |b| {
            b.iter_custom(|runs| {
                let start = Instant::now();
                let mut total_ops = 0usize;

                for _ in 0..runs {
                    total_ops += black_box(teardown_test(phases, allocs_per_phase));
                }

                black_box(total_ops);
                start.elapsed()
            });
        },
    );

    group.finish();
    print_rss("bulk_phase_teardown_mixed_sizes");
}

fn teardown_test_multi(threads: usize, phases: usize, allocs_per_phase: usize) -> usize {
    let mut handles = Vec::with_capacity(threads);

    for _ in 0..threads {
        handles.push(thread::spawn(move || {
            teardown_test(phases, allocs_per_phase)
        }));
    }

    handles
        .into_iter()
        .map(|handle| handle.join().expect("teardown worker panicked"))
        .sum()
}

fn run_teardown_multi(c: &mut Criterion) {
    let cpus = thread::available_parallelism().map_or(1, |n| n.get());

    let mut group = c.benchmark_group("bulk_phase_teardown_mixed_sizes_multi");
    group.warm_up_time(Duration::from_secs(2));
    group.measurement_time(Duration::from_secs(10));

    let configs = [
        (cpus.max(1), 8usize, 4 * 1024usize),
        ((cpus * 2).clamp(2, 128), 8usize, 4 * 1024usize),
        ((cpus * 4).clamp(4, 256), 6usize, 4 * 1024usize),
    ];

    for (threads, phases, allocs_per_phase) in configs {
        group.throughput(Throughput::Elements(
            (threads * phases * allocs_per_phase * 2) as u64,
        ));

        group.bench_with_input(
            BenchmarkId::new("bulk_phase_teardown_mixed_sizes_threads", threads),
            &(threads, phases, allocs_per_phase),
            |b, &(threads, phases, allocs_per_phase)| {
                b.iter_custom(|runs| {
                    let start = Instant::now();
                    let mut total_ops = 0usize;

                    for _ in 0..runs {
                        total_ops +=
                            black_box(teardown_test_multi(threads, phases, allocs_per_phase));
                    }

                    black_box(total_ops);
                    start.elapsed()
                });
            },
        );
    }

    group.finish();
    print_rss("bulk_phase_teardown_mixed_sizes_multi");
}

fn bench_trim_pressure(c: &mut Criterion) {
    let mut group = c.benchmark_group("trim_pressure");
    const SIZES: &[usize] = &[4096, 8192, 16384, 32768, 65536];
    const BOUNDARY_SIZES: &[usize] = &[3072, 4095, 4096, 4097, 8191, 8192];

    group.bench_function("alloc_free_trim_classes", |b| {
        b.iter(|| unsafe {
            let mut ptrs = Vec::with_capacity(4096);

            for i in 0..4096 {
                let size = SIZES[i % SIZES.len()];
                let ptr = malloc(size);
                black_box(ptr);
                ptrs.push(ptr);
            }

            for ptr in ptrs {
                free(ptr);
            }
        });
    });

    group.bench_function("manual_malloc_trim_after_free", |b| {
        b.iter(|| unsafe {
            let mut ptrs = Vec::with_capacity(4096);

            for i in 0..4096 {
                let size = SIZES[i % SIZES.len()];
                let ptr = malloc(size);
                black_box(ptr);
                ptrs.push(ptr);
            }

            for ptr in ptrs {
                free(ptr);
            }

            black_box(malloc_trim(0));
        });
    });

    group.bench_function("trim_boundary_size_classes", |b| {
        b.iter(|| unsafe {
            let mut ptrs = Vec::with_capacity(4096);

            for i in 0..4096 {
                let size = BOUNDARY_SIZES[i % BOUNDARY_SIZES.len()];
                let ptr = malloc(size) as *mut u8;
                assert!(!ptr.is_null());
                fill_and_probe(ptr, size, i as u8);
                ptrs.push((ptr, size, i as u8));
            }

            for (ptr, size, byte) in ptrs {
                check_prefix(ptr, size, byte);
                free(ptr as *mut c_void);
            }

            black_box(malloc_trim(0));
        });
    });

    group.finish();
}

criterion_group!(
    benches,
    bench_thread_churn,
    bench_allocator_edge_cases,
    bench_simd_alloc,
    run_teardown,
    run_teardown_multi,
    bench_trim_pressure,
);
criterion_main!(benches);
