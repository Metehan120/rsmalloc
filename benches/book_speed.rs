// WARNING: This benchmark only measures the speed bookkeeping speed of the allocator, not the real performance of the allocator.

use std::{hint::black_box, os::raw::c_void};

use criterion::{Criterion, criterion_group, criterion_main};
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
    group.bench_function("64B", |b| {
        b.iter(|| unsafe {
            let ptr = black_box(malloc(64));
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

criterion_group!(benches, bench_alloc_free,);

criterion_main!(benches);
