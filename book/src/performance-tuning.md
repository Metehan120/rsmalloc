# Performance Tuning

Tune an observed bottleneck, not a presumed one. Every experiment needs a default baseline, a target metric, a regression budget, and a fresh process so initialization-time settings actually take effect.

Rust builds use `Config::new(Tuning::DEFAULT...)`; [preload builds](preload.md#runtime-configuration-set-it-before-bootstrap) use environment variables. Configuration is process-wide and first-initializer-wins. Neither multiple allocator instances nor changing environment variables after bootstrap provides independent heaps or live retuning.

## Baseline and default values

| Setting | Default | What it is not |
| --- | --- | --- |
| Initial transfer prediction | 128 blocks, subject to class limits | A fixed refill size for every class. |
| Refill retries | 3 | A cache-hit acceleration or universal latency bound. |
| Minimum slab arena setting | 256 MiB | Precommitted RSS or a process memory quota. |
| Initial segmented-bitmap region | 64 MiB | A hard total cache limit. |
| Small / bitmap trim thresholds | 10 MiB / 512 MiB cached VA | RSS thresholds or per-object release deadlines. |
| General THP / forced bitmap THP | Enabled / disabled | Guaranteed kernel huge-page placement. |
| Pressure relief | Disabled; 85% disable / 80% re-enable thresholds | Cgroup-aware OOM prevention. |

Measure throughput, latency, CPU time, peak RSS, idle RSS, and faults after reuse together. Diagnostics help explain a result but must not be part of the final timing configuration.

## Refill prediction and retries

`with_refill_init_batch` changes the initial transfer prediction. Initialized-block reuse and fresh-block bulk initialization have separate per-CPU/class predictors. A larger initial prediction may amortize a burst but can move too much inventory to one CPU; a smaller one may reduce retention while increasing refill/exchange frequency.

Evaluate absolute refill counts and their cost, not just a predictor miss percentage. A large miss percentage across a handful of refills can be irrelevant to millions of cache hits. Workers share CPU-indexed state; a newly created thread does not automatically start a private cold predictor.

`with_max_refill_retries` controls retry policy used by refill/backing reservations. More attempts may help transient failures, but cost slow-path work and cannot repair unsupported RSEQ or guarantee allocation success. It does not remove ordinary cache-hit overhead.

`confidence-predictor` and `experimental-predictive-demand` are opt-in policies. Test each independently before combining them, including their memory and latency effects. Demand feedback runs at refill time, not every cache hit; enabled builds also carry additional per-CPU state. Neither is part of `extra-checks`.

## Arena and region sizing

`ArenaSize::new` accepts zero and multiples of **4 KiB**; bootstrap enforces a **256 KiB minimum**. The default remains 256 MiB. A reservation larger than the minimum can require a larger arena.

Larger arenas can reduce growth/mapping frequency while retaining more VA and potentially increasing RSS under sparse access and THP. Smaller arenas can increase metadata and mapping work. Compare the workload's touched bytes and mapping activity, not just the configured size.

Despite its name, `PerCacheLimit` sets the **initial segmented-bitmap region size**. Later growth adds 64 MiB regions. Raising it is not a way to set a larger process-wide memory budget, and lowering it is not a hard retention cap. Only consider increasing it when measured large-buffer reuse justifies the reservation.

## Transparent huge pages

THP can reduce TLB pressure for dense, hot data but inflate residency for sparse arenas. Kernel policy, page touching, and trimming matter alongside allocator advice.

- Compare default behavior with `page-backend-no-huge-page` when sparse slab residency is a problem.
- `page-backend-huge-page` requests slab huge pages. Select only one page-backend advice feature: enabling both suppresses both advice calls rather than giving no-huge-page advice precedence.
- Segmented-bitmap forcing is separate from general THP support. Test it with actual large-buffer access patterns, not untouched reservations.
- Track faults and latency after trim as well as steady-state throughput and RSS.

These are advice policies, not guaranteed physical page sizes. Do not assume the lowest RSS setting also has the best latency.

## Trimming: eligibility before thresholds

Background trimming uses cached-VA thresholds and lifetime/age heuristics. Lower thresholds can permit work sooner but do not guarantee an RSS target or immediate reclamation. Both subsystems share a low-cache gate: scans skip when **both** cached totals are below their thresholds; their scheduling intervals remain separate.

Slab trimming requires complete reclaimable payload pages. Tiny-class retention cannot be solved simply by lowering the threshold. Bitmap pages must be free and eligible; live objects cannot be reclaimed. Virtual arenas may remain reserved after payload pages are advised away.

`trim-aggressively` tightens idle-class eligibility. `lazy-page-trim` uses `MADV_FREE` rather than immediate discard for both slab and segmented-bitmap payload pages, including explicit and relief-triggered bitmap trimming. It can preserve reuse but delay RSS reduction until kernel reclaim. Benchmark burst/idle/burst sequences to expose the tradeoff.

Manual core trimming can be appropriate between phases; see [native/core APIs](native-api.md#core-operations-initialization-and-best-effort-trim). `All` is an unbounded best-effort pass, not proof that all freed memory was returned. Bootstrap sets the effective big trim threshold to `max(requested, initial_cached_bytes + 1)`, not a fixed 128 MiB rejection floor. The footprint includes the normalized initial region, page-rounded region metadata, and page-rounded state/node/hint tables. Footprint or `+ 1` overflow logs `RSMallocError::Bootstrap` and aborts. See `src/backend/bootstrap.rs` — `PreparedBootstrapConfig::big_trim_threshold`; `src/big_allocations/segmented_bitmap.rs` — `initial_cached_bytes`.

Disabling periodic background trimming does not disable manual trimming or all worker activity: enabled relief can still need the worker. Only try disabling it when retained memory is acceptable and measured background interference matters. Avoid trimming on every request.

## Memory-pressure relief

Relief uses system-wide RAM-plus-swap usage from `sysinfo`. It can disable segmented-bitmap allocation under pressure, advise cached bitmap pages away, and later re-enable allocation after qualifying lower-pressure checks. Direct mappings remain a fallback; disabling bitmap reuse does not prohibit all large allocations.

It is not based on a container's budget or a `MemAvailable` estimate. Do not rely on it to prevent cgroup OOM. In preload builds **zero enables `RS_ENABLE_RELIEF`**, while nonzero disables it. Use valid decimal values: malformed text can parse as zero.

## A disciplined experiment

1. Reproduce an application regression with defaults and a baseline allocator.
2. Use a separate diagnostic run to classify it: cache-hit cost, refill, transfer contention, mappings, page faults, or trimming.
3. Change one relevant setting or feature. Restart the process and record the full configuration.
4. Repeat cold, sustained, peak, and idle/reuse phases at realistic CPU placement and concurrency.
5. Keep a change only if the target improvement exceeds variability and no correctness, memory, or latency budget regresses.

Batch changes cannot fix an instruction-bound cache-hit loop. Trim changes cannot reclaim live objects or manufacture page-granular reclaimability. Security weakening is not normal tuning. When those boundaries matter, revisit [workload fit](workload-fit.md) rather than piling on settings.
