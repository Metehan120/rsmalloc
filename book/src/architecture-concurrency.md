# Concurrency, Invariants, and Observation Limits

RSMalloc combines several synchronization mechanisms. No single phrase such as “RSEQ-based,” “per-CPU,” or “lock-free transfer cache” describes the correctness of the whole allocator. This chapter collects the boundaries needed to read profiles, review changes, and understand current alpha limitations.

## Which state is authoritative?

| Structure | Ownership transition or protection | What does not follow from it |
| --- | --- | --- |
| CPU-local slab list | CPU validation and final head-store RSEQ commit | Abort does not undo arbitrary preceding writes; usage is not updated in the same commit. |
| Transfer list head | Pointer-plus-generation CAS | Generation tagging does not by itself protect pre-CAS raw link traversal from concurrent rewrite. |
| CPU pending extent slot | Claim by removing its pointer; return by release publication | A predictor or sampled usage count is not permission to mutate another claimant's extent. |
| Pending overflow queue | Tagged atomic head publication/removal | Queue storage availability is not guaranteed after initialization failure. |
| Page-arena cursor | CAS reserves a unique bump range | Growth/search still uses node locks; a reservation need not be resident. |
| Bitmap segment state | CAS claims aligned slot masks | A free-run hint or report is not an allocation claim. |
| Bitmap reclamation | Claim free slots as occupied before advice, then release | Sampled used bytes can include trim reservations rather than application objects. |
| Ownership radix | Atomic region bits, synchronized node creation | A marked 4 KiB region does not validate an individual pointer boundary or lifetime. |
| Exact large metadata | Sharded lock-protected pointer map | Large allocation/free/realloc are not globally lock-free. |
| Bootstrap state | Shared once-election and completion publication | Multiple frontend instances are not isolated heaps or live configuration updates. |

Source: `src/rseq_core/slab_cache/rseq_asm.rs`;
`src/rseq_core/{bulk_fill,pending_queue}.rs`;
`src/rseq_core/slab_cache/{transfer,reclaim}.rs`;
`src/backend/page_allocator.rs`;
`src/big_allocations/segmented_bitmap/segment.rs`;
`src/internals/{radix_tree,big_meta_map,once}.rs`.

## Publication, accounting, and policy are separate

List heads determine which objects a successful operation removes or publishes. Counters are updated separately. A producer may publish a list head before incrementing inventory; another CPU may remove that head before the increment occurs. Usage updates also follow RSEQ commit.

Therefore observations can disagree without establishing object duplication. Conversely, agreement of counters does not prove pointer correctness. Transfer trimming introduces an additional issue: it subtracts a sampled inventory count after detaching the entire chain, so delayed producer accounting can cause **persistent drift after quiescence**. The exact chain length is not reconciled by that sampled subtraction.

Source: `src/rseq_core/slab_cache/transfer.rs` — publication/removal;
`src/rseq_core/slab_cache/reclaim.rs` — `SlabCache::trim_small`.

The following state is policy/advisory rather than an ownership authority:

- nonempty and being-stolen bits;
- separately maintained transfer inventory and cached-VA totals;
- current sampled CPU-cache usage;
- transfer/bulk predictor values and expected-demand samples;
- lifetime estimates and trim schedules;
- preferred segments, topology ranges, and scan starting points.

A stale hint can trigger a futile search or skip useful reuse. A dropped predictor update can change batching. Neither must be treated as evidence that a block exists, that a reader owns it, or that it is safe to free an address.

## ABA protection is one part of a proof

Transfer and pending heads preserve the pointer in 64 bits and carry a wrapping 64-bit generation in the other half of `AtomicU128`. Changing a head advances the generation so a stale equal pointer does not ordinarily satisfy the old head comparison. This is not infinite generation space, hazard-pointer protection, or automatic safety for every non-atomic field read.

In particular, transfer pop traverses ordinary `Header.next` links before its head CAS. The slab trimmer checks/sets a flag and detaches the chain, but does not drain all readers admitted before that flag. It then rewrites links while partitioning. A losing CAS rejects stale head removal, **not the preceding raw reads**.

This is a current source-level reader/rewrite race concern, not a demonstrated claim that a particular run necessarily reclaims a live application object. Describe detached ownership as the intended slab protocol, not a completed proof of reader exclusion. The bitmap trimmer's occupied-slot claim is a different mechanism and should not be used to justify slab safety.

Read [transfer exclusion limits](architecture-transfer.md) and [reclamation](architecture-reclamation.md). Documentation explains this boundary; it does not repair the implementation or turn the alpha into an audited allocator.

## Slow paths and progress

A cache hit avoids a general cache mutex, but successful local slab operations still execute locked usage accounting. Transfer exchange can retry tagged CAS and walk dependent pointers. Node growth, metadata, initialization, trimming, and preload fork coordination use locks or spin-based primitives.

Single RSEQ push/pop wrappers retry failures; a batch push has bounded RSEQ attempts before transfer fallback. A bounded batch retry is not a bound on overall allocation latency: refill, mapping, first touch, growth, and scheduling remain possible. Predictor updates use one weak relaxed CAS and can be dropped because their state is advisory.

Pressure relief and trimming are scheduled best-effort work, not real-time deadlines or a memory quota. Unsupported libc RSEQ is a bootstrap failure, not another supported cache mode. Allocation exhaustion can return failure from some paths and abort in mandatory metadata paths.

Source: `src/rseq_core/slab_cache.rs`; `src/core_prim/predictor.rs`;
`src/backend/{bootstrap,background_thread,page_allocator}.rs`;
`src/internals/{lock,fork_lock,radix_tree}.rs`.

## Fork, loader, and process lifecycle

Preload handlers coordinate bootstrap and selected backend lock families. Fork-aware reservations allow the forking thread's callback work to temporarily access guarded data while preserving exclusion. Parent and child release inherited reservation state; fallback symbol caches are reset in the child.

These mechanisms cover the specified families, not every application lock or allocation lifecycle. They do not relax POSIX restrictions on allocation between multithreaded fork and exec, guarantee async-signal safety, or support recursive fork/callback waits on allocator-blocked threads. Rust non-preload locks do not use that preload fork protocol.

Source: `src/core_prim/fork.rs`; `src/internals/fork_lock.rs`;
`src/inner/fallback.rs`. Details are in [ownership and fork behavior](architecture-ownership.md).

## Telemetry observes—and perturbs—the system

Structured stats, exit reports, and live printers do not produce a coherent transaction across the allocator. Buffer construction, string formatting, and arena reporting can themselves allocate or lock. CPU and per-class summaries can sample different moments. Per-class min/average calculations over nonempty CPUs are not interchangeable with minima over all configured CPUs.

Some field names need additional caution:

| Reported metric | Current interpretation |
| --- | --- |
| `rseq_aborts` | Every failed single push is counted; batch failures count once after all attempts fail; pop retries are not counted. Not all RSEQ failures. |
| mmap calls/requested bytes | Instrumentation recording sites, not necessarily every syscall attempt, successful mapping, or outstanding bytes. |
| `trim_calls` | Scans admitted past their gates/locks, not all public trim requests or successful passes. |
| `trimmed_blocks_small` | Candidate accounting can include budget-skipped or unsuccessful release candidates. Not successful reclaimed-object count. |
| `avg_madvise_cycles_small` | Release-attempt timing divided by candidate count; not a measured mean of actual successful syscalls. |
| Bitmap free blocks by order | Greedy disjoint decomposition of free slots, not all possible candidates at every order. |
| Bitmap reused/trimmed classifications | Whole free-run classification from dirty/history planes, not historical events or exact dirty/reclaimed slot byte totals. |
| Bitmap used bytes | Occupied-slot observation can include temporary trim claims. Not exact application live payload. |
| Cached-VA/high-water counters | Allocator capacity observations; not RSS or process/cgroup peak memory. |

Source: `src/frontend/global_alloc2/debug.rs`;
`src/frontend/global_alloc.rs`; `src/debug_exit_printer.rs`;
`src/debug_printer_thread.rs`; `src/global_vals.rs`;
`src/big_allocations/segmented_bitmap.rs` — reporting;
`src/rseq_core/slab_cache/reclaim.rs` — candidate accounting.

Keep diagnostics out of final allocator comparisons. Use operating-system and application measurements to test hypotheses raised by counters. A metric called “exact” can count instrumented events exactly while still having narrower semantics than its name suggests—or sample independently changing state.

## Review checklist for implementation changes

1. Identify the allocation-domain owner and the exact claim/publication transition.
2. Keep initialization before publication and preserve readers' required ordering.
3. For RSEQ, place the commit at the final shared head store and keep non-idempotent accounting outside the abortable interval.
4. Distinguish head CAS correctness from the lifetime and synchronization of links read before CAS.
5. Never promote hints, inventory, or predictor state into ownership or boundary validation.
6. Preserve aligned-base validation in ownership-first free mode; metadata-first mode instead requires valid allocator-owned inputs.
7. Handle or explicitly account for failed metadata/queue publication; do not assume every OOM returns to the caller.
8. Describe trim candidate eligibility, advice exclusion, and reporting semantics separately.
9. Preserve frontend-specific layout, zero-size, resize, and failure contracts.
10. Validate uninstrumented application latency and memory, not only an instruction count or instrumented counter.

This checklist is a guide to current boundaries, not evidence that every current path satisfies a complete correctness proof. RSMalloc remains alpha-quality. The [user security chapter](security.md) defines the usage obligations; this section explains why implementation details cannot replace them.
