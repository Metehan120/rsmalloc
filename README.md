# RSMalloc (rseq/rust slab memory allocator)

rsmalloc is a Rust memory allocator focused on low-overhead concurrent allocation for real-world application workloads, not benchmark-only allocation patterns. The slab allocation path is built around Linux Restartable Sequences (RSEQ), so cache ownership is CPU-oriented rather than thread-oriented. Larger allocations use a separate big-allocation path with a buddy backend for cached regions.

Current release line: `0.2.0-alpha`.

`0.2.0-alpha` is a full allocator overhaul from the `0.1.0-alpha` line: RSEQ/slab internals, transfer-cache balancing, lazy refill metadata reuse, NUMA placement, buddy caching/trimming, preload ABI behavior, and Rust configuration have all changed materially.

crates.io: https://crates.io/crates/rsmalloc

See [UPDATES.md](UPDATES.md) for release notes and [TODO.md](TODO.md) for planned work.
See [benchmarks/benchmarks.md](benchmarks/benchmarks.md) for general benchmark notes and [benchmarks/rstress_results.md](benchmarks/rstress_results.md) for the current `rstress` RSEQ allocator stress benchmark snapshot.

> **Note:** Linux kernel `7.0.10` appears to trigger `SIGBUS` in some workloads when using rsmalloc. If you hit unexplained `SIGBUS` crashes, try updating or downgrading your kernel before assuming allocator corruption.

## Current Status

rsmalloc is under active development. It is intended to become a practical allocator for normal applications with messy, mixed allocation behavior, not a microbenchmark-specialized allocator. It is useful for allocator experiments, preload testing, and early integration work, but **it is not production-ready.**

The crate currently requires nightly Rust and `rustc 1.96.0` or higher.

The current codebase supports two intended modes:

- `cdylib` preload mode with the `preload` feature enabled.
- Rust library/global allocator mode through the `rlib` target and `RSMalloc`.

## Current Capabilities

- Small allocations are served from size classes backed by `SLAB_CACHE` per-CPU RSEQ caches, transfer caches, adaptive refill, a hybrid slab page backend, and pending refill metadata reuse.
- RSEQ fast paths use inline assembly critical sections for push/pop operations.
- Overflow and zero-size handling exists for calloc/realloc paths.
- Big allocations are tracked separately in `BIG_METADATA_MAP` and can use the NUMA-aware 4 MiB to 64 MiB `BUDDY_BACKEND` cache with old-block trimming and optional memory-pressure relief.
- Transparent huge page attempts are configurable for big allocation regions, and the slab page backend has an opt-in `page-backend-no-huge-page` feature for systems that aggressively promote THP and inflate RSS.
- Preload builds provide C ABI allocation entry points including `malloc`, `calloc`, `realloc`, `reallocarray`, `recallocarray`, `free`, sized-free compatibility shims, usable-size queries, alignment APIs, and opt-in `malloc_trim(...)` support.
- Trimming supports buddy-cache blocks and small-allocation/background trim scanning for size classes equal to or greater than 4096 bytes.
- Non-preload builds expose `RSMalloc`, `RSMallocConfig`, and `GlobalAlloc` integration.
- Runtime tuning is available for refill behavior, predictor behavior, THP behavior, buddy cache sizing, trim-thread behavior, memory-pressure relief, trimming, magic-value behavior, and foreign-pointer handling in global-allocator mode.
- Small-allocation refill sizing uses a fast integer adaptive predictor, with a separate bulk-fill predictor so cache-pop/steal behavior does not force page/list initialization into tiny batches.
- In the default thread-local refill path, pending refill metadata is drained on thread exit into a lock-free per-node global pending queue to reduce stranded per-thread pending slabs.
- Optional `extended-header` Cargo feature provides wider allocator metadata for experiments and stress testing.
- Non-preload builds expose a small capability snapshot with allocator version, configured THP state, and current public NUMA support status.
- Internal allocation paths are NUMA-aware where topology is available: transfer-cache victim stealing, slab page-backend arenas, pending metadata reuse, buddy backend regions, and direct big mappings prefer the current CPU's node.
- Batch transfer-cache stealing uses relaxed per-class nonempty CPU hints to narrow victim selection before falling back to the actual ABA-tagged transfer-list pop path.

## Adaptive Refill Prediction

Small-allocation cache refills use a fast integer adaptive predictor to estimate current per-size-class refill demand. When a refill returns exactly the requested batch and the class still has headroom, observed demand is slightly uplifted (+25%, clamped) so the next refill can grow quickly under pressure. Sustained low-demand samples shrink the predicted batch gradually to avoid oscillating on one-off dips.

The initial predictor batch is configurable through `RefillPredictorSettings`/`with_refill_predictor_settings` in the Rust API and `RS_PREDICTOR_INIT_BATCH` in preload builds. A separate bulk-fill predictor is used so page/list initialization can still happen in practical batches even when cache-pop or steal behavior observes smaller short-term demand. Bulk fill initializes headers only for the selected batch and keeps remaining mapped metadata as pending refill state.

## Current Limitations

- **This is alpha-quality allocator software with limited test coverage.**
- The crate currently requires nightly Rust features and `rustc 1.96.0` or higher.
- The preload path and the Rust `GlobalAlloc` path are still being separated and stabilized.
- Big allocation metadata still uses an internal hashmap, which is planned for replacement.
- Runtime behavior under every libc, loader, and fork/preload combination has not been fully audited.
- Optional extended-header metadata is a debugging/diagnostic aid, not a replacement for memory-safety tooling or a high-security sandbox.
- Documentation inside the allocator internals is incomplete.
- Benchmarks are used as development signals, but they are not the design target and are not yet authoritative enough to make stable performance claims.
- The public Rust API is still subject to change before a stable release.

## Using As A Rust Global Allocator

The non-preload library path exposes `RSMalloc` as a `GlobalAlloc` implementation.

```rust
use rsmalloc::RSMalloc;

#[global_allocator]
static GLOBAL: RSMalloc = RSMalloc::new_default();
```

For explicit runtime configuration:

```rust
use rsmalloc::{
    BuddyTHP, ExperimentalFeatures, ForeignPointerSettings, PerCacheLimit, Percentage,
    RefillPredictorSettings, ReliefSettings, ReliefState, RSMalloc, RSMallocConfig, THP,
    THPSettings,
};

const CONFIG: RSMallocConfig = RSMallocConfig::DEFAULT
    .with_thp_settings(THPSettings::new(THP::Enabled, BuddyTHP::Force))
    .with_refill_predictor_settings(RefillPredictorSettings::new(16))
    .with_max_refill_retries(4)
    .with_max_per_buddy_cache(PerCacheLimit::Bytes(512 * 1024 * 1024))
    .with_relief_settings(ReliefSettings::new(
        ReliefState::Enabled,
        Percentage::new(85),
        Percentage::new(80),
    ))
    .with_experimental_features(ExperimentalFeatures::DEFAULT)
    .with_foreign_pointer(ForeignPointerSettings::DEFAULT);

#[global_allocator]
static GLOBAL: RSMalloc = RSMalloc::new_with_config(CONFIG);
```

`RSMallocConfig` groups allocator tuning into THP settings, adaptive refill-predictor settings, magic-value safety behavior, foreign-pointer behavior, buddy-cache sizing, slab page-arena minimum sizing, memory-pressure relief behavior, refill retry limits, and alpha feature flags. The default configuration keeps randomized magic values enabled, aborts on foreign pointers in Rust global-allocator mode, enables general THP support, leaves buddy THP forcing disabled, uses the default buddy cache limit, uses a 256 MiB minimum slab page-arena size, leaves memory-pressure relief disabled by default, and starts the refill predictor with allocator defaults.

`THPSettings` uses explicit enums instead of raw booleans: `THP::Enabled` or `THP::Disabled` for general THP behavior, and `BuddyTHP::Disabled` or `BuddyTHP::Force` for buddy-region huge-page requests.

The buddy cache limit is configured with `PerCacheLimit`. `PerCacheLimit::Default` uses the allocator default, while `PerCacheLimit::Bytes(...)` requests an explicit byte limit that is rounded up to a power of two during initialization.

The slab page-backend arena minimum is configured through `RSMallocConfig::arena_min_size` using `Bytes(...)`. Its default is 256 MiB. This is a minimum reserved virtual data size rather than a committed-RSS limit: an arena grows to fit a larger refill request, the selected size is page-aligned internally, and physical pages are populated as refill memory is touched.

Memory-pressure relief is configured with `ReliefSettings` and is disabled by default. When enabled, the background worker periodically samples system-wide memory pressure. If usage rises above the configured disable threshold, the buddy backend is temporarily disabled and future large allocations are served directly with `mmap`/`munmap` instead of being cached. Once memory pressure remains below the configured re-enable threshold for repeated samples, the buddy backend is re-enabled. This can significantly increase allocation overhead, but may prevent allocator-side caching from worsening OOM-prone workloads. It is intended for memory-constrained or burst-heavy applications rather than maximum-throughput runs.

Rust-mode trimming is available through `RSMalloc::rs_trim(...)`. Use `RSMallocTrim::Request(bytes)` or `RSMallocTrim::All` to ask the buddy backend and eligible small-allocation caches to return cold pages to the kernel. The method returns `RSTrimStatus::Trimmed(bytes)` today; the `Disabled` and `NothingToTrim` variants are reserved for API compatibility as the trim policy evolves. Helper methods include `get_trim_size()`, `succeeded()`, and `disabled()`.

Magic-value behavior can be weakened for debugging, reproducible tests, security research, or allocator experiments. These modes require explicit unsafe acknowledgement:

```rust
use rsmalloc::{
    DisableMagic, MagicSafety, MagicSafetyDisable, RSMalloc, RSMallocConfig,
};

const FIXED_MAGIC: RSMallocConfig = RSMallocConfig::DEFAULT.with_magic_safety(
    MagicSafety::FixedMagic(unsafe {
        MagicSafetyDisable::acknowledge_safety_risk()
    }),
);

const DISABLE_MAGIC_CHECKS: RSMallocConfig = RSMallocConfig::DEFAULT.with_magic_safety(
    MagicSafety::Disabled(unsafe {
        DisableMagic::acknowledge_safety_risk()
    }),
);

#[global_allocator]
static GLOBAL: RSMalloc = RSMalloc::new_with_config(FIXED_MAGIC);
```

`RSMalloc` also exposes Rust-facing low-level helper methods such as `rs_malloc`, `rs_calloc`, `rs_memalign`, `rs_realloc`, `rs_trim`, `rs_free`, and `rs_usable_size` in non-preload builds. These helpers should only be used with pointers returned by `RSMalloc` where pointer ownership applies.

Capability information is available without allocation:

```rust
let caps = GLOBAL.get_capabilities();
```

The capability snapshot reports the allocator version, whether THP is enabled by the current config, and NUMA support status. In `0.2.0-alpha`, NUMA support is reported as `NumaSupport::Partial`: internal allocation paths use NUMA-aware placement when topology is available, but the public capability surface does not yet promise full NUMA policy control.

## Building For Preload

The preload ABI is behind the `preload` feature.

```sh
cargo build --release --features preload
```

The generated shared object is intended for LD_PRELOAD-style testing. Preload-specific fallback, libc, fork, and errno handling are compiled only in this mode.

Preload builds currently expose these C ABI symbols: `malloc`, `malloc_usable_size`, `malloc_trim`, `calloc`, `free`, `free_sized`, `free_aligned_sized`, `realloc`, `reallocarray`, `recallocarray`, `posix_memalign`, `memalign`, `aligned_alloc`, `valloc`, and `pvalloc`.

## Optional Cargo Features

For allocator experiments and stress testing, rsmalloc can use wider header metadata:

```sh
cargo build --release --features extended-header
```

- `extended-header` uses wider header metadata and implies a larger per-allocation header.

Other optional Cargo features:

- `page-backend-no-huge-page` applies `MADV_NOHUGEPAGE`/`Advice::LinuxNoHugepage` to slab page-backend arenas. This is useful on systems such as CachyOS or other kernels/configurations that aggressively promote transparent huge pages for allocator arenas: it can significantly reduce apparent RSS, at the cost of higher TLB pressure. If both slab page-backend THP advice features are enabled, no explicit page-backend THP advice is applied.
- `page-backend-huge-page` applies huge-page advice to slab page-backend arenas when `page-backend-no-huge-page` is not enabled. This is a TLB/RSS tradeoff knob; do not enable it on systems where THP promotion already inflates RSS.
- `check-owned-on-alloc` enables an opt-in semi-hardening ownership check that verifies popped allocation pointers are still owned by `RADIX` before they are returned to callers. This can catch some corrupted freelist/transfer-cache metadata earlier, but it is not a full integrity proof and adds an ownership-map lookup to allocation paths.
- `lazy-page-trim` uses lazy page-free advice for small-allocation trim where supported instead of immediate `MADV_DONTNEED`-style advice.
- `print-cpu-on-double-free` includes the current RSEQ CPU id in fatal double-free/corruption reports when available.

### Alpha-2 debug modes

`0.2.0-alpha` has several explicit debug feature tiers. These modes are intentionally split because some are useful for routine allocator visibility while others add significant measurement overhead:

- `debug`: base internal counters, including RSEQ/refill debug counters used by reports and stats.
- `debug-print`: enables `debug` and prints an allocator report at process exit via `.fini_array`/`eprintln!`.
- `debug-printer-thread`: enables `debug-print` and starts a background printer thread for live allocator-state snapshots.
- `debug-exact`: enables `debug-print` and adds higher-overhead lock counters such as lock calls, retries, try-lock misses, and spin waits.
- `debug-predictor-exact`: enables `debug-print` and uses more intrusive refill-prediction accounting to distinguish over/under prediction behavior.
- `predictor-debug`: logs predictor batch decisions with `eprintln!` from the predictor path.
- `transfer-debug`: enables `debug-exact` and tracks transfer-cache steals, dry steals, and CAS retries.
- `transfer-debug-exact`: enables `transfer-debug` and also counts transfer-cache push/pop calls.
- `debug-full`: convenience feature for broad transfer/debug instrumentation.
- `debug-full-critic`: enables broad debug instrumentation plus exact predictor diagnostics.

Use semi-hardening and exact/transfer/predictor debug modes only when diagnosing allocator behavior or corruption; they can materially change benchmark results.

## Runtime Configuration For Preload

- `RS_ARENA_SIZE`: Minimum slab page-backend arena data size in bytes. Defaults to `268435456` (256 MiB). Actual arenas are page-aligned and may be larger when required by a refill request; the reservation does not imply that the entire arena is resident.
- `RS_PREDICTOR_INIT_BATCH`: Initial per-size-class predictor batch value for small allocation refills. Defaults to `128`.
- `RS_MAX_REFILL_RETRIES`: Maximum number of refill retries. Defaults to `3`.
- `RS_BUDDY_PER_CACHE_SIZE`: Initial buddy backend region size for big allocations. Defaults to `268435456` bytes, is clamped to at least `268435456`, and is rounded up to a power of two.
- `RS_BUDDY_ATTEMPT_HUGEPAGE`: Set to `1` to request transparent huge pages for buddy backend regions.
- `RS_DISABLE_TRIM_THREAD`: Set to nonzero to disable the background trim worker. Manual `malloc_trim(...)` remains available.
- `RS_TRIMMER_THRESHOLD`: Minimum cached virtual address space before starting the background trim worker. Defaults to `10485760` bytes.
- `RS_ENABLE_RELIEF`: Controls system-memory-pressure relief behavior in the current alpha preload path. Relief is disabled by default; set this to `0` to enable it.
- `RS_BUDDY_RELIEF_DISABLE_PERCENTAGE`: System memory usage percentage at or above which the buddy backend is disabled and buddy trim is forced when relief is enabled. Defaults to `85`.
- `RS_BUDDY_RELIEF_ENABLE_PERCENTAGE`: System memory usage percentage at or below which the buddy backend may be re-enabled after repeated low-pressure samples. Defaults to `80` and is clamped to the disable percentage.
- `RS_DISABLE_THP`: Set to `1` to disable transparent huge page attempts.
- `RS_DISABLE_RANDOMIZING`: Set to `1` to keep fixed built-in magic values instead of randomizing them at bootstrap.

## Architecture

For a fuller architecture walkthrough, see [`architecture.md`](architecture.md).

The allocator is organized into a few main areas:

- `abi`: C ABI entry points for preload builds.
- `global_alloc`: Rust `GlobalAlloc` integration and direct Rust-facing allocation helpers.
- `core_prim`: bootstrap, predictor state, fork handling, and pointer wrappers.
- `inner`: allocator operation implementations such as allocation, free, calloc, realloc, and alignment.
- `big_allocations`: big allocation path and `BUDDY_BACKEND` implementation.
- `internals`: internal data structures including `BIG_METADATA_MAP`, `RADIX` ownership tracking, NUMA parsing/binding helpers, locks, and once primitives.
- `rseq_core`: `SLAB_CACHE` structures, transfer caches, inline assembly critical sections, bulk-fill metadata, and pending refill queues.
- `utility`: size classes and shared allocation helpers.

## Design Notes

rsmalloc treats the small allocation fast path as a per-CPU cache problem. RSEQ lets the allocator update CPU-local linked lists without normal lock overhead when the current CPU remains stable through the critical section. If the kernel preempts or migrates the thread during that critical section, the operation is aborted and retried or moved to a fallback path.

Big allocations do not use the same slab path. They are tracked separately in `BIG_METADATA_MAP`, can be mapped directly, and can be served from the NUMA-aware `BUDDY_BACKEND` cache for eligible sizes.

### Slab page backend and RSS on aggressive THP systems

`0.2.0-alpha` uses a hybrid slab page backend for refill memory instead of mapping each refill span independently. Each NUMA node gets page arenas with a configurable 256 MiB default minimum; allocation tries cheap bump allocation first, then bitmap-tracked reusable page runs, then maps a new arena if needed. Rust configurations select the minimum with `RSMallocConfig::arena_min_size`, while preload builds use `RS_ARENA_SIZE` in bytes. An arena may exceed the configured minimum when a refill request itself is larger. This reduces `mmap` call count, VMA churn, and scattered refill mappings while keeping refill memory NUMA-local.

The backend reserves virtual arena space, but `bulk_fill()` still initializes headers lazily and `RADIX` marks only the allocated metadata span, not the whole arena. In practice this can reduce RSS even when cached/reserved virtual address space increases. Some systems aggressively promote these arenas to transparent huge pages, though; on those systems, RSS can look much higher than expected when arena slack is backed by huge pages rather than remaining cheap virtual space.

If that happens, build with:

```sh
cargo build --release --features page-backend-no-huge-page
```

This asks Linux not to back slab page-backend arenas with huge pages. It can significantly reduce RSS on THP-aggressive systems, at the cost of higher TLB pressure. If your workload is TLB-sensitive and RSS is fine, leave it disabled. The opposite `page-backend-huge-page` feature requests huge-page advice for page-backend arenas when `page-backend-no-huge-page` is not also enabled.

## Contributing

See [CONTRIBUTING.md](CONTRIBUTING.md) for details on how to contribute to rsmalloc.
