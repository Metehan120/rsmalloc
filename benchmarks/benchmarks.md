# Benchmarks

This directory contains development benchmark snapshots for RSMalloc and several widely used allocators. See [**Detailed results: per-workload tables and time/RSS charts**](benchmarks_detailed.md) for a readable comparison of all 22 workloads. The raw measurements are in [`benchmark_overall.txt`](benchmark_overall.txt).

A reproducible MariaDB/sysbench memory harness is available under [`mariadb/`](mariadb/README.md).

## Important Warning

These results are development signals, not general performance guarantees. CPU topology, kernel and libc versions, compiler flags, preload mode, THP policy, NUMA layout, background activity, and allocator configuration can materially change the results.

Test the allocator with the target workload and production configuration before drawing conclusions.

## Current Snapshot

The current release-candidate snapshot includes:

- RSMalloc `0.3.0-alpha`
- glibc `2.44`
- tcmalloc, latest "4.6.5" CachyOS build at collection time
- mimalloc `3.5`
- jemalloc `5.3.1`

The raw file contains 22 alpha-3 results, and aggregate charts use all **22 tests shared by all five allocators**.

All five allocators now use **column-wise medians across ten runs for the same 20 workloads**. RSMalloc uses the previously supplied ten-run results; mimalloc, jemalloc, tcmalloc, and glibc use the newly supplied ten-run results. With an even run count, each median is the average of the fifth and sixth sorted observations; fractional RSS or fault counts and extra timing precision reflect that calculation, not increased measurement precision. `rbstressN` and `z3` were not included in these runs and retain older single-run measurements for every allocator. The 22-test aggregates therefore mix 20 repeated-run medians with two older results. Allocator version labels and environment details below are retained from the existing snapshot; the new measurements do not independently record build flags or configuration.

Historical alpha-2 rows remain in the raw file for development context but are not included in the charts.

## Test Environment

The snapshot was collected with `mimalloc-bench` on:

- CPU: AMD Ryzen 5 5600X
- RAM: 16 GiB DDR4-3200
- OS: CachyOS, kernel `7.2.6-1-cachyos-bore`
- Desktop: KDE Plasma
- Motherboard: MSI B550M PRO-VDH

The alpha-3 snapshot used kernel `7.2.6-1-cachyos-bore`, which differs from some historical rows. This is another reason not to treat cross-version RSS as directly comparable.

The raw columns are:

- `time`: elapsed time in seconds
- `rss`: resident set size in KiB
- `user`: user CPU time in seconds
- `sys`: system CPU time in seconds
- `page-faults`: major page faults
- `page-reclaims`: minor faults/reclaims reported by the harness

Lower is generally better, but allocator tradeoffs are workload-dependent.

## Reading the Results

The synthetic tests exercise very different allocator behavior. In particular:

- `alloc-testN` performs roughly 600 million allocations and 600 million frees, amplifying every instruction in the local allocation/free paths.
- `sh6benchN` and `sh8benchN` are difficult contention and scheduler-interaction cases for RSMalloc's RSEQ-centered design.
- `xmalloc-testN` strongly favors very small conventional fast paths and remains an expected RSMalloc weakness.

The tables should therefore be read as a workload profile, not as a universal allocator ranking.

## Summary

The following values were calculated from the 22-test common set in [`benchmark_overall.txt`](benchmark_overall.txt). Exact ties at the harness's reported precision are excluded from clean winner counts.

### Per-test winner counts

Five elapsed-time tests tie:

- `cache-scratch1`: all five allocators
- `cache-scratchN`: glibc, tcmalloc, and jemalloc
- `mstressN`: tcmalloc and mimalloc
- `rbstressN`: RSMalloc and tcmalloc
- `z3`: all five allocators

The clean elapsed-time wins therefore total 17 rather than 22.

```mermaid
xychart-beta
    title "Fastest-time clean wins across 22 common tests"
    x-axis [rsmalloc, glibc, tcmalloc, mimalloc, jemalloc]
    y-axis "wins" 0 --> 22
    bar [4, 2, 3, 8, 0]
```

```mermaid
xychart-beta
    title "Lowest-RSS wins across 22 common tests"
    x-axis [rsmalloc, glibc, tcmalloc, mimalloc, jemalloc]
    y-axis "wins" 0 --> 22
    bar [2, 15, 4, 1, 0]
```

### Overall relative score

For each test, an allocator's result is divided by the best result for that test. The chart reports the geometric mean of those ratios multiplied by 100. A score of `100` would match the best allocator on every test; lower is better.

```mermaid
xychart-beta
    title "Elapsed-time relative score"
    x-axis [rsmalloc, glibc, tcmalloc, mimalloc, jemalloc]
    y-axis "score x100" 0 --> 180
    bar [122, 156, 141, 104, 114]
```

```mermaid
xychart-beta
    title "RSS relative score"
    x-axis [rsmalloc, glibc, tcmalloc, mimalloc, jemalloc]
    y-axis "score x100" 0 --> 240
    bar [156, 117, 162, 189, 233]
```

### Stress cases

```mermaid
xychart-beta
    title "sh6benchN elapsed time"
    x-axis [rsmalloc, glibc, tcmalloc, mimalloc, jemalloc]
    y-axis "milliseconds" 0 --> 3400
    bar [310, 3015, 200, 180, 290]
```

```mermaid
xychart-beta
    title "sh6benchN RSS"
    x-axis [rsmalloc, glibc, tcmalloc, mimalloc, jemalloc]
    y-axis "MiB" 0 --> 400
    bar [361, 334, 216, 213, 291]
```

```mermaid
xychart-beta
    title "sh8benchN elapsed time"
    x-axis [rsmalloc, glibc, tcmalloc, mimalloc, jemalloc]
    y-axis "milliseconds" 0 --> 14000
    bar [1595, 12140, 4125, 440, 850]
```

```mermaid
xychart-beta
    title "sh8benchN RSS"
    x-axis [rsmalloc, glibc, tcmalloc, mimalloc, jemalloc]
    y-axis "MiB" 0 --> 260
    bar [172, 234, 127, 248, 244]
```

### Page-reclaim and system-time totals

These totals cover the same 22-test common set and are not included in the relative scores.

| Allocator | Minor page-reclaims | System CPU time |
| --- | ---: | ---: |
| RSMalloc | 113,996.5 | 4.435 s |
| glibc | 867,421.5 | 62.780 s |
| tcmalloc | 460,133 | 44.505 s |
| mimalloc | 114,258 | 4.085 s |
| jemalloc | 1,287,155.5 | 6.960 s |

Instrumentation, scheduler variation, and outlier stress tests can strongly affect these totals. Use the raw per-test rows when investigating a specific result.
