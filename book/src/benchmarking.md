# Benchmarking Correctly

Measure application outcomes first and allocator mechanisms second. A realistic comparison includes correctness, throughput, latency, and memory behavior—not only calls per second.

## Match the integration and build

Use non-diagnostic release builds for timing. Compare the same application work, compiler settings, protection level, and access pattern. Record allocator version/commit and every Cargo feature. Repository `target-cpu=native` builds need compatible hardware.

For preload, compare the same executable with and without RSMalloc interposition, and confirm loading **and call routing**. For Rust global allocation, build an otherwise equivalent system-allocator variant. A debug or hardened result is a separate configuration, not interchangeable with default release data.

Use fresh processes for initialization-time settings and cold-start samples. Run separate diagnostic reproductions; counters, logging, probes, and extra threads can change contention and hide races.

## Test phases and traffic patterns

| Pattern | What it exposes |
| --- | --- |
| Cold process / first request | Bootstrap, initial mappings, metadata, and first touch. |
| Warm paired allocate/use/free | Cache-hit overhead and repeated reuse. |
| Growing live set | Refills, class footprint, residency, and region growth. |
| Cross-thread producer/consumer | Placement, transfer traffic, and cache imbalance. |
| Many CPUs exchanging one class | A concentrated shared-head contention case. |
| Burst, idle, then another burst | Retention, reclaim eligibility, faults on reuse, and latency recovery. |
| Large/aligned/zeroed buffers | Backend routing, padding, zeroing, rounding, and map/reuse policy. |
| Oversubscription and constrained deployment | Scheduler interaction, actual memory limits, and overload behavior. |

Touch data as the application does. Unused allocations can be optimized away or remain mostly nonresident. `black_box` can help inhibit optimization, but cannot make an artificial access pattern representative.

Make timing boundaries explicit. A malloc-only benchmark with cleanup outside the timer measures live-set growth without free cost. A free-only benchmark starts with prepared allocations. Neither is the inverse of a steady-state allocation/free workload, and either may omit operationally expensive setup or cleanup.

## Record enough context

Capture CPU topology and placement, thread counts, frequency/thermal conditions, NUMA policy, THP policy, background load, kernel/libc, build flags, revision, preload path, features, and tuning. Record any cgroup memory or CPU restrictions.

Affinity can make a controlled test repeatable while removing migration seen in production. Run both controlled and realistic scheduling where it matters. Measure peak live bytes as well as RSS: cached VA, requested payload, and resident memory are different quantities.

Useful metrics include wall time, operations/requests per second, latency percentiles, user/system CPU time, peak and post-idle RSS, major/minor faults, and page faults during the next burst. CPU seconds can exceed elapsed seconds in multithreaded work.

## Repeat and preserve distributions

Alternate or randomize allocator order and collect enough runs to observe variability. Keep per-run data, not just the best sample. Report median, range or another spread measure, and the workload duration/sample count. For latency, collect request-level distributions; a median of total runtimes is not p99 request latency.

A one-percent difference is not automatically meaningful because a benchmark executes many allocator calls. Repeated A/B runs, reversed ordering, adequate duration, and profiling are needed to distinguish it from scheduler or thermal effects.

## The repository's aggregation method

The current five-allocator comparison reports **column-wise medians of ten runs**. Each median averages the fifth and sixth sorted observations independently for elapsed time, RSS, CPU time, and faults. Fractional counts and extra digits reflect aggregation, not greater harness precision. The resulting row need not correspond to any one run.

RSMalloc's latest supplied log updates 20 workloads; its `z3` row retains the earlier ten-run median. Other allocator rows retain their existing datasets. Environment/version labels alone do not establish simultaneous runs or identical unrecorded build flags.

For each workload and metric, the relative-score method divides an allocator's result by the minimum across allocators. It then takes the geometric mean across the common set and multiplies by 100. Lower is better; 100 would match the best on every workload. Equal workload weighting is a presentation choice, **not your application's traffic mix**.

Best-value highlighting includes exact ties before chart rounding; clean winner counts exclude ties. A score, a win count, or a low RSS bar cannot alone establish the best deployment choice. See [workload examples](workload-fit.md#what-the-current-snapshot-actually-shows) and the repository's `benchmarks/` source tables.

## Profile the losing path

On Linux, these commands illustrate measurement and profiling; replace `your-program` with your workload:

```sh
/usr/bin/time -v your-program
perf stat -r 7 -- your-program
perf record -e cycles:u -- your-program
perf report --no-children
```

`perf` availability and event permissions depend on the host. Use profiles to distinguish local cache instructions, transfer/refill work, mapping/zeroing, and application computation. Fault and RSS evidence is needed for memory hypotheses; CPU samples alone do not prove them.

Disassembly can confirm that a code change removes instructions, but application measurements must confirm its impact. Preserve the original release result and a separate [diagnostic run](diagnostics.md). If a result regresses, use [troubleshooting](troubleshooting.md) instead of assuming the allocator design is always the cause.
