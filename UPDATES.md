# Updates

## v0.2.0-alpha

v0.2.0-alpha focuses on memory reclamation, NUMA-aware placement, buddy allocator overhaul, and preload robustness. It adds small-allocation trimming, buddy-cache old-block trimming, NUMA-aware RSEQ/buddy/big refill behavior, a lock-free pending metadata queue, background trim worker support, opt-in memory-pressure relief, lazy page trim support, and several fork/errno/alignment fixes.

- Removed the allocator canary feature and decoupled `extended-header` from canary-specific metadata/checking.
- Added small-allocation trimming for size classes equal to or greater than 4096 bytes.
- Added a background trim worker with `RS_DISABLE_TRIM_THREAD` runtime control.
- Added `RS_TRIMMER_THRESHOLD`, defaulting to 10 MiB of cached virtual address space, so the background trim worker is not started during fragile early preload/bootstrap paths.
- Added `lazy-page-trim` to use lazy page-free advice for eligible small-allocation and buddy trim paths.
- Updated `malloc_trim(...)` and Rust-facing trim support to combine buddy-cache trimming with eligible small-allocation cache trimming.
- Added `should_trim` and lifetime tracking to allocation headers so trimmed blocks are not repeatedly advised without reuse.
- Added buddy free-block lifetime/trim state tracking and background old-block trimming for buddy cached blocks, with successful trim accounting based on `madvise` success.
- Added a global non-blocking trim lock so manual trim, background trim, small trim, and buddy trim do not overlap.
- Added per-mail trim locks and ABA-tagged mail-cache support for safely detaching and restoring trim-scanned mail lists.
- Added fork-child reset handling for trim locks, buddy locks, big-allocation map locks, and background trim state.
- Added `SerialLock::try_lock()` and fork-reset helpers for allocator-internal locks.
- Updated fallback symbol initialization to use resettable once-lock state after fork.
- Added preload errno helpers and improved C ABI errno behavior for calloc overflow/failure and alignment API failures.
- Added NUMA topology parsing from sysfs with CPU-to-node mapping, direct `cpu_ranges[node_id]` lookup, malformed-list handling, overflow checks, CPU clipping, and fallback to node `0` for missing/invalid CPU entries.
- Added NUMA preferred-placement support through `prefer_node(...)`, currently using the `syscalls` crate's `mbind` wrapper with `MPOL_PREFERRED | MPOL_F_STATIC_NODES`.
- Reworked RSEQ assembly into `src/rseq_core/rseq_asm.rs` and removed the old `rseq_core.rs` module.
- Updated RSEQ cache APIs for trim access, overflow mail handling, NUMA topology access, local-node lookup, and fork-time trim-lock reset.
- Added `get_size_4096_class()` and cached lookup support for selecting trim-eligible size classes.
- Updated Rust global-allocator configuration with `TrimThreadSettings`, `TrimThread`, `Bytes`, `ReliefSettings`, `ReliefState`, and `Percentage` for background trim worker and opt-in system-memory-pressure relief control.
- Updated C alignment APIs toward standard behavior, including `posix_memalign`-style validation, `aligned_alloc` size-multiple checks, checked `pvalloc` page rounding, and `memalign` errno reporting.
- Updated calloc zeroing to use allocation zero-state flags correctly while always zeroing under `lazy-page-trim`.
- Updated preload runtime configuration documentation for `RS_DISABLE_TRIM_THREAD`, `RS_TRIMMER_THRESHOLD`, default-disabled `RS_DISABLE_RELIEF`, buddy relief pressure thresholds, current EMA clamping, and buddy-cache sizing behavior.
- Replaced the old `speed` benchmark with `book_speed` and `rstress`, and added checked-in `rstress` benchmark results with thread-churn, allocator edge-case, SIMD, teardown, and trim-pressure coverage.
- Updated README, TODO, and architecture documentation for current trim capabilities, NUMA-aware subsystems, pending metadata queue behavior, and feature flags.

### NUMA-aware allocation

- Added NUMA-aware RSEQ victim stealing: allocation first tries local CPU mail, then CPUs in the same node range, then remote node ranges when NUMA is active.
- Added NUMA node selection for small refill mappings, direct big mappings, and buddy regions based on current CPU id.
- Added `node_id` to small refill `MetaData` so abandoned pending metadata returns to the correct NUMA queue.
- Added per-node/per-class global pending metadata queues so thread-exit drained refill metadata is reused by local-node threads instead of being globally mixed.
- Added bootstrap allocation for the pending queue's node/class head table using the parsed NUMA range count, with non-NUMA systems using node slot `0`.

### Buddy allocator overhaul

- Added NUMA-aware buddy region tagging, local-node-first allocation, local-node growth, and remote-node fallback scanning.
- Added preferred NUMA placement for buddy regions and direct big mapping fallback.
- Added per-region buddy `nonempty_mask` tracking so allocation can skip empty order lists with bit operations instead of linearly scanning every order.

### RSEQ refill and pending metadata

- Added a lock-free global pending metadata queue for abandoned thread-local refill metadata, indexed by NUMA node and size class.
- Added thread-exit draining of thread-local pending refill metadata into the per-node global pending queue to reduce stranded pending slabs.
- Added low-level TLS destructor registration for `ThreadBulk` cleanup and a regression test proving pending metadata is drained on thread exit.

### Testing

- Added NUMA parser tests for range parsing, whitespace, malformed lists, overflow rejection, CPU clipping, sparse `cpu_ranges[node_id]` behavior, and missing/invalid CPU fallback.
- Added a thread-exit pending metadata drain regression test for the refill `ThreadBulk` cleanup path.

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
- Added public non-preload configuration exports for `RSMallocConfig`, `THPSettings`, `THP`, `BuddyTHP`, `EMASettings`, `EmaAlpha`, `MagicSafety`, `MagicSafetyDisable`, `DisableMagic`, `ForeignPointerSettings`, `ForeignPointerPolicy`, `PerCacheLimit`, `ExperimentalFeatures`, `RSMallocTrim`, `RSTrimStatus`, `Size`, `RSMallocCapabilities`, and `RSMalloc`.

### Runtime configuration

- Added grouped runtime configuration for the Rust global allocator:
  - `THPSettings`, `THP`, and `BuddyTHP` control general THP behavior and buddy-region huge-page requests without raw boolean arguments.
  - `EMASettings` and `EmaAlpha` control refill predictor responsiveness.
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
- Updated the radix ownership map to a lazy multi-level atomic tree covering the low canonical 56-bit user address range, with checked range bounds instead of modulo-wrapped indices.
- Added checked size arithmetic in the big-allocation path so oversized allocation requests fail cleanly instead of wrapping.
- Replaced the RSEQ cache initialization panic with allocator error reporting on mmap failure.

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
- Added retry loops for buddy allocator metadata and region mapping attempts.
- Added requested-size buddy trimming through `malloc_trim(...)` gated by `RS_ENABLE_TRIM` in preload mode and opt-in `RSMalloc::rs_trim_buddy(...)` in Rust mode; the Rust API uses `RSMallocTrim::Request(bytes)` or `RSMallocTrim::All` and returns `RSTrimStatus`, while C ABI `malloc_trim(0)` requests trimming all currently free buddy blocks when preload trimming is enabled.

### EMA refill prediction

- Added EMA-based small-allocation refill prediction to smooth recent allocation activity and estimate current per-size-class demand, allowing refill batch sizes to adapt to workload pressure without overreacting to one-off bursts.
- Added an observed-demand uplift fix: when a refill path returns exactly the requested batch and the class has headroom, observed demand is bumped by +25% (clamped to class max) before feeding the EMA so predictors can recover faster after saturation bursts.
- Kept bulk-fill prediction separate from cache-pop/steal demand so page/list initialization can still happen in practical batches.

### RSEQ and performance

- Optimized refill behavior by making retry counts configurable and reusing the same retry setting across small refills and buddy mapping attempts.
- Optimized RSEQ fast paths by simplifying retry loops, reducing extra spinning, and trimming unused pop/cache trait indirection.
- Added next-node prefetching in the RSEQ pop inline-assembly fast path.
- Updated RSEQ cache layout to 4096-byte alignment to keep per-CPU cache structures page-separated and reduce false sharing risk.
- Changed the hot-path RSEQ cache usage counter into an approximate pressure signal to avoid stale-high mailbox routing cliffs while allowing recoverable stale-low drift.
- Fixed low-level x86-64 RSEQ registration syscall clobber handling and documented the weak libc RSEQ symbol pointer pattern used for fallback detection.
- Added failure handling for per-thread internal RSEQ registration before returning thread-local RSEQ state.
- Added default `rseq-thread-failure-fallback` handling for invalid/unregistered RSEQ CPU IDs, using the extra overflow cache slot instead of indexing per-CPU state directly.
- Removed the experimental per-CPU pending-refill metadata path in favor of the thread-local pending refill path.
- Added `debug-predictor-exact` instrumentation mode for exact refill prediction miss accounting when high-overhead diagnostics are needed.
- Simplified RSEQ cache/core retry paths and removed unused cache trait/API surface.

### Testing and validation

- Added Rust global-allocator integration tests covering standard collections, usable-size queries, zeroed allocation, over-aligned realloc behavior, big allocation usable size, and direct `RSMalloc` helper methods.
- Added C ABI stress/correctness tests for multithreaded malloc/free/realloc behavior, random realloc sizes, and slab growth/shrink prefix preservation.
- Added checked-in benchmark snapshot notes under `benchmarks/` as development signals rather than stable performance claims.

### Documentation and cleanup

- Added crate-level Rust documentation with quick-start examples for default and explicitly configured global allocator usage.
- Added `CONTRIBUTING.md` with safety-sensitive contribution guidance and recommended checks.
- Reworked the README around the current alpha architecture, supported modes, configuration, limitations, and benchmark caveats.
- Added `architecture.md` as a fuller alpha architecture walkthrough covering RSEQ cache flow, `SelfMail`, refill metadata, EMA prediction, big allocations, buddy trimming, and current tradeoffs; linked it from the README.
- Cleaned up pointer wrapper helpers and internal unused APIs.
- Removed unused radix collision helpers and related tests from the main radix implementation.
- Removed unused build flags from generated Rust configuration.

## v0.0.2-pre-alpha

- Reworked the big allocation path into the `big_allocations` module.
- Added a buddy allocator for 4 MiB to 64 MiB big allocations, with pool growth, block splitting/coalescing, and in-place growth support for eligible reallocations.
- Added configurable big-allocation bootstrap settings:
  - `RS_BUDDY_PER_CACHE_SIZE`
  - `RS_BUDDY_ATTEMPT_HUGEPAGE`
  - `RS_DISABLE_THP`
  - `RS_DISABLE_RANDOMIZING`
- Added `RS_PREDICTOR_INIT_BATCH` to configure the initial predictor batch size for small allocation refills.
- Added `RS_EMA_ALPHA` to configure the exponential moving average alpha value for the predictor.
- Updated big allocation metadata to track allocation order so pooled blocks can be returned to the buddy allocator correctly.
- Changed big allocation ownership tracking to use single radix entries for direct mappings and full ranges for buddy regions.
- Added raw RSEQ registration support with an internal thread-local `rseq` fallback when libc TLS RSEQ symbols are unavailable.
- Added CPU migration checks to the RSEQ assembly fast path and reduced retry loops before falling back to mail-cache paths.
- Added dynamic CPU count detection for RSEQ cache initialization through `get_nprocs_conf`.
- Added ABA-tagged mail-cache pointers and a single-item mail-cache pop path.
- Added adaptive per-thread refill prediction for small allocation batches.
- Changed small allocation refill flow so `bulk_fill` returns initialized batches directly instead of pushing them to the mail cache first.
- Updated `free` to detect big allocations before fallback ownership checks, and preserve double-free/corruption abort behavior.
- Updated `realloc` handling for in-place small shrink/no-op cases, null-pointer allocation behavior, big allocation in-place growth, and out-of-memory propagation.
- Added `malloc_trim` ABI stub, just a no-op stub that returns success.
- Added fork handlers to reset fallback symbol initialization in forked children.
- Set `errno` to `ENOMEM` on small allocation failure.

### Small Note From dev: I've been testing this update through my personal computer via preloading through `/etc/ld.so.preload` for days by now so should be stable enough for use but not yet ready for production also COME ON NAME ALSO TELLS PRE-ALPHA.

## v0.0.1-pre-alpha

- Initial public development baseline.
