# Updates

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
- Added optional `canary` and `extended-header` Cargo features for lightweight higher-security allocator metadata checks during experiments and stress testing. These are defense-in-depth diagnostics, not high-security isolation guarantees.
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
- Restricted direct big-allocation `mremap` growth to in-place remaps and recomputed canary metadata after successful remap when canary checks are enabled.
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
- Added optional per-CPU pending-refill metadata paths behind `cpu-refill-paths` for experiments with shared refill metadata locality tradeoffs.
- Added default thread-exit draining of thread-local pending refill metadata into mail caches to reduce stranded pending slabs, with an opt-out `disable-thread-pending` feature flag.
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
