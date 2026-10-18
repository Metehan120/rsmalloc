# Troubleshooting

Diagnose before tuning. Keep the original failing artifact and change one variable at a time. RSMalloc is alpha-quality, not production-ready; switching back to a known-working allocator is a reasonable containment step while investigating, not proof of which component is at fault.

## Establish the evidence

For every reproduction, record:

- allocator version/commit, compiler, target CPU/profile, Cargo features, and matching binary/library and symbols;
- Rust global/native API or preload integration, effective allocator settings, and launch environment;
- kernel and libc versions, workload input, thread count, allowed CPUs/nodes, affinity, CPU quota, and memory limits;
- failing workload phase, elapsed time, throughput/latency distribution, and relevant memory/counter deltas.

For preload, inspect the actual workload process's mappings and loader/interposition behavior. A mapped `librsmalloc.so` is evidence of loading, not proof that every allocation source uses it. Static linking, another interposer, plugins, or a service manager's environment can change the result. For Rust, verify the selected global allocator and dependency features. See [LD_PRELOAD](./preload.md), [Rust Global Allocator](./rust-global-allocator.md), and [Security and Reliability](./security.md).

On Linux, `/proc/<pid>/status`, `maps`, `smaps` or `smaps_rollup`, `numa_maps`, and `cgroup` can supply process evidence; substitute the actual PID and account for permissions and process lifetime. Locate the process's real cgroup and controller mount before collecting its limit/usage/event files. Paths and available fields differ across cgroup versions and deployments. Use the profiler/debugger available on the target rather than assuming a tool or permission is present.

## Startup fails or RSEQ is unavailable

The supported target is Linux `x86_64` with libc-provided RSEQ TLS, such as glibc 2.35+ with working registration. RSMalloc reads libc's `__rseq_size` and `__rseq_offset` and aborts at bootstrap if either is zero. It does not register RSEQ itself or promise a working non-RSEQ mode.

1. Capture the exact loader error or allocator message. Unresolved libc symbols, `rseq unavailable`, and an ordinary critical-section abort are different failures.
2. Identify the libc actually loaded by this process, not just the host's package version. Check launch-time libc tunables, sandbox/seccomp policy, and whether kernel RSEQ support and registration are available in the failing environment.
3. Compare the same artifact in a supported environment, changing one environment component at a time. A libc version number alone does not prove usable per-thread RSEQ state.

Do not disable libc RSEQ registration as a workaround for this allocator. `abort-on-rseq-failure` adds an abort on sampled CPU ID `u32::MAX`; it does not enable missing support. Missing deployment prerequisites should block use of RSMalloc in that environment.

## The application is slower

1. Verify an optimized build with diagnostic features disabled. Review `target-cpu=native` when running repository-built artifacts on a different machine.
2. Confirm integration and compare identical workloads in alternating allocator A/B runs. Keep warm-up, memory-touch behavior, CPU restrictions, and THP policy comparable.
3. Profile the whole application, including page faults, syscalls, and scheduling where tooling permits. An allocator-symbol sample alone may miss first-touch costs paid later.
4. Use a separate [diagnostic run](./diagnostics.md) to distinguish cache hits, refills, transfers, large allocations, ownership checks, and reclaim activity.
5. Retain a change only if uninstrumented throughput, tail latency, and memory behavior improve for the deployment's priorities.

If successful local cache hits dominate and slow paths consume little time, refill tuning may have little leverage. Single-threaded or otherwise different workloads may favor another allocator's fast path. More cache capacity is not a general remedy.

## RSS is too high

First distinguish a live application working set from reusable allocator memory and from other process mappings. Collect a time series through allocation, free, idle, and reuse phases: RSS/mapping breakdowns, huge-page accounting, page faults, allocator per-CPU/transfer inventory, slab and segmented cached VA, trim activity, and cgroup usage/events.

**VA is not RSS.** Slab arenas and segmented regions can remain reserved after payload pages are reclaimed. Live objects cannot be reclaimed by allocator trimming; very small slab payloads do not contain complete reclaimable pages. Per-CPU retained blocks are not automatically released at thread exit. THP can increase residency for sparsely touched memory; investigate huge-page evidence before blaming reservation size.

Test the hypothesis supported by the data:

- If cached eligible memory persists, verify that periodic trimming is enabled, whether the worker started, cached-VA thresholds, and age eligibility. Lower thresholds permit scanning; they are not RSS targets or reclaim deadlines.
- If THP amplification is visible, compare `page-backend-no-huge-page` for slab arenas and the actual segmented/direct-mapping THP settings. Slab advice alone does not control every backend or guarantee a resident-memory limit. Measure TLB/fault and latency costs too.
- If oversized fresh batches or unused capacity dominate, test smaller refill/arena/region settings individually. Smaller arenas can increase mapping and ownership-metadata work; batch changes can increase refill frequency.
- If the process is near a cgroup limit, use that cgroup's evidence. Do not expect host-wide relief to detect it.

Do not make a series of unrelated changes until RSS drops; that loses the explanation and can trade memory for unacceptable latency.

### Trimming did not lower RSS

Background trimming is best-effort and age-based. The worker may not start until cached VA reaches a threshold; spawning can fail. Slab and segmented passes have separate schedules, and scans can be gated or contend on trim locks. Scheduled sleep intervals and lifetime estimates are not deadlines.

Manual trimming is also best-effort. V2 trim scans slab first, then segmented memory for the remaining target. Explicit segmented trimming bypasses age eligibility, but slab scans can still return early below cached-VA thresholds. An “all” request does not mean all mappings are unmapped or all free bytes are immediately resident-memory savings.

Correlate trim deltas with RSS and page state. Default reclamation uses `MADV_DONTNEED`; `lazy-page-trim` uses `MADV_FREE`, whose pages may stay resident until reclaimed by the kernel. Reuse and new faults can offset reclaim. Place controlled manual trim tests at an idle boundary, not on a request hot path.

### Memory-pressure relief did not prevent an OOM

Relief is disabled by default. In the current preload alpha, **`RS_ENABLE_RELIEF=0` enables it; nonzero disables it**. Set a valid decimal value before bootstrap. Rust uses `ReliefSettings`; changing preload variables after initialization does not reconfigure a Rust allocator.

The policy samples system-wide RAM-plus-swap usage from `sysinfo`, not `MemAvailable`, process RSS, or cgroup usage. Default thresholds disable segmented-bitmap allocation at 85% and permit re-enabling after two consecutive checks at or below 80%. Disabling the backend trims cached segmented blocks, but direct mappings remain a fallback. This changes allocation routing, not the application's live-memory requirement.

The background worker handles enabled relief even if periodic trimming is disabled, but detection depends on a running worker and scheduling. It cannot guarantee timely reaction to a burst or prevent allocation failure/OOM. A container can exhaust its memory limit while host pressure remains below the threshold. Check cgroup memory/swap limits, usage, pressure and OOM events independently, and provide application-level headroom or load shedding where needed.

## Refills are high

Identify affected size classes and workload phases. Compare refill counts and bytes with reusable transfer inventory, per-CPU occupancy, overflow, and placement of allocation/free traffic. A refill may reflect demand shifting between CPUs rather than inadequate total memory.

Investigate failed reuse before initializing larger fresh batches. The initial batch is a starting point for adaptive policy, not a permanent size. Larger batches can help some bursts but retain more memory and lengthen cold refills. Short-lived threads do not each start with their own empty slab cache: caches and predictor state are per CPU.

## RSEQ aborts are high

Compare interval abort counts with relevant allocation/free traffic and actual time spent retrying. Collect scheduler migrations, preemptions, signals, runnable-thread counts, effective affinity, and cgroup CPU throttling over the same interval. A large absolute count may still be a small part of total work; a high rate alone does not identify the cause.

Test realistic worker counts or stable affinity as controlled experiments, not universal fixes. Pinning can reduce migration while concentrating contention, restricting reuse, or worsening NUMA placement. Do not weaken CPU validation or RSEQ correctness checks to improve results.

## Transfer pushes dominate

Compare call counts with bytes, classes, current inventory, and pop/steal activity. Cache high-water overflow, producer/consumer separation, or a phase with many frees can produce many pushes without a leak. Do not attribute them to automatic per-thread cache teardown: these caches are per CPU.

Check whether consumers run on CPUs able to reuse the supplied classes and whether topology/search hints match demand. Increasing retention may reduce transfers but increase memory footprint. Validate any change with both latency and memory evidence.

### Scheduler and NUMA placement

Record the process's effective CPU/node restrictions and actual worker execution, not just the machine's socket count. Compare placement during allocation, first touch, and later consumption. Use `numa_maps` or available page-placement tooling to test a remote-memory hypothesis.

RSMalloc prefers local-node reuse and uses preferred `mbind` placement; it does not guarantee physical locality. Policy calls can fail or be restricted, pages can be placed elsewhere, and CPU migration can change the consumer's node. Cross-CPU stealing is not inherently cross-node access, and a topology report does not prove physical page placement. Compare placement changes under the real cpuset/quota rather than assuming pinning always helps.

## Lock retries are high

Use profile/stack evidence to identify the operation before tuning. Aggregate lock counters do not identify a particular lock. Possible paths include ownership metadata, arena/region growth, large-allocation metadata, and trimming. Ordinary local RSEQ cache operations avoid these locks, although usage accounting still uses atomics.

Distinguish lock call counts, retries, spin waits, and measured waiting time. Increasing unrelated caches may merely hide or shift the cost.

## Large allocations are slow

Separate segmented-bitmap reuse, region scans/growth, direct mappings, ownership updates, exact metadata operations, and later page faults/zero-fill. Routing uses adjusted size including metadata and alignment; nominal application size alone does not identify the backend.

Measure allocation and memory touching separately. Check whether relief disabled segmented allocation, whether the workload reuses compatible blocks, and whether THP/NUMA behavior explains later access cost. Raising region capacity only helps if reuse or reduced growth offsets greater reservation and retention.

## Latency spikes

Timestamp workload phases and correlate tail latency with refills, cold initialization, arena/region growth, direct mapping, page faults, trim activity, relief transitions, and CPU throttling. Average throughput can hide these events.

Manual initialization before serving can move bootstrap work out of requests, but it does not prefault future payloads or eliminate future growth. Compare warm-up with the real access pattern. Test periodic trimming changes only if correlated evidence implicates it; disabling periodic trims increases retention and does not disable enabled relief. Manual trim belongs at a controlled maintenance boundary and can make later reuse pay refault costs.

## Crashes or corruption reports

Preserve the original reproducer. Capture the signal or allocator error, fault address, failing instruction and stack, thread stacks when available, process mappings, kernel/libc, features/settings, integration mode, allocation size/alignment, and the pointer's allocation/reallocation/free history. Core dumps can contain secrets; review before sharing.

For magic mismatches, double-free reports, foreign-pointer failures, or recovered-aligned-base rejection, audit API ownership and lifetimes first: FFI/plugin boundaries, interior pointers, stale pointers, matching layouts, and buffer writes into metadata. Such reports can also expose an allocator bug; do not assume the caller is always at fault.

The default free path reads metadata before ownership validation and can fault before foreign-pointer fallback. A controlled `validate-foreign-first` or `extra-checks` reproduction can add evidence, but its radix checks are page-granular, not proof of a valid live allocation. Do not disable magic checks to keep a corrupt process running. Diagnostic/hardening changes can alter timing; preserve results from both builds.

### Unexplained SIGBUS

The README records a known observation of `SIGBUS` in some workloads on Linux **`7.0.10`**. This is not a diagnosis: the signal alone establishes neither allocator corruption nor a kernel defect.

Collect the signal details, faulting address/instruction, mapping containing that address, stacks, and relevant kernel logs where permitted. Check application file-backed mappings and their backing-file lifetime as well as allocator activity; not every mapping belongs to RSMalloc. Reproduce the same binary, settings, and workload on another kernel when practical, then compare other environment changes separately. Record whether the failure tracks the kernel or persists elsewhere, without claiming that either outcome alone proves causation.

Report a minimal reproducer with the evidence above. For suspected exploitable corruption, use the private reporting route in [Security and Reliability](./security.md); for ordinary correctness failures, use the project's issue channel.
