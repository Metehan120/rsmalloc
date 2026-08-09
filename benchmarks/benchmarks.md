# Benchmarks

This directory contains development benchmark snapshots for rsmalloc and a few
widely used allocators. The raw overall result table is stored in
[`benchmark_overall.txt`](benchmark_overall.txt).

See [`real_workloads.md`](real_workloads.md) for microarchitectural observations.

## Important Warning

These numbers do **not** reflect guaranteed real-world application performance.
They are a development signal for allocator behavior under this specific
benchmark setup. Results can change with CPU topology, kernel version, compiler
flags, preload/global-allocator mode, workload mix, background system activity,
THP settings, NUMA layout, and allocator configuration.

Do not use these results as a stable performance claim. Use them to understand
where rsmalloc currently looks strong or weak, then test with your own workload.

## Current Snapshot

The current snapshot compares:

* `rsmalloc 0.2.0-alpha`
* `tcmalloc 4.6.5`
* `mimalloc 3.4.4`
* `jemalloc 5.3.1`
* `rpmalloc` (default build, march native)

`benchmark_overall.txt` also carries two extra rsmalloc runs for reference:
`0.1.0-alpha` (THP could not be forced by the kernel at that point) and
`0.2.0-alpha` built with `page-backend-no-huge-page` (THP explicitly disabled
for slab arenas). Those two are not part of the charts below; they exist to
show the RSS/time delta THP handling makes on its own. See the raw file for
those rows.

`mimalloc-bench`'s `lean` test is currently broken in this environment and is
excluded from this snapshot; it will be added back soon enough.

## Test Environment

This snapshot was collected with `mimalloc-bench` on:

* CPU: AMD Ryzen 5 5600X
* RAM: 16GB RAM DDR4 3200MHz
* OS: CachyOS 7.1.4-cachyos-bore
* Desktop Environment: KDE Plasma 6.7.3
* Environment Temperature: ~28-30C
* CPU Cooler: Arctic Freezer 36
* Motherboard: MSI B550M PRO-VDH
* BIOS: 2.M0 (Reported by dmidecode)
* GPU: ASUS ROG Strix OC RX 6600 XT

The table records:

* `time`: elapsed benchmark time
* `rss`: resident set size reported by the benchmark harness
* `user`: user CPU time
* `sys`: system CPU time
* `page-faults`: major page faults
* `page-reclaims`: minor page faults / reclaims as reported by the harness

Lower values are generally better for `time`, `rss`, CPU time, and fault counts,
but allocator tradeoffs are workload-dependent. A result that is good for one
test can be bad for another real application.

## Reading The Results

The table should be read as a rough comparison across allocator behavior, not as
a ranking. Some tests are closer to synthetic allocator stress, while others are
larger application-style workloads. rsmalloc is still alpha software, so both
performance and memory behavior are expected to move as internals change.

The `sh6benchN` and `sh8benchN` tests are especially unfriendly to the current
RSEQ-centered design and should be treated as worst-case RSEQ stress tests, not
as representative application behavior.

In some tests, the RSEQ critical sections abort extremely frequently because the
kernel preempts or migrates the running thread inside the critical section. Those
aborts force retry/fallback work and can account for a large part of rsmalloc's
runtime in the affected benchmarks. These results therefore measure both allocator
policy and workload/scheduler interaction with RSEQ.

## Mermaid Summary

These column charts are parsed from [`benchmark_overall.txt`](benchmark_overall.txt). Lower is better
for elapsed time, RSS, and relative-to-best scores. RSS values are converted from
KiB to MiB for readability. The `rsmalloc` series here is the
`0.2.0-alpha (THP handled by kernel)` run.

### Per-test winner counts

4 of the 19 tests land on an exact tie at the harness's reported precision
(`gs`: rsmalloc/mimalloc/rpmalloc; `alloc-testN`: mimalloc/rpmalloc;
`cache-scratch1`: all five; `cache-scratchN`: rsmalloc/tcmalloc). Ties are not
counted as a win for anyone below, so the bars sum to 15, not 19.

```mermaid
xychart-beta
    title "Fastest-time clean wins across 19 tests (4 ties excluded)"
    x-axis [rsmalloc, tcmalloc, mimalloc, jemalloc, rpmalloc]
    y-axis "wins" 0 --> 19
    bar [3, 3, 3, 1, 5]
```

```mermaid
xychart-beta
    title "Lowest-RSS wins across 19 tests"
    x-axis [rsmalloc, tcmalloc, mimalloc, jemalloc, rpmalloc]
    y-axis "wins" 0 --> 19
    bar [4, 8, 2, 0, 5]
```

### Overall relative score

A score of `100` means matching the best observed allocator for every test.
Higher values are worse. These are geometric means of each allocator's per-test
ratio to the best result for that test, scaled by `100` for cleaner chart axes.
For example, `128` means `1.28x` the per-test best.


```mermaid
xychart-beta
    title "Elapsed-time relative score, lower is better"
    x-axis [rsmalloc, tcmalloc, mimalloc, jemalloc, rpmalloc]
    y-axis "score x100" 0 --> 220
    bar [128, 147, 108, 116, 113]
```

```mermaid
xychart-beta
    title "RSS relative score, lower is better"
    x-axis [rsmalloc, tcmalloc, mimalloc, jemalloc, rpmalloc]
    y-axis "score x100" 0 --> 220
    bar [118, 119, 143, 174, 177]
```

### `sh6benchN` stress case

`sh6benchN` is the worst RSS case for this rsmalloc snapshot.

```mermaid
xychart-beta
    title "sh6benchN elapsed time, lower is better"
    x-axis [rsmalloc, tcmalloc, mimalloc, jemalloc, rpmalloc]
    y-axis "milliseconds" 0 --> 400
    bar [320, 190, 200, 270, 130]
```

```mermaid
xychart-beta
    title "sh6benchN RSS, lower is better"
    x-axis [rsmalloc, tcmalloc, mimalloc, jemalloc, rpmalloc]
    y-axis "MiB" 0 --> 400
    bar [361, 216, 213, 291, 293]
```

### `sh8benchN` stress case

`sh8benchN` is a RSEQ worst case for tcmalloc and, to a lesser extent, rsmalloc
in this snapshot. The values below come from the current
[`benchmark_overall.txt`](benchmark_overall.txt) snapshot.

```mermaid
xychart-beta
    title "sh8benchN elapsed time, lower is better"
    x-axis [rsmalloc, tcmalloc, mimalloc, jemalloc, rpmalloc]
    y-axis "milliseconds" 0 --> 4400
    bar [1400, 4180, 440, 830, 540]
```

```mermaid
xychart-beta
    title "sh8benchN RSS, lower is better"
    x-axis [rsmalloc, tcmalloc, mimalloc, jemalloc, rpmalloc]
    y-axis "MiB" 0 --> 340
    bar [171, 126, 249, 239, 321]
```

### Page-reclaim and sys-time behaviour

These aren't part of the time/RSS charts above but stand out in the raw data.
Totals are summed across all 19 tests.

| allocator | total minor page-reclaims | total sys time (s) |
|---|---|---|
| rsmalloc  | 78,267    | 4.70  |
| tcmalloc  | 371,609   | 44.39 |
| mimalloc  | 97,032    | 5.60  |
| jemalloc  | 1,217,941 | 7.26  |
| rpmalloc  | 2,071,655 | 9.06  |

```mermaid
xychart-beta
    title "Total minor page-reclaims across 19 tests, lower is better"
    x-axis [rsmalloc, tcmalloc, mimalloc, jemalloc, rpmalloc]
    y-axis "reclaims" 0 --> 2100000
    bar [78267, 371609, 97032, 1217941, 2071655]
```

```mermaid
xychart-beta
    title "Total sys time across 19 tests (seconds), lower is better"
    x-axis [rsmalloc, tcmalloc, mimalloc, jemalloc, rpmalloc]
    y-axis "seconds" 0 --> 46
    bar [4.7, 44.39, 5.6, 7.26, 9.06]
```

For real evaluation, run the allocator against the target application with the
same deployment mode and configuration that would be used in production.
