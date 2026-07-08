# RSMalloc (rseq/rust slab malloc)

rsmalloc is an experimental Rust memory allocator focused on low-overhead concurrent allocation for real-world application workloads, not benchmark-only allocation patterns. The small-allocation path is built around Linux Restartable Sequences (RSEQ), so cache ownership is CPU-oriented rather than thread-oriented. Larger allocations use a separate big-allocation path with a buddy allocator for cached regions.

Current release line: `0.2.0-alpha`.

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

- Small allocations are served from size classes backed by per-CPU RSEQ caches.
- RSEQ fast paths use inline assembly critical sections for push/pop operations.
- Overflow and zero-size handling exists for calloc/realloc paths.
- Big allocations are tracked separately and can use a NUMA-aware 4 MiB to 64 MiB buddy allocator cache.
- Transparent huge page attempts are configurable for big allocation regions.
- Preload builds provide C ABI allocation entry points including `malloc`, `calloc`, `realloc`, `reallocarray`, `recallocarray`, `free`, sized-free compatibility shims, usable-size queries, alignment APIs, and opt-in `malloc_trim(...)` support.
- Trimming supports buddy-cache blocks and small-allocation/background trim scanning for size classes equal to or greater than 4096 bytes.
- Non-preload builds expose `RSMalloc`, `RSMallocConfig`, and `GlobalAlloc` integration.
- Runtime tuning is available for refill behavior, predictor behavior, THP behavior, buddy cache sizing, opt-in experimental buddy trimming, magic-value behavior, and foreign-pointer handling in global-allocator mode.
- Small-allocation refill sizing uses an EMA predictor, with a separate bulk-fill predictor so cache-pop/steal behavior does not force page/list initialization into tiny batches.
- In the default thread-local refill path, pending refill metadata is drained on thread exit into a lock-free per-node global pending queue to reduce stranded per-thread pending slabs.
- Optional `extended-header` Cargo feature provides wider allocator metadata for experiments and stress testing.
- Non-preload builds expose a small capability snapshot with allocator version, configured THP state, and current public NUMA support status.
- Internal allocation paths are NUMA-aware where topology is available: RSEQ victim stealing, small refill mappings, pending metadata reuse, buddy regions, and direct big mappings prefer the current CPU's node.

## EMA Refill Prediction

Small-allocation cache refills use an exponential moving average (EMA) predictor to smooth recent allocation activity and estimate current per-size-class demand. This lets refill batch sizes adapt to workload pressure without reacting too sharply to one-off bursts. When a refill returns exactly the requested batch and the class still has headroom, observed demand is slightly uplifted (+25%, clamped) before EMA update to improve burst recovery.

The predictor is configurable through `EMASettings`, `EmaAlpha`, `RS_EMA_ALPHA`, and `RS_PREDICTOR_INIT_BATCH`. A separate bulk-fill predictor is used so page/list initialization can still happen in practical batches even when cache-pop or steal behavior observes smaller short-term demand.

## Current Limitations

- **This is alpha-quality experimental software with limited test coverage.**
- The crate currently requires nightly Rust features and `rustc 1.96.0` or higher.
- The preload path and the Rust `GlobalAlloc` path are still being separated and stabilized.
- Big allocation metadata still uses an internal hashmap, which is planned for replacement.
- Runtime behavior under every libc, loader, and fork/preload combination has not been fully audited.
- Optional extended-header metadata is experimental and not a replacement for memory-safety tooling or a high-security sandbox.
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
    BuddyTHP, EMASettings, EmaAlpha, ExperimentalFeatures, ForeignPointerSettings,
    PerCacheLimit, Percentage, ReliefSettings, ReliefState, RSMalloc, RSMallocConfig, THP,
    THPSettings,
};

const CONFIG: RSMallocConfig = RSMallocConfig::DEFAULT
    .with_thp_settings(THPSettings::new(THP::Enabled, BuddyTHP::Force))
    .with_ema_settings(EMASettings::new(EmaAlpha::Fast, 16))
    .with_max_refill_retries(4)
    .with_max_per_buddy_cache(PerCacheLimit::Bytes(512 * 1024 * 1024))
    .with_relief_settings(ReliefSettings::new(
        ReliefState::Enabled,
        Percentage::new(85),
        Percentage::new(80),
    ))
    .with_experimental_features(ExperimentalFeatures::DEFAULT.with_buddy_trim())
    .with_foreign_pointer(ForeignPointerSettings::DEFAULT);

#[global_allocator]
static GLOBAL: RSMalloc = RSMalloc::new_with_config(CONFIG);
```

`RSMallocConfig` groups allocator tuning into THP settings, EMA refill-predictor settings, magic-value safety behavior, foreign-pointer behavior, buddy-cache sizing, memory-pressure relief behavior, refill retry limits, and experimental feature flags. The default configuration keeps randomized magic values enabled, aborts on foreign pointers in Rust global-allocator mode, enables general THP support, leaves buddy THP forcing disabled, uses the default buddy cache limit, leaves memory-pressure relief disabled by default, and starts the refill predictor with allocator defaults.

`THPSettings` uses explicit enums instead of raw booleans: `THP::Enabled` or `THP::Disabled` for general THP behavior, and `BuddyTHP::Disabled` or `BuddyTHP::Force` for buddy-region huge-page requests.

The buddy cache limit is configured with `PerCacheLimit`. `PerCacheLimit::Default` uses the allocator default, while `PerCacheLimit::Bytes(...)` requests an explicit byte limit that is rounded up to a power of two during initialization.

Memory-pressure relief is configured with `ReliefSettings` and is disabled by default. When enabled, the background worker periodically samples system-wide memory pressure. If usage rises above the configured disable threshold, the buddy backend is temporarily disabled and future large allocations are served directly with `mmap`/`munmap` instead of being cached. Once memory pressure remains below the configured re-enable threshold for repeated samples, the buddy backend is re-enabled. This can significantly increase allocation overhead, but may prevent allocator-side caching from worsening OOM-prone workloads. It is intended for memory-constrained or burst-heavy applications rather than maximum-throughput runs.

Rust-mode buddy trimming is experimental and disabled by default. Enable it with `ExperimentalFeatures::DEFAULT.with_buddy_trim()`, then call `RSMalloc::rs_trim_buddy(...)` with `RSMallocTrim::Request(bytes)` or `RSMallocTrim::All`. The method returns `RSTrimStatus::Disabled`, `RSTrimStatus::NothingToTrim`, or `RSTrimStatus::Trimmed(bytes)`; helper methods include `get_trim_size()`, `succeeded()`, and `disabled()`.

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

`RSMalloc` also exposes Rust-facing low-level helper methods such as `rs_malloc`, `rs_calloc`, `rs_memalign`, `rs_realloc`, `rs_trim_buddy`, `rs_free`, and `rs_usable_size` in non-preload builds. These helpers should only be used with pointers returned by `RSMalloc` where pointer ownership applies.

Capability information is available without allocation:

```rust
let caps = GLOBAL.get_capabilities();
```

The capability snapshot reports the allocator version, whether THP is enabled by the current config, and whether NUMA support is exposed through the public capability surface. The capability field currently remains `false` even though internal allocation paths use NUMA-aware placement when topology is available.

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

- `rseq-thread-failure-fallback` enables the default recovery path for invalid/unregistered RSEQ CPU IDs.
- `predictor-debug` enables refill-predictor debug logging.
- `debug-predictor-exact` enables exact refill-mispredict accounting instrumentation (higher overhead than normal debug mode).
- `lazy-page-trim` uses lazy page-free advice for small-allocation trim where supported instead of immediate `MADV_DONTNEED`-style advice.

## Runtime Configuration For Preload

- `RS_PREDICTOR_INIT_BATCH`: Initial per-size-class predictor batch value for small allocation refills. Defaults to `128`.
- `RS_EMA_ALPHA`: Exponential moving average alpha value for the refill predictor. Defaults to `0.15` and is clamped to `0.05..=0.25`.
- `RS_MAX_REFILL_RETRIES`: Maximum number of refill retries. Defaults to `3`.
- `RS_BUDDY_PER_CACHE_SIZE`: Initial buddy allocator region size for big allocations. Defaults to `268435456` bytes, is clamped to at least `268435456`, and is rounded up to a power of two.
- `RS_BUDDY_ATTEMPT_HUGEPAGE`: Set to `1` to request transparent huge pages for buddy allocator regions.
- `RS_DISABLE_TRIM_THREAD`: Set to nonzero to disable the background trim worker. Manual `malloc_trim(...)` remains available.
- `RS_TRIMMER_THRESHOLD`: Minimum cached virtual address space before starting the background trim worker. Defaults to `10485760` bytes.
- `RS_DISABLE_RELIEF`: Controls system-memory-pressure relief behavior. Relief is disabled by default; set this to `0` to enable it.
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
- `big_allocations`: big allocation path and buddy allocator.
- `internals`: internal data structures including the big allocation map, radix ownership tracking, NUMA parsing/binding helpers, locks, and once primitives.
- `rseq_core`: RSEQ cache structures, inline assembly critical sections, bulk-fill metadata, and pending refill queues.
- `utility`: size classes and shared allocation helpers.

## Design Notes

rsmalloc treats the small allocation fast path as a per-CPU cache problem. RSEQ lets the allocator update CPU-local linked lists without normal lock overhead when the current CPU remains stable through the critical section. If the kernel preempts or migrates the thread during that critical section, the operation is aborted and retried or moved to a fallback path.

Big allocations do not use the same slab path. They are tracked separately, can be mapped directly, and can be served from a NUMA-aware buddy allocator cache for eligible sizes.

## Contributing

See [CONTRIBUTING.md](CONTRIBUTING.md) for details on how to contribute to rsmalloc.
