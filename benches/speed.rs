use std::{
    hint::black_box,
    sync::{Arc, Barrier},
};

use criterion::{BenchmarkId, Criterion, criterion_group, criterion_main};
use rsmalloc::inner::{free::rs_free, malloc::alloc, realloc::rs_realloc};

fn bench_alloc_free(c: &mut Criterion) {
    let mut group = c.benchmark_group("alloc_free");

    // 64B
    group.bench_function("64B", |b| {
        b.iter(|| unsafe {
            let ptr = black_box(alloc(64));
            black_box(rs_free(ptr));
        });
    });

    // 4KB
    group.bench_function("4KB", |b| {
        b.iter(|| unsafe {
            let ptr = black_box(alloc(4096));
            black_box(rs_free(ptr));
        });
    });

    // 1MB
    group.bench_function("1MB", |b| {
        b.iter(|| unsafe {
            let ptr = black_box(alloc(1024 * 1024));
            black_box(rs_free(ptr));
        });
    });

    // 3MB
    group.bench_function("3MB", |b| {
        b.iter(|| unsafe {
            let ptr = black_box(alloc(3 * 1024 * 1024));
            black_box(rs_free(ptr));
        });
    });

    group.finish();
}

fn bench_realloc(c: &mut Criterion) {
    let mut group = c.benchmark_group("realloc");

    group.bench_function("grow_64B_to_4KB", |b| {
        b.iter(|| unsafe {
            let ptr = alloc(64);
            let out = black_box(rs_realloc(ptr, 4096));
            black_box(rs_free(out));
        });
    });

    group.bench_function("shrink_4KB_to_64B", |b| {
        b.iter(|| unsafe {
            let ptr = alloc(4096);
            let out = black_box(rs_realloc(ptr, 64));
            black_box(rs_free(out));
        });
    });

    group.bench_function("same_class_80B_to_79B", |b| {
        b.iter(|| unsafe {
            let ptr = alloc(80);
            let out = black_box(rs_realloc(ptr, 79));
            black_box(rs_free(out));
        });
    });

    group.bench_function("small_to_big_4KB_to_3MB", |b| {
        b.iter(|| unsafe {
            let ptr = alloc(4096);
            let out = black_box(rs_realloc(ptr, 3 * 1024 * 1024));
            if !out.is_null() {
                black_box(rs_free(out));
            }
        });
    });

    group.bench_function("big_to_small_3MB_to_4KB", |b| {
        b.iter(|| unsafe {
            let ptr = alloc(3 * 1024 * 1024);
            let out = black_box(rs_realloc(ptr, 4096));
            if !out.is_null() {
                black_box(rs_free(out));
            }
        });
    });

    group.finish();
}

criterion_group!(benches, bench_alloc_free, bench_realloc,);

criterion_main!(benches);
