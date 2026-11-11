# Diagnostics and Statistics

Diagnostics identify the allocator paths a workload exercises; they do not establish that those paths dominate runtime. Counters, shared atomic updates, detailed scans, logging, and extra threads can change throughput, contention, and scheduling. Even an optimized instrumented build is not the performance baseline.

Use a reproducible uninstrumented release build for timing, and a separate diagnostic run of the same workload for explanation. Record the commit, feature set, configuration, integration mode, kernel/libc, CPU placement, and workload phase. See [Benchmarking Correctly](./benchmarking.md) and [Troubleshooting](./troubleshooting.md).

## Feature tiers

These are feature dependency groups, **not one cumulative ladder**. Enable only the evidence you need; no diagnostic feature has a fixed or universally negligible overhead.

| Feature | What it enables | Operational cost or limitation |
| --- | --- | --- |
| `debug` | Base RSEQ, refill, mapping, and reclaim counters; structured Rust statistics. | Counter updates and bookkeeping; not total allocation/free call counting. |
| `debug-print` | `debug` plus an exit-time report. | Report collection, formatting, and output. Abnormal termination may bypass the exit hook. |
| `debug-printer-thread` | `debug-print` plus live snapshots. | A background thread and recurring collection/output that can perturb the workload. |
| `debug-preload` | `preload` plus `debug-print`. | Convenience combination for preload diagnosis. |
| `debug-exact` | `debug-print`, lock calls/retries/spin waits, and detailed transfer-byte/reclaim accounting. | Additional instrumentation; “exact” does not mean a consistent process-wide snapshot. |
| `transfer-debug` | `debug-exact` plus transfer steals, dry steals, and CAS retries. | Shared transfer-path counter updates. |
| `transfer-debug-exact` | `transfer-debug` plus transfer push/pop call counts. | Additional call accounting. |
| `debug-predictor-exact` | `debug-exact` plus intrusive refill over/under-prediction accounting. | Changes refill observation and work; use only to investigate prediction. |
| `predictor-debug` | Refill logging, independent of the report/counter groups. | Potentially high-volume I/O; can strongly distort refill timing. |
| `debug-full` | Convenience bundle through `transfer-debug-exact`. | Broad counter instrumentation, without exact predictor diagnosis. |
| `debug-full-critic` | `debug-full` plus `debug-predictor-exact`; total internal allocation/free call counters. | Broadest counter bundle; particularly unsuitable for performance comparisons. |

A practical progression is base reports first, transfer or lock detail second, and exact predictor diagnosis only when refills warrant investigation. With `confidence-predictor`, predictor logs include `is_low` captured before feedback. The bulk log label `mmap` means the bulk-refill path, **not necessarily a mapping syscall**.

For preload, select these features when building the library as described in [LD_PRELOAD](./preload.md); `debug-preload` is the base reporting combination. For Rust, enable features on the allocator dependency. Keep symbols matched to each artifact. If comparing artifacts, verify that the intended library is loaded and that diagnostic features did not remain enabled in the timing build.

The live printer reads `RS_PRINTER_SLEEP` as milliseconds, defaulting to `1000` when absent or unparsable. Set it before the printer starts; avoid zero or excessively frequent polling. Lack of live output does not prove there is no allocator activity: check the feature set, output capture, and whether the printer thread started.

## Reading the report

Use interval deltas from comparable workload phases where possible. Exit totals combine initialization, steady state, idle periods, and teardown; final cache state is not peak memory. Reports sample counters and structures separately while the allocator may still be changing. Values are not a transactional snapshot, and policy counters and hints may be approximate.

A missing counter, or one not instrumented in the selected build, must not be interpreted as zero events. Total allocation/free counts require `debug-full-critic`; if using application counts instead, document which operations they cover.

### RSEQ aborts

RSEQ retries are expected when a protected operation is interrupted. Compare abort deltas with relevant allocator traffic and with migration, preemption, signals, and CPU scheduling evidence over the same interval. A raw lifetime abort total is not an abort rate, and aborts alone do not identify the cause or the time lost. Distinguish these retries from an RSEQ-unavailable startup failure.

### Refills

Read per-class refill counts before tuning batch policy. Bursty demand, depleted current-CPU caches, uneven allocation/free placement, and ineffective transfer reuse can all produce refills. Thread churn matters through changes in CPU demand, not because each thread owns a private slab cache.

If reusable transfer inventory exists, investigate its distribution and consumption before increasing fresh-block initialization. Low absolute refill counts limit the potential benefit of predictor changes even when the reported miss percentage is high.

### Predictor misses

Under-prediction can require further refills; over-prediction can initialize or retain blocks that are not soon consumed. Read these classifications with the selected predictor and instrumentation features, absolute refill counts, class sizes, and retained bytes. A miss percentage is neither a cache-hit rate nor a measured runtime penalty. More aggressive batching can exchange fewer refill events for larger individual latency spikes and memory retention.

### Transfer cache

- Push/pop call counts and byte counts answer different questions. Compare both by class when available.
- Steals show cross-CPU reuse; dry steals show unsuccessful searches, not necessarily a correctness problem.
- CAS retries suggest contested list updates, but do not directly measure time waiting.
- Many pushes with few pops may reflect producer/consumer placement, workload phase, or excess retention. Correlate with current inventory rather than assuming a leak.

These caches are per CPU. Do not assume thread exit empties them or that final totals represent steady-state traffic. For suspected cross-node costs, combine allocator topology and transfer evidence with OS page-placement data; a steal count alone does not establish remote physical memory access.

### Locks

Calls, retries, failed try-locks, and spin waits are distinct. Calls alone do not prove contention; retries alone do not measure latency. The report's aggregate counters do not identify every contended lock. Use stack/profile evidence to distinguish metadata, backend growth, and trimming activity before tuning. Ordinary local RSEQ cache operations avoid those locks but still perform atomic usage accounting.

### Per-CPU cache distribution

Minimum, maximum, and per-class cached bytes describe sampled distribution. Scheduler placement, CPU restrictions, asymmetric producers/consumers, and earlier workload phases can leave unequal inventory. Investigate imbalance when it coincides with repeated refill, overflow, or expensive reuse—not solely because some CPUs retain more than others. Allowed CPUs and available NUMA nodes may differ from the machine topology.

### mmap and cached virtual memory

Mapping requests, cached VA, live application payload, RSS, and cgroup memory charges are different measurements. Reserved arenas can be untouched or reclaimed while remaining mapped; metadata, alignment, THP, stacks, and other application mappings also affect process memory.

Compare allocator snapshots with process mapping/RSS breakdowns, huge-page accounting, page faults, and the applicable cgroup's usage and events. Trimmed-byte totals report allocator reclamation activity, not a guaranteed equal RSS decrease. `lazy-page-trim` uses `MADV_FREE`, so resident pages can persist until kernel reclaim. High-water cached VA is not peak RSS.

## Structured Rust statistics

For `rsmalloc::v2::alloc::RSMalloc`, `get_stats()` is available with `debug`; `get_exact_stats()` requires `debug-exact`, with further fields gated by transfer and full-critic features. These methods report shared allocator state, not memory owned by an individual instance.

`get_stats()` initializes the allocator if needed, collects backend/radix reports, allocates report storage, and scans CPUs and classes. Collection is not free or side-effect-free. Keep it out of request hot paths, avoid aggressive polling, and do not expect it to be an atomic snapshot. Use it for controlled phase boundaries or low-frequency application reporting, then confirm performance conclusions in the uninstrumented build.
