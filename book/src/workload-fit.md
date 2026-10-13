# Workload Fit and Failure Modes

RSMalloc is worth evaluating when CPU-local reuse and movement of free blocks between CPUs address a real application cost. It can fall behind when the overhead of that machinery dominates, or when retained memory and slow paths violate resource budgets.

The guidance below is an **architectural hypothesis to test**, not a prediction of a particular application's speed. Alpha reliability and platform compatibility are independent acceptance criteria.

## Where the design can help

| Workload shape | Why it may suit RSMalloc | What to verify |
| --- | --- | --- |
| Sustained concurrent allocation with repeated class reuse | Warm CPU caches amortize refill; batches reuse initialized blocks. | End-to-end throughput, tail latency, RSS, and refill frequency. |
| Worker turnover or thread pools with changing demand | Cached blocks are not tied to a departed thread's private heap. | Startup cost and cache warming on the CPUs actually used. |
| Mixed allocation/free ownership across workers | Freed blocks can be reused by the current CPU or exchanged without permanent thread ownership. | Transfer contention and imbalance; cross-thread traffic can also be a worst case. |
| Repeated large buffers within bitmap orders | Reuse can avoid repeated map/unmap work; some growth can occur in place. | Rounding overhead, metadata locks, region growth, and reuse distance. |
| Node-local work with stable placement | Refill and reuse prefer the local NUMA node. | Real physical placement and remote access; preference is not a guarantee. |

An application may benefit even if a tight allocator microbenchmark loses. Conversely, fast cache hits do not guarantee good request latency when the application frequently reaches refills, mappings, or trimming.

## Where it can fall apart

### Extremely cheap, repetitive allocation loops

A loop whose only work is allocation/free exposes every extra instruction: header handling, dependent list loads, and locked usage updates. Shared reuse offers little compensation if one CPU continually recycles the same small objects.

`alloc-testN` performs roughly 600 million allocations and 600 million frees. A few extra instructions repeated about 1.2 billion times matter. Increasing refill batches cannot remove cache-hit instruction costs.

### Concentrated cross-CPU exchange

“Cross-thread friendly” does not mean “all cross-thread workloads win.” Many CPUs circulating one size class can drive overflow, batch extraction, and repeated CAS attempts on transfer heads. Linked-list dependency chains and scheduler behavior can amplify contention.

Test producer/consumer placement, thread counts below and above CPU count, and realistic migration. Pinning may improve repeatability but hide the deployment's behavior. If transfer work dominates, compare a different allocator architecture rather than assuming more batching is the answer.

### Sparse, bursty, or memory-constrained workloads

Caches and regions favor future reuse, not immediate release after every free. Sparse arenas, class rounding, strong alignment, and THP can amplify RSS. Per-CPU state also means hardware topology matters, not only the current application thread count.

Tiny allocations do not each contain complete reclaimable payload pages. Lowering a trim threshold cannot manufacture reclaimable pages in those classes. A large idle RSS therefore needs class/access-pattern investigation, not only a trim call.

Under a container limit, host-wide pressure relief may not react before the cgroup runs out of memory. The initial bitmap-region setting is **not a memory cap**. Maintain headroom and test peak live memory, post-burst residency, faults on reuse, and actual OOM behavior.

### Cold starts and latency-sensitive services

Bootstrap, ownership metadata, new page arenas, first touch, zeroing, and region growth cost more than warmed cache hits. A short-lived process may never amortize them. In a long-running service, rare slow paths can dominate p99/p999 even when mean throughput improves.

Separate cold and warm measurements. `manual_init` can move bootstrap work but does not prepopulate every class, ensure residency, or eliminate future slow paths. Aggressive trimming can trade idle RSS for faults and latency during the next burst.

### Unsupported deployment or invalid ownership

Unsupported RSEQ/libc, incompatible CPU builds, static binaries that cannot be interposed, allocator mixing, and invalid pointers are compatibility/correctness failures—not tuning problems. Fork handlers do not authorize arbitrary allocation after multithreaded fork before exec. Read [security](security.md) and [preload boundaries](preload.md#requirements-and-boundaries) before diagnosing a crash as a performance issue.

## What the current snapshot actually shows

The repository's development snapshot uses column-wise medians of ten runs on a Ryzen 5 5600X with CachyOS kernel `7.2.6-1-cachyos-bore`. The latest RSMalloc update covers 20 workloads; `z3` retains an earlier ten-run median. Competitor datasets were not recollected with that update. Treat this as a workload profile, not a controlled claim that all measurements were simultaneous.

Selected elapsed-time medians, in seconds:

| Workload | RSMalloc alpha-3 | Comparison | Lesson |
| --- | ---: | --- | --- |
| `lua` | 0.985 | glibc 1.01, mimalloc 1.02 | A small reported lead needs repeated-run uncertainty, not a universal claim. |
| `rocksdb` | 3.22 | mimalloc 3.24, glibc 3.35 | An application-like benchmark can be competitive; this does not characterize every database workload. |
| `alloc-testN` | 3.09 | mimalloc 2.69, glibc 3.07 | Hot-path overhead is amplified by repeated operations. |
| `sh6benchN` | 0.30 | mimalloc 0.18, glibc 3.015 | Stress-case results differ sharply by allocator; RSMalloc also has about 361 MiB RSS here. |
| `sh8benchN` | 1.575 | mimalloc 0.44, glibc 12.14 | A win against one allocator can coexist with a large loss against another. |
| `xmalloc-testN` | 0.718 | mimalloc 0.216, tcmalloc 13.3245 | This is a RSMalloc weakness against small conventional fast paths, not a universal ordering. |

These numbers are illustrative and may become stale as the allocator changes. Current source measurements and full comparisons live in the repository's `benchmarks/benchmark_overall.txt`, `benchmarks/benchmarks.md`, and `benchmarks/benchmarks_detailed.md`. Column medians need not describe one actual run. Synthetic names alone do not establish the exact bottleneck: profile it.

## Decide using acceptance criteria

1. **Compatibility:** supported environment, correct ownership, and successful application tests.
2. **Performance:** required throughput and latency at normal and overload concurrency, including cold starts.
3. **Memory:** peak and post-idle RSS within the real limit, with acceptable faults and latency on reuse.
4. **Operations:** reproducible builds, diagnostic visibility, and an easy baseline/rollback path.

If defaults fail, use [diagnostics](diagnostics.md) to isolate the path before [tuning](performance-tuning.md). If the underlying workload mismatch remains, keeping the system allocator or another allocator is a valid outcome. Do not weaken pointer protection to hide correctness failures or call a regression a success because one score improved.
