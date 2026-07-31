# Updates

## v0.2.0-alpha

v0.2.0-alpha is a full allocator overhaul relative to the `main` branch's `0.1.0-alpha` line. It reworks the RSEQ/slab path, transfer-cache reuse, refill metadata ownership, NUMA placement, buddy backend, trimming and relief, preload ABI behavior, debug reporting, benchmarks, and the public Rust configuration surface.

Major themes are: fewer mapping/VMA slow paths, NUMA-aware locality, lower refill overhead, bounded cache pressure, stronger diagnostics, and making alpha debug modes explicit enough that benchmark/debug behavior is not confused with normal allocator behavior.

### Release layout and breaking alpha changes

- Bumped the crate to `0.2.0-alpha` and updated Cargo metadata/features for the new allocator architecture.
- Removed old feature paths that no longer match the allocator design, including `canary`, `legacy-glibc-support`, `cpu-refill-paths`, and `disable-thread-pending`.
- Removed the allocator canary feature and decoupled `extended-header` from canary-specific metadata/checking.
- Removed the old RSEQ thread-failure fallback path. Current `0.2.0-alpha` treats working per-thread RSEQ state as an allocator invariant instead of silently redirecting invalid CPU IDs through a fallback slot.
- Renamed major internals around the new architecture:
  - `RSEQ_CACHE` -> `SLAB_CACHE`
  - `RseqInner` -> `SlabCacheInner`
  - `SelfMail` -> `TransferCache`
  - `mail_*` APIs -> `transfer_*`
  - `BIG_BUDDY_ALLOCATOR` -> `BUDDY_BACKEND`
  - `L3_RADIX` -> `RADIX`
  - big-allocation metadata naming toward `BIG_META_MAP` / `BIG_MAP`
- Added `syscalls` and expanded `rustix` feature usage for NUMA binding, system memory-pressure sampling, thread CPU lookup, filesystem/sysfs topology parsing, and memory advice paths.

### Slab/RSEQ allocation path

- Kept `try_pop_single(...)` as a direct current-CPU transfer-cache probe after repeated RSEQ pop aborts.
- Renamed `RSEQ_MAX_BLOCKS` to `CACHE_HIGH_BLOCKS` and split cache byte targets into `SMALL_CLASS_BYTES`, `MEDIUM_CLASS_BYTES`, and `BIG_CLASS_BYTES`.
- Added `CACHE_LOW_BLOCKS` as a derived half-watermark table for future cache pressure policy.
- Added global `NCPU` initialized during `SLAB_CACHE.ensure_cache()` and reused by trim/debug paths.
- Added `get_size_4096_class()` and cached lookup support for selecting trim-eligible size classes.
- Updated larger slab/cache pressure behavior so oversized batches spill through transfer caches instead of overfilling CPU-local class caches.

### Transfer cache balancing

- Added NUMA-aware transfer-cache victim stealing for batch `try_pop(...)`: local CPU transfer cache first, same-node CPU ranges next, then remote node ranges when NUMA is active.
- Added relaxed per-class transfer-cache nonempty CPU hints, stored as CPU-word bitmaps, so batch stealing can skip ranges that have no nonempty hint for the requested class.
- Updated hint maintenance to mark a class/CPU bit only on transfer-cache empty-to-nonempty transitions, and to clear/recheck the hint when a pop observes an empty transfer list.
- Kept the hint bitmap deliberately approximate: correctness remains in the ABA-tagged transfer-list CAS path.
- Expanded the transfer-list head generation to eight bits in the unused high byte of the allocator's 56-bit user-address representation. The pointer and generation remain in one 64-bit atomic word, preserving the existing CAS width and low pointer bits.
- Each successful update advances the generation, rejecting a stale CAS across 1–255 intervening successful updates to the same CPU/class shard. The packed value can repeat after 256 updates; alpha-2 explicitly treats this as a bounded-tag limitation rather than claiming an unbounded ABA proof.
- CPU/class sharding keeps ordinary mutations local. Reaching the wrap schedule requires exceptional cross-CPU activity such as stealing or trim handling, plus reuse of the same head pointer with a changed successor while an operation is stalled. This substantially reduces practical exposure without making the schedule formally impossible.
- Added a cold `trimmed` transfer-cache list beside the normal transfer list. Normal transfer blocks are preferred; successfully madvised small blocks are returned through the trimmed list as a cold fallback. Transfer class hints deliberately prioritize the hot normal list and remain approximate.

### Hybrid slab page backend and pending metadata

- Added `src/backend/page_allocator.rs`, a hybrid page backend for bulk-fill/refill memory so small allocation refill spans are served from larger NUMA-preferred arenas instead of direct per-refill mappings.
- The page backend allocation order is bump allocation from the current node arena first, bitmap-tracked reusable page runs second, and mapping a new arena only when needed.
- Page-backend arenas store `PageArena` metadata and a bitmap at the front of the mapping, then expose a page-aligned data region for refill spans.
- Set the default minimum page-backend arena data size to 256 MiB and made it configurable through Rust `RSMallocConfig::arena_min_size` or the preload `RS_ARENA_SIZE` byte value. Arena creation still grows beyond the minimum when required by a refill request.
- Bump allocations also mark bitmap bits, so future bitmap reuse and release logic share one page-run ownership model.
- Batched contiguous page-backend bitmap marking and clearing by 64-bit word. Full words now use one store, while leading and trailing partial words use masks that preserve neighboring page ownership bits.
- Added `PAGE_ALLOCATOR.release(...)` as future span-reclaim scaffolding. It is not part of normal slab free yet; safe use still requires future span live-count/reclaim policy.
- Added page-backend in-place growth support for single-block slab refill spans, replacing the old direct `mremap` shortcut for large small-class realloc growth under the new page-backend model.
- `bulk_fill()` still initializes headers lazily for only the requested adaptive batch and leaves remaining span space tracked by thread-local or pending `MetaData`.
- `RADIX` ownership and cached-VA accounting still apply to the allocated metadata span, not to every byte of page-backend arena slack.
- Added a lock-free per-node/per-class global pending metadata queue so thread-exit drained refill metadata can be reused by local-node threads.
- Added thread-exit draining of thread-local pending refill metadata into the per-node global pending queue.
- Added low-level TLS destructor registration for `ThreadBulk` cleanup and a regression test proving pending metadata is drained on thread exit.
- Added page-backend THP advice knobs:
  - `page-backend-no-huge-page` asks Linux not to use huge pages for page-backend arenas, useful on THP-aggressive systems where arena slack inflates RSS.
  - `page-backend-huge-page` requests huge-page advice for TLB-sensitive experiments when `page-backend-no-huge-page` is not also enabled.
  - enabling both page-backend advice features intentionally results in no explicit page-backend THP advice.

### Adaptive refill batcher

- Replaced the old EMA refill predictor with a small integer adaptive batcher for per-thread/per-class refill sizing.
- Split normal refill prediction from bulk-fill prediction so cache-pop/steal behavior does not force page/list initialization into tiny batches.
- The batcher grows quickly when a refill fully satisfies the requested batch, then shrinks only after repeated low-demand observations.
- Growth uses the larger of observed demand and roughly 1.5x the current batch, while sustained low demand halves the batch after several samples.
- Kept the implementation integer-only: no floating point and no EMA alpha knob.
- Renamed the Rust configuration API from `EMASettings`/`with_ema_settings` to `RefillPredictorSettings`/`with_refill_predictor_settings`.
- Preload builds keep `RS_PREDICTOR_INIT_BATCH` as the initial batch-size knob.

### NUMA-aware allocation and binding

- Added NUMA topology parsing from sysfs with CPU-to-node mapping, direct `cpu_ranges[node_id]` lookup, malformed-list handling, overflow checks, CPU clipping, and fallback to node `0` for missing/invalid CPU entries.
- Added `internals::binder` for NUMA placement helpers:
  - `prefer_node(...)` using `MPOL_PREFERRED`
  - `bind_node(...)` using `MPOL_BIND`
- Moved transfer-cache stealing, slab page-backend arenas, buddy allocation, and direct big mappings toward current-CPU node locality.
- Bulk-bound contiguous per-CPU `SLAB_CACHE` / transfer-cache ranges to their NUMA node when topology is available, instead of issuing per-CPU binding calls.
- Added preferred NUMA placement through `prefer_node(...)`, currently using the `syscalls` crate's `mbind` wrapper with `MPOL_PREFERRED | MPOL_F_STATIC_NODES`.
- Added `node_id` to small refill `MetaData` so abandoned pending metadata returns to the correct NUMA queue.
- Added bootstrap allocation for the pending queue's node/class head table using parsed NUMA range count, with non-NUMA systems using node slot `0`.

### Buddy backend and big allocations

- Added NUMA-aware buddy region tagging, local-node-first allocation, local-node growth, and remote-node fallback scanning.
- Added preferred NUMA placement for buddy regions and direct big mapping fallback.
- Changed buddy region `nonempty_mask` from a plain `u8` to `AtomicU8`.
- Replaced the single per-region buddy free-list lock with `order_locks: [SpinLock; NUM_ORDERS]`.
- Made configured buddy regions larger than the 64 MiB maximum allocation order usable by partitioning each mapping into independent 64 MiB top-order blocks. Region sizing now uses checked power-of-two normalization without widening the fixed order tables or allowing coalescing above the cached-allocation limit.
- Replaced the hashmap-based big-allocation metadata map with `internals::rbtree`, a hand-rolled red-black tree (`RBTree`), keeping metadata lookups lock-protected behind one global `SpinLock`.
- Fixed buddy free-block lifetime stamping: newly created region blocks and bootstrap/global-allocator init now seed `CURRENT_STAMP` from `get_clock()` instead of leaving fresh blocks stamped `0`, so trim-eligibility timing is correct from process start.
- Stored the stable owning buddy-region token in large-allocation metadata. Buddy free and in-place realloc growth now address their region directly instead of linearly scanning the append-only region list; direct mappings retain a zero owner token.
- Added per-region, per-order free-block bitmaps and backward free-list links. Buddy coalescing and in-place growth now test exact buddy membership through the bitmap and unlink known free buddies in constant time instead of linearly searching an order list; existing lifetime and trim-state traversal remains intact.
- Updated buddy allocation, free/coalescing, in-place growth, trim, and fork-child lock reset paths to use per-order locks.
- Added buddy free-block lifetime/trim state tracking and background old-block trimming for buddy cached blocks, with successful trim accounting based on `madvise` success.
- Routed realloc copy/fallback paths through shared inner `rs_alloc`/`rs_free` operations so big-block transitions reuse normal ownership, buddy, metadata-map, and optional semi-hardening checks.
- Updated direct/aligned big allocation ownership tracking so `RADIX` uses the correct direct-big or full-range shape.

### Trim, relief, calloc, and memory advice

- Added small-allocation trimming for size classes equal to or greater than 4096 bytes.
- Updated `malloc_trim(...)` and Rust-facing trim support to combine buddy-cache trimming with eligible small-allocation cache trimming.
- Added `should_trim` and lifetime tracking to allocation headers so trimmed blocks are not repeatedly advised without reuse.
- Added a background trim worker with `RS_DISABLE_TRIM_THREAD` runtime control.
- Added `RS_TRIMMER_THRESHOLD`, defaulting to 10 MiB of cached virtual address space, so the background trim worker is not started during fragile early preload/bootstrap paths.
- Added `lazy-page-trim` to use lazy page-free advice for eligible small-allocation and buddy trim paths.
- Added a global non-blocking trim lock so manual trim, background trim, small trim, and buddy trim do not overlap.
- Updated calloc zeroing to distinguish full zero-state from trimmed/cold state. `TRIMMED_FLAG` means the block went through trim/cold handling, not that the entire user payload is guaranteed zero, so calloc still zeroes as needed.
- Updated Rust global-allocator configuration with `TrimThreadSettings`, `TrimThread`, `Bytes`, `ReliefSettings`, `ReliefState`, and `Percentage` for background trim worker and opt-in system-memory-pressure relief control.

### Semi-hardening and safety diagnostics

- Added `check-owned-on-alloc`, an opt-in semi-hardening diagnostic feature that verifies non-null popped small/big allocation pointers are still owned by `RADIX` before stamping them allocated.
- The ownership check is intended to catch some freelist/transfer-cache metadata-injection style corruption earlier; it is not a full pointer-integrity or class/header validation proof.
- Strengthened aligned-pointer recovery by checking that recovered original pointers are owned by `RADIX` before trusting aligned metadata.
- Added a small free-path user-address-range check before handling unowned pointers as foreign.
- Batched `RADIX` ownership range updates by 64-bit bitmap word and L3 leaf. Contiguous registration and removal now resolve the radix hierarchy once per 16 MiB leaf and update up to 64 adjacent 4 KiB ownership chunks with one Release atomic RMW, while masked boundary words preserve unrelated ownership bits.
- Kept weakened magic-value modes behind explicit unsafe acknowledgement in the Rust configuration surface.

### Concurrency and ordering fixes

- Fixed `SpinLock::get_lock()` to use `Acquire` instead of `Relaxed`, closing a data race where `BuddyAllocator::regions_head()` could observe a partially-published region list after only polling the lock state.
- Fixed `Once::call_once()`'s fast path to use `Acquire` instead of `Relaxed`, ensuring initialization side effects are properly visible once the fast path is taken.

### Preload, C ABI, fork, and runtime configuration

- Added preload errno helpers and improved C ABI errno behavior for calloc overflow/failure and alignment API failures.
- Updated C alignment APIs toward standard behavior, including `posix_memalign` validation, `aligned_alloc` size-multiple checks, checked `pvalloc` page rounding, and `memalign` errno reporting.
- Added fork-child reset handling for trim locks, buddy backend locks, `BIG_META_MAP` locks, fallback symbol initialization, and background trim state.
- Added `SerialLock::try_lock()` and fork-reset helpers for allocator-internal locks.
- Updated preload runtime configuration documentation for `RS_DISABLE_TRIM_THREAD`, `RS_TRIMMER_THRESHOLD`, default-disabled `RS_ENABLE_RELIEF` where `0` enables relief in the current alpha preload path, buddy relief pressure thresholds, adaptive refill predictor initialization, and buddy-cache sizing behavior.

### Debug modes, reporting, and telemetry

Debug mode behavior is a major part of `0.2.0-alpha` because several allocator subsystems now have separate low-overhead, exact, and high-overhead diagnostic modes:

- Added `debug` as the base instrumentation mode for refill/RSEQ counters such as abort and predictor-miss tracking.
- Added `debug-print`, which enables `debug` and prints an exit-time internal report through a plain `.fini_array` path using `eprintln!`.
- Added `debug-printer-thread`, which enables `debug-print` and starts a simple once-only background thread for repeated live allocator reports.
- Added `debug-exact`, which enables `debug-print` and records higher-overhead lock telemetry: lock calls, retries, try-lock calls/misses, and spin waits.
- Added `debug-predictor-exact`, which enables `debug-print` and uses more intrusive refill-prediction accounting that can probe transfer caches to distinguish over/under prediction more accurately.
- Added `predictor-debug`, which logs per-class predictor batch decisions directly from the predictor path with `eprintln!`.
- Added `transfer-debug`, which enables `debug-exact` and tracks transfer-cache steals, dry steals, and CAS retries.
- Added `transfer-debug-exact`, which enables `transfer-debug` and additionally counts transfer push/pop calls.
- Added convenience feature groups `debug-full` and `debug-full-critic` for broad allocator instrumentation; `debug-full-critic` also enables exact predictor diagnostics.
- Added debug mmap telemetry: total allocator `mmap` calls and requested bytes.
- Added transfer class hint dumps, printing per-class CPU hint strings where `1 = hinted nonempty` and `0 = no nonempty hint`.
- Added trim/relief report fields for trim calls, trimmed VA, small trimmed block counts under `debug-exact`, and average small `madvise` cycles under `debug-exact`.
- Added human-readable byte formatting for report fields, including raw byte counts alongside KiB/MiB/GiB-style units.
- Added report sections for process state, NUMA topology, RSEQ refill counters, lock counters, transfer-cache counters, cached virtual address usage, trim/relief state, buddy backend state, radix ownership metadata, mmap counters, and transfer class hints.
- Added per-CPU RSEQ cache usage reporting in bytes, including total/min/max/non-empty CPU summaries.
- Added per-size-class refill telemetry through `REFILLS_BY_CLASS` and report output for every size class, including payload size, refill count, total cached bytes, active CPU count, and per-active-CPU min/max/average cached bytes.
- Added detailed buddy backend report fields for per-order free-block state breakdowns: never allocated, reused, and trimmed blocks.
- Expanded buddy report output with used/free bytes, used/free percentages, free block totals, state-specific byte totals, and a per-order free-list breakdown.
- Expanded radix reporting with chunk size, owned bytes, metadata bytes, and metadata-per-owned-chunk estimates.

### Benchmarks, tests, and documentation

- Replaced the old `speed` benchmark with `book_speed`, later extended with a multi-threaded `alloc_free_mt` benchmark group to measure bookkeeping speed under concurrent per-CPU cache contention across several thread counts and sizes.
- Added the large `rstress` benchmark covering thread churn, allocator edge cases, SIMD-style allocation patterns, teardown behavior, and trim pressure.
- Added `rstress_app.rs`, a larger multi-threaded stress/application-shaped benchmark harness with producer/processor/aggregator thread roles, later refined alongside a page-backend arena overhaul.
- Added checked-in benchmark notes/results for the updated benchmark suite.
- Added NUMA parser tests for range parsing, whitespace, malformed lists, overflow rejection, CPU clipping, sparse `cpu_ranges[node_id]` behavior, and missing/invalid CPU fallback.
- Added a thread-exit pending metadata drain regression test for refill `ThreadBulk` cleanup.
- Added radix range batching tests covering bitmap-word boundaries, L3-leaf boundaries, masked clearing, and unaligned byte ranges.
- Updated README, TODO, and architecture documentation for current trim capabilities, NUMA-aware subsystems, hybrid slab page-backend behavior, transfer-cache behavior, pending metadata queue behavior, internal component naming, feature flags, and detailed allocation/free lifecycle diagrams.

## v0.1.0-alpha

### Release layout

- Bumped the release line to `0.1.0-alpha`.
- Added package metadata for description, repository, README, and license-file fields.
- Added Cargo feature gating for preload-specific code through the new `preload` feature.
- Added explicit feature flags for legacy raw-RSEQ fallback support and opt-in predictor/RSEQ debug logging.
- Added `rlib` output alongside `cdylib`, allowing Rust global-allocator usage and preload builds from the same crate.
- Added an explicit x86-64-only compile guard for the current assembly-backed implementation.
- Replaced the generated release `build.rs` configuration flow with static Cargo configuration.

### Rust global allocator API

- Added `global_alloc` support with `RSMalloc`, `RSMallocConfig`, and `GlobalAlloc` integration for non-preload usage.
- Added `RSMalloc::new_default()` and `RSMalloc::new_with_config(...)` for default and explicit const configuration.
- Added `RSMALLOC_VERSION` as an allocation-free crate version string exposed from Cargo package metadata.
- Added `RSMallocCapabilities` and `RSMalloc::get_capabilities()` for querying configured THP state, current NUMA support status, and allocator version.
- Added direct Rust-facing helper methods on `RSMalloc` for malloc-style allocation, calloc, aligned allocation, realloc, buddy trimming, free, and usable-size queries.
- Added `Size::RS` and `Size::NotRS` usable-size result variants for the Rust-facing API.
- Added `RSMallocTrim` and `RSTrimStatus` for the Rust-facing buddy trim API.
- Added public non-preload configuration exports for `RSMallocConfig`, `THPSettings`, `THP`, `BuddyTHP`, `RefillPredictorSettings`, `MagicSafety`, `MagicSafetyDisable`, `DisableMagic`, `ForeignPointerSettings`, `ForeignPointerPolicy`, `PerCacheLimit`, `ExperimentalFeatures`, `RSMallocTrim`, `RSTrimStatus`, `Size`, `RSMallocCapabilities`, and `RSMalloc`.

### Runtime configuration

- Added grouped runtime configuration for the Rust global allocator:
  - `THPSettings`, `THP`, and `BuddyTHP` control general THP behavior and buddy-region huge-page requests without raw boolean arguments.
  - `RefillPredictorSettings` controls the initial refill predictor batch.
  - `MagicSafety` controls randomized magic values, fixed magic values, or disabled magic validation.
  - `ForeignPointerSettings` controls whether foreign pointers in non-preload/global-allocator mode are ignored or abort.
  - `max_refill_retries` controls refill and mapping retry counts.
  - `PerCacheLimit` and `max_per_buddy_cache` control initial buddy cache sizing.
  - `ExperimentalFeatures::with_buddy_trim()` enables the experimental Rust-mode buddy trim surface.
- Added `ForeignPointerSettings::new(...)` and `RSMallocConfig::with_foreign_pointer_policy(...)` convenience constructors.
- Added configurable refill retry behavior through `RS_MAX_REFILL_RETRIES`, defaulting to `3`.
- Added `RS_ENABLE_TRIM` to enable preload `malloc_trim(...)` buddy trimming; it is disabled by default.
- Moved allocator bootstrap configuration into reusable runtime state for buddy cache sizing, huge-page attempts, THP disabling, EMA alpha, predictor initial batch size, foreign-pointer handling, magic behavior, and refill retries.
- Clamped custom EMA alpha values to a sane range during global-allocator initialization.

### Safety and hardening

- Added unsafe acknowledgement tokens for weakened magic behavior:
  - `MagicSafetyDisable::acknowledge_safety_risk()` keeps fixed built-in magic values.
  - `DisableMagic::acknowledge_safety_risk()` disables magic checks and randomization for security research, allocator experiments, and tightly controlled debugging.
- Added randomized aligned-allocation tags during normal allocator bootstrap, reducing reliance on a fixed `ALIGN_TAG` value for detecting over-aligned allocation metadata.
- Added optional `extended-header` Cargo feature for wider allocator metadata during experiments and stress testing.
- Changed global-allocator foreign-pointer handling to default to aborting on foreign pointers in non-preload mode, with `ForeignPointerSettings::IGNORE` available for explicit ignore behavior.
- Added non-preload foreign-pointer abort handling in `free`, `realloc`, and usable-size queries.
- Fixed big-allocation usable-size lookup to query metadata by the original payload pointer.
- Updated `RADIX`, the ownership map, to a lazy multi-level atomic tree covering the low canonical 56-bit user address range, with checked range bounds instead of modulo-wrapped indices.
- Added checked size arithmetic in the big-allocation path so oversized allocation requests fail cleanly instead of wrapping.
- Replaced the `SLAB_CACHE` initialization panic with allocator error reporting on mmap failure.

### Preload and C ABI

- Updated preload-only fallback, libc, fork, and errno handling to compile only when the `preload` feature is enabled.
- Updated calloc and malloc failure paths so errno writes are only used in preload builds.
- Updated fork-child handling to re-register internal RSEQ state when libc RSEQ symbols are unavailable and `legacy-glibc-support` is enabled.
- Preserved the existing preload `free_sized` and `free_aligned_sized` symbols as compatibility shims over normal `free`, avoiding loader-constructor and GLib sized-free mismatches during preload startup.
- Updated preload C ABI coverage around alignment and realloc-family entry points, including `posix_memalign`, `memalign`, `aligned_alloc`, `valloc`, `pvalloc`, `reallocarray`, and `recallocarray`.

### Free and Realloc behavior

- Rust `GlobalAlloc::dealloc` delegates through the shared `rs_free` path; preload sized-free symbols remain compatibility shims over normal `free`.
- Updated Rust `GlobalAlloc::realloc` to delegate through the shared `rs_realloc` path, including over-aligned allocation handling.
- Fixed the small-allocation `realloc` `mremap` path to use the slab mapping base and metadata-inclusive mapping sizes, and limited that fast path to in-place remaps before falling back to allocate/copy/free.
- Updated realloc unit tests to run only in preload builds, matching their direct internal allocation path.

### Big allocations

- Updated aligned big allocation ownership tracking so direct aligned big mappings mark and clear full radix ranges instead of only one radix page.
- Updated big allocation metadata to track whether an allocation came from an aligned request, so free can clear the correct ownership shape.
- Optimized direct big-allocation realloc bookkeeping by avoiding map remove/insert and radix updates when `mremap` keeps the mapping in place.
- Restricted direct big-allocation `mremap` growth to in-place remaps.
- Fixed buddy-backed big realloc growth to use the buddy block base instead of the payload pointer, and to preserve the enlarged buddy order when fallback allocation is needed after partial in-place growth.
- Added retry loops for buddy backend metadata and region mapping attempts.
- Added requested-size buddy trimming through `malloc_trim(...)` gated by `RS_ENABLE_TRIM` in preload mode and opt-in `RSMalloc::rs_trim_buddy(...)` in Rust mode; the Rust API uses `RSMallocTrim::Request(bytes)` or `RSMallocTrim::All` and returns `RSTrimStatus`, while C ABI `malloc_trim(0)` requests trimming all currently free buddy blocks when preload trimming is enabled.

### EMA refill prediction

- Added EMA-based small-allocation refill prediction to smooth recent allocation activity and estimate current per-size-class demand, allowing refill batch sizes to adapt to workload pressure without overreacting to one-off bursts.
- Added an observed-demand uplift fix: when a refill path returns exactly the requested batch and the class has headroom, observed demand is bumped by +25% (clamped to class max) before feeding the EMA so predictors can recover faster after saturation bursts.
- Kept bulk-fill prediction separate from cache-pop/steal demand so page/list initialization can still happen in practical batches.

### RSEQ and performance

- Optimized refill behavior by making retry counts configurable and reusing the same retry setting across small refills and buddy mapping attempts.
- Optimized RSEQ fast paths by simplifying retry loops, reducing extra spinning, and trimming unused pop/cache trait indirection.
- Added next-node prefetching in the RSEQ pop inline-assembly fast path.
- Updated `SLAB_CACHE` layout to 4096-byte alignment to keep per-CPU cache structures page-separated and reduce false sharing risk.
- Changed the hot-path `SLAB_CACHE` usage counter into an approximate pressure signal to avoid stale-high transfer-routing cliffs while allowing recoverable stale-low drift.
- Fixed low-level x86-64 RSEQ registration syscall clobber handling and documented the weak libc RSEQ symbol pointer pattern used for fallback detection.
- Added failure handling for per-thread internal RSEQ registration before returning thread-local RSEQ state.
- Added default `rseq-thread-failure-fallback` handling for invalid/unregistered RSEQ CPU IDs, using the extra overflow cache slot instead of indexing per-CPU state directly.
- Removed the experimental per-CPU pending-refill metadata path in favor of the thread-local pending refill path.
- Added `debug-predictor-exact` instrumentation mode for exact refill prediction miss accounting when high-overhead diagnostics are needed.
- Simplified `SLAB_CACHE`/RSEQ core retry paths and removed unused cache trait/API surface.

### Testing and validation

- Added Rust global-allocator integration tests covering standard collections, usable-size queries, zeroed allocation, over-aligned realloc behavior, big allocation usable size, and direct `RSMalloc` helper methods.
- Added C ABI stress/correctness tests for multithreaded malloc/free/realloc behavior, random realloc sizes, and slab growth/shrink prefix preservation.
- Added checked-in benchmark snapshot notes under `benchmarks/` as development signals rather than stable performance claims.

### Documentation and cleanup

- Added crate-level Rust documentation with quick-start examples for default and explicitly configured global allocator usage.
- Added `CONTRIBUTING.md` with safety-sensitive contribution guidance and recommended checks.
- Reworked the README around the current alpha architecture, supported modes, configuration, limitations, and benchmark caveats.
- Added `architecture.md` as a fuller alpha architecture walkthrough covering `SLAB_CACHE` flow, `TransferCache`, refill metadata, EMA prediction, big allocations, buddy trimming, and current tradeoffs; linked it from the README.
- Cleaned up pointer wrapper helpers and internal unused APIs.
- Removed unused radix collision helpers and related tests from the main radix implementation.
- Removed unused build flags from generated Rust configuration.

## v0.0.2-pre-alpha

- Reworked the big allocation path into the `big_allocations` module.
- Added a buddy backend for 4 MiB to 64 MiB big allocations, with pool growth, block splitting/coalescing, and in-place growth support for eligible reallocations.
- Added configurable big-allocation bootstrap settings:
  - `RS_BUDDY_PER_CACHE_SIZE`
  - `RS_BUDDY_ATTEMPT_HUGEPAGE`
  - `RS_DISABLE_THP`
  - `RS_DISABLE_RANDOMIZING`
- Added `RS_PREDICTOR_INIT_BATCH` to configure the initial predictor batch size for small allocation refills.
- Added early refill predictor runtime tuning. Current alpha releases use `RS_PREDICTOR_INIT_BATCH` instead of an EMA alpha knob.
- Updated big allocation metadata to track allocation order so pooled blocks can be returned to the buddy backend correctly.
- Changed big allocation ownership tracking to use single `RADIX` entries for direct mappings and full ranges for buddy regions.
- Added raw RSEQ registration support with an internal thread-local `rseq` fallback when libc TLS RSEQ symbols are unavailable.
- Added CPU migration checks to the RSEQ assembly fast path and reduced retry loops before falling back to transfer-cache paths.
- Added dynamic CPU count detection for `SLAB_CACHE` initialization through `get_nprocs_conf`.
- Added ABA-tagged transfer-cache pointers and a single-item transfer-cache pop path.
- Added adaptive per-thread refill prediction for small allocation batches.
- Changed small allocation refill flow so `bulk_fill` returns initialized batches directly instead of pushing them to the transfer cache first.
- Updated `free` to detect big allocations before fallback ownership checks, and preserve double-free/corruption abort behavior.
- Updated `realloc` handling for in-place small shrink/no-op cases, null-pointer allocation behavior, big allocation in-place growth, and out-of-memory propagation.
- Added `malloc_trim` ABI stub, just a no-op stub that returns success.
- Added fork handlers to reset fallback symbol initialization in forked children.
- Set `errno` to `ENOMEM` on small allocation failure.

### Small Note From dev: I've been testing this update through my personal computer via preloading through `/etc/ld.so.preload` for days by now so should be stable enough for use but not yet ready for production also COME ON NAME ALSO TELLS PRE-ALPHA.

## v0.0.1-pre-alpha

- Initial public development baseline.
