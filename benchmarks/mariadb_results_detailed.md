# Detailed MariaDB benchmark results

[Summary and charts](mariadb_results.md) · [Harness](mariadb/README.md)

## Dataset and methodology

Source: `/tmp/rsmalloc-mariadb-bench/results/{rsmalloc,system,tcmalloc,mimalloc,jemalloc}/`, collected **2026-10-23**. This report transcribes recorded results; no workloads were rerun to produce it.

Each directory contains `environment.txt`, `workload.txt`, `warmup.txt`, `memory.csv`, `memory-summary.csv`, `maps.txt`, and `mariadb.log`. There is **one completed run per configuration**. No median, confidence interval, or repeated-run variability is available. The original files remain outside the repository; these reports preserve the aggregate results, not the complete raw time series or logs.

### Recorded environment

| Setting | Value |
| --- | --- |
| Kernel | Linux `7.2.9-2-cachyos`, `PREEMPT_DYNAMIC`, x86-64 |
| Storage | SanDisk SSD Plus 1TB A3N |
| Database | `mariadbd Ver 13.0.2-MariaDB for Linux on x86_64 (Arch Linux)` |
| Driver | sysbench `1.0.20-1472a05`, system LuaJIT `2.1.1720049189` |
| Tables | 8 |
| Rows per table | 250,000 |
| Client threads | 32 |
| Buffer pool | `256M` |
| Read-only warmup | 30 seconds |
| Read/write workload | 60 seconds |
| Idle observation | 15 seconds |
| Requested sample interval | 0.20 seconds |

The checked-in harness uses a fresh copy of the prepared database per variant, `--no-defaults`, `--skip-networking`, a private Unix socket, `innodb-flush-log-at-trx-commit=2`, `innodb-doublewrite=0`, `performance-schema=OFF`, and `thread-cache-size=0`. These are benchmark settings, **not production durability recommendations**. See [`mariadb/run.sh`](mariadb/run.sh) for the complete harness; the environment records do not independently archive every server option.

CPU, RAM capacity, THP policy, allocator package versions/build options, and RSMalloc commit/compiler/features are not recorded. A library's SONAME is not sufficient to infer its exact package release. In particular, the `system` label is retained as **system default**, without assigning an unrecorded glibc version.

### Allocator loading

| Label | Requested library (`environment.txt`) | Matching recorded mapping |
| --- | --- | --- |
| rsmalloc | `/home/metehan/Documents/rsmalloc/target/release/librsmalloc.so` | Same path |
| system | `system-default` | `/usr/lib/libc.so.6`; no alternative allocator requested |
| tcmalloc | `/lib64/libtcmalloc.so.4.6.5` | `/usr/lib/libtcmalloc.so.4.6.5` |
| mimalloc | `/lib64/libmimalloc.so.3.5` | `/usr/lib/libmimalloc.so.3.5` |
| jemalloc | `/lib64/libjemalloc.so.2` | `/usr/lib/libjemalloc.so.2` |

The mappings confirm the requested libraries were loaded, not complete interception of all allocations. Server shutdown timestamps give the observed sequence **tcmalloc → RSMalloc → jemalloc → mimalloc → system**. No rotated-order repetitions are present in this dataset.

## Read/write workload: throughput and SQL counts

Values are the final aggregate sysbench statistics, not averages of its one-second reports.

| Allocator | Transactions | Transactions/s | Total queries | Queries/s | Ignored errors | Errors/s | Reconnects | Duration (s) |
| --- | ---: | ---: | ---: | ---: | ---: | ---: | ---: | ---: |
| RSMalloc | 464,846 | 7,746.64 | 9,371,092 | 156,168.79 | 4,626 | 77.09 | 0 | 60.0059 |
| System default | 460,583 | 7,675.65 | 9,286,920 | 154,767.12 | 4,698 | 78.29 | 0 | 60.0055 |
| tcmalloc | 462,550 | 7,708.23 | 9,327,630 | 155,441.61 | 4,785 | 79.74 | 0 | 60.0070 |
| mimalloc | 463,097 | 7,717.39 | 9,337,967 | 155,614.73 | 4,746 | 79.09 | 0 | 60.0067 |
| jemalloc | 459,133 | 7,651.20 | 9,258,070 | 154,280.59 | 4,704 | 78.39 | 0 | 60.0077 |

| Allocator | Read queries | Write queries | Other queries |
| --- | ---: | ---: | ---: |
| RSMalloc | 6,572,608 | 1,864,166 | 934,318 |
| System default | 6,513,934 | 1,847,122 | 925,864 |
| tcmalloc | 6,542,690 | 1,855,055 | 929,885 |
| mimalloc | 6,549,802 | 1,857,225 | 930,940 |
| jemalloc | 6,493,718 | 1,841,382 | 922,970 |

The ignored errors are included explicitly because these are not error-free workload reports. Their codes are not recorded in the supplied output. Similar counts across variants do not establish their cause. No allocator-specific failure conclusion is drawn from them.

### Read/write latency

All values are **milliseconds**. The sums aggregate event latency across concurrent workers and therefore exceed wall-clock duration.

| Allocator | Minimum | Mean | Maximum | p95 | Sum |
| --- | ---: | ---: | ---: | ---: | ---: |
| RSMalloc | 1.16 | 4.13 | 26.95 | 6.09 | 1,919,610.24 |
| System default | 1.22 | 4.17 | 47.56 | 6.32 | 1,919,584.56 |
| tcmalloc | 1.17 | 4.15 | 28.02 | 6.21 | 1,919,636.98 |
| mimalloc | 1.19 | 4.15 | 28.07 | 6.09 | 1,919,590.27 |
| jemalloc | 1.17 | 4.18 | 41.18 | 6.32 | 1,919,587.59 |

No p99 measurement is present. Maximum latency is the largest observed event, not an estimate of p99.

### Read/write worker fairness

These are sysbench's per-thread averages and standard deviations, not variability across independent runs.

| Allocator | Events/thread mean | Events/thread stddev | Execution time/thread mean (s) | Execution time/thread stddev (s) |
| --- | ---: | ---: | ---: | ---: |
| RSMalloc | 14,526.4375 | 238.70 | 59.9878 | 0.00 |
| System default | 14,393.2188 | 293.83 | 59.9870 | 0.00 |
| tcmalloc | 14,454.6875 | 256.88 | 59.9887 | 0.00 |
| mimalloc | 14,471.7812 | 334.92 | 59.9872 | 0.00 |
| jemalloc | 14,347.9062 | 217.67 | 59.9871 | 0.00 |

## Read-only warmup

The warmup is a separate 30-second read-only phase, not included in the read/write throughput above. All warmups report zero ignored errors and zero reconnects.

| Allocator | Transactions | Transactions/s | Total queries | Queries/s | Duration (s) | Mean latency (ms) | p95 (ms) | Maximum (ms) |
| --- | ---: | ---: | ---: | ---: | ---: | ---: | ---: | ---: |
| RSMalloc | 303,178 | 10,104.52 | 4,850,848 | 161,672.29 | 30.0038 | 3.17 | 4.25 | 10.58 |
| System default | 303,135 | 10,103.05 | 4,850,160 | 161,648.88 | 30.0039 | 3.17 | 4.10 | 11.83 |
| tcmalloc | 302,694 | 10,088.46 | 4,843,104 | 161,415.42 | 30.0035 | 3.17 | 4.18 | 14.56 |
| mimalloc | 302,405 | 10,078.78 | 4,838,480 | 161,260.48 | 30.0037 | 3.17 | 4.10 | 13.78 |
| jemalloc | 301,243 | 10,039.52 | 4,819,888 | 160,632.38 | 30.0053 | 3.19 | 4.18 | 13.21 |

## Per-phase memory peaks

All following memory tables use **KiB**, preserving source precision. Each cell is an independent sampled maximum within its phase; a row need not describe a single simultaneous memory snapshot. RSS and high-water RSS come from process status, while PSS/anonymous/private/swap come from rollup sampling; these reads are not atomic. HWM is the process's lifetime resident high-water mark observed during that phase, not a resettable phase-specific counter.

The summary's `peak_swap_kb` cells are blank. All raw samples have `swap_kb=0`, so swap is reported below as **0 from the raw series**, not inferred from those blank cells.

| Allocator | Phase | Peak RSS | Observed peak HWM | Peak PSS | Peak anonymous | Peak private | Peak swap |
| --- | --- | ---: | ---: | ---: | ---: | ---: | ---: |
| RSMalloc | startup | 247,380 | 247,380 | 237,111 | 221,164 | 236,284 | 0 |
| RSMalloc | warmup | 426,128 | 426,128 | 415,852 | 399,080 | 415,032 | 0 |
| RSMalloc | workload | 457,812 | 457,812 | 447,537 | 430,572 | 446,716 | 0 |
| RSMalloc | idle | 457,576 | 457,576 | 447,307 | 430,336 | 446,480 | 0 |
| System default | startup | 221,620 | 221,620 | 211,449 | 195,724 | 210,624 | 0 |
| System default | warmup | 410,624 | 410,624 | 400,446 | 383,736 | 399,628 | 0 |
| System default | workload | 449,992 | 449,992 | 439,814 | 423,040 | 438,996 | 0 |
| System default | idle | 449,768 | 449,768 | 439,597 | 422,816 | 438,772 | 0 |
| tcmalloc | startup | 232,684 | 232,684 | 222,259 | 206,492 | 221,452 | 0 |
| tcmalloc | warmup | 410,672 | 410,672 | 400,240 | 383,504 | 399,440 | 0 |
| tcmalloc | workload | 451,908 | 451,908 | 441,509 | 424,636 | 440,684 | 0 |
| tcmalloc | idle | 451,560 | 451,560 | 441,160 | 424,280 | 440,328 | 0 |
| mimalloc | startup | 248,252 | 248,252 | 237,960 | 222,492 | 237,136 | 0 |
| mimalloc | warmup | 432,764 | 432,764 | 422,419 | 406,148 | 421,584 | 0 |
| mimalloc | workload | 479,548 | 479,548 | 469,203 | 452,804 | 468,368 | 0 |
| mimalloc | idle | 479,344 | 479,344 | 469,006 | 452,600 | 468,164 | 0 |
| jemalloc | startup | 271,460 | 271,460 | 261,261 | 245,320 | 260,444 | 0 |
| jemalloc | warmup | 462,032 | 462,032 | 451,826 | 434,868 | 451,016 | 0 |
| jemalloc | workload | 491,052 | 491,052 | 480,850 | 463,764 | 480,040 | 0 |
| jemalloc | idle | 448,344 | 490,456 | 438,145 | 421,052 | 437,328 | 0 |

Virtual-size measurements are excluded from this report because the recorded values appear anomalous and have not been validated. The source files are unchanged; RSS, PSS, and private/anonymous measurements are retained.

## Final idle samples

Unlike `memory-summary.csv`, which reports phase maxima, these rows use the **last row of `memory.csv`**. Elapsed time is measured from the sampler's start, not from the start of idle.

| Allocator | Elapsed (ms) | RSS (KiB) | PSS (KiB) | Anonymous (KiB) | Private (KiB) | Swap (KiB) |
| --- | ---: | ---: | ---: | ---: | ---: | ---: |
| RSMalloc | 105,144 | 421,232 | 410,963 | 393,992 | 410,136 | 0 |
| System default | 105,186 | 449,768 | 439,597 | 422,816 | 438,772 | 0 |
| tcmalloc | 105,128 | 451,560 | 441,160 | 424,280 | 440,328 | 0 |
| mimalloc | 105,128 | 479,344 | 469,006 | 452,600 | 468,164 | 0 |
| jemalloc | 105,196 | 448,344 | 438,145 | 421,052 | 437,328 | 0 |

| Allocator | Workload peak PSS − final idle PSS (KiB) | Same difference (MiB) |
| --- | ---: | ---: |
| RSMalloc | 36,574 | 35.717 |
| System default | 217 | 0.212 |
| tcmalloc | 349 | 0.341 |
| mimalloc | 197 | 0.192 |
| jemalloc | 42,705 | 41.704 |

These differences describe whole-process peak-to-end changes, not measured allocator reclaim bytes. RSMalloc has the lowest final idle PSS; jemalloc has the largest decrease from its higher workload peak.

## Sampling coverage and completion

| Allocator | Startup samples | Warmup samples | Workload samples | Idle samples | Total |
| --- | ---: | ---: | ---: | ---: | ---: |
| RSMalloc | 1 | 131 | 258 | 73 | 463 |
| System default | 1 | 132 | 261 | 73 | 467 |
| tcmalloc | 1 | 131 | 259 | 72 | 463 |
| mimalloc | 1 | 132 | 260 | 72 | 465 |
| jemalloc | 1 | 130 | 257 | 73 | 461 |

Startup is only one sampled point per run, not an exhaustive startup peak measurement. Samples can miss transient peaks; the requested 0.20 s interval does not include all sampling overhead. Each run has a final sysbench workload report and a clean MariaDB shutdown. Server logs contain no `[Warning]` or `[ERROR]` entries, but that does not negate the ignored errors reported by the client.

## How to interpret this snapshot

- Compare workload peak and final idle memory separately; idle phase maxima hide RSMalloc's later decrease.
- PSS and private/anonymous memory are more useful than virtual size for this comparison, but still include database memory unrelated to allocator overhead.
- Throughput differs by only about 1.25% between the highest and lowest reported TPS. Independent repeated runs are needed before claiming a reproducible advantage.
- The same configured table counts, durations, and threads do not imply identical SQL execution: sysbench seeds from the current time, and transaction/error totals differ.
- No CPU-time, hardware-counter, allocation-rate, allocator-only overhead, or p99 data is available in these reports. Do not infer a mechanism or tail-latency guarantee from the aggregate results.

See the [summary](mariadb_results.md) for normalized comparisons and charts, and the [harness documentation](mariadb/README.md) to repeat the experiment.
