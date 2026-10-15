// Predictor bookkeeping only, not allocator throughput or forecast accuracy.
// Sampling/training use synthetic timestamps; elapsed_clock measures the real clock separately.
use criterion::{BatchSize, Criterion, Throughput, criterion_group, criterion_main};
use rsmalloc::experimental_predictor::{AdaptiveBatching, demand_clock_ns};
use std::hint::black_box;

const INIT: usize = 128;
const MAX: usize = 1024;
const STRIDE: usize = 64;

fn complete(predictor: &AdaptiveBatching, started: u64, delivered: usize) {
    predictor
        .begin_demand_sample(black_box(started), INIT, MAX, STRIDE, true)
        .finish(started + 100, black_box(delivered), true);
}

fn seeded() -> AdaptiveBatching {
    let predictor = AdaptiveBatching::new();
    complete(&predictor, 0, 64);
    complete(&predictor, 50_100, 64);
    predictor
}

fn bench_predictor(c: &mut Criterion) {
    let mut group = c.benchmark_group("predictor");
    group.throughput(Throughput::Elements(1));

    black_box(demand_clock_ns());
    group.bench_function("elapsed_clock", |b| {
        b.iter(|| black_box(demand_clock_ns()));
    });

    let predictor = seeded();
    group.bench_function("forecast_read", |b| {
        b.iter(|| black_box(black_box(&predictor).expected_demand(black_box(INIT))));
    });

    // Fresh seeded state per measurement keeps the gate closed/open exactly;
    // setup and initialization are outside the measured routine.
    group.bench_function("completion_gated", |b| {
        b.iter_batched_ref(
            seeded,
            |predictor| complete(black_box(predictor), 51_000, 64),
            BatchSize::SmallInput,
        );
    });
    group.bench_function("completion_training", |b| {
        b.iter_batched_ref(
            seeded,
            |predictor| complete(black_box(predictor), 100_200, 64),
            BatchSize::SmallInput,
        );
    });

    // A deterministic burst/quiet trace exercises persistent model history and
    // both gate outcomes, without generating random numbers in the timed path.
    const TRACE: [(u64, usize); 8] = [
        (1_000, 8),
        (4_000, 32),
        (10_000, 128),
        (50_000, 64),
        (2_000, 256),
        (20_000, 16),
        (80_000, 32),
        (5_000, 64),
    ];
    let predictor = seeded();
    let mut now = 50_200_u64;
    group.throughput(Throughput::Elements(TRACE.len() as u64));
    group.bench_function("mixed_refill_trace", |b| {
        b.iter(|| {
            for &(gap, delivered) in black_box(&TRACE) {
                now += gap;
                complete(black_box(&predictor), now, delivered);
                now += 100;
                black_box(predictor.expected_demand(INIT));
            }
        });
    });
    group.finish();
}

criterion_group!(benches, bench_predictor);
criterion_main!(benches);
