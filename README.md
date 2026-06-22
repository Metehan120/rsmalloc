# RSMalloc (rseq/rust slab malloc)

rsmalloc is an experimental Rust memory allocator focused on low-overhead concurrent allocation for real-world application workloads, not benchmark-only allocation patterns. The small-allocation path is built around Linux Restartable Sequences (RSEQ), so cache ownership is CPU-oriented rather than thread-oriented. Larger allocations use a separate big-allocation path with a buddy allocator for cached regions.

Current release line: `0.1.0-alpha`.

crates.io: https://crates.io/crates/rsmalloc

See [UPDATES.md](UPDATES.md) for release notes and [TODO.md](TODO.md) for planned work.
See [benchmarks/benchmarks.md](benchmarks/benchmarks.md) for benchmark results.

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
- Big allocations are tracked separately and can use a 4 MiB to 64 MiB buddy allocator cache.
- Transparent huge page attempts are configurable for big allocation regions.
- Preload builds provide C ABI allocation entry points including `malloc`, `calloc`, `realloc`, `reallocarray`, `recallocarray`, `free`, sized-free compatibility shims, usable-size queries, alignment APIs, and opt-in `malloc_trim(...)` support.
- Non-preload builds expose `RSMalloc`, `RSMallocConfig`, and `GlobalAlloc` integration.
- Runtime tuning is available for refill behavior, predictor behavior, THP behavior, buddy cache sizing, opt-in experimental buddy trimming, magic-value behavior, and foreign-pointer handling in global-allocator mode.
- Small-allocation refill sizing uses an EMA predictor, with a separate bulk-fill predictor so cache-pop/steal behavior does not force page/list initialization into tiny batches.
- In the default thread-local refill path, pending refill metadata is drained on thread exit into the current CPU mail cache to reduce stranded per-thread pending slabs; this can be disabled with `disable-thread-pending`.
- Optional `canary` and `extended-header` Cargo features provide lightweight higher-security metadata checks for allocator experiments and stress testing.
- Non-preload builds expose a small capability snapshot with allocator version, configured THP state, and current NUMA support status.

## Important Fixes

The `0.1.0-alpha` line includes several high-impact fixes over the earlier alpha/pre-alpha internals:

- Fixed RSEQ cache usage accounting by switching the hot-path cache counter into an approximate pressure signal that avoids stale-high performance cliffs. If the counter drifts stale-low, later cache activity can recover the pressure signal; this tradeoff reduced unnecessary mailbox traffic in worst-case RSEQ stress benchmarks and improved the checked-in `sh6benchN`/`sh8benchN` snapshot.
- Fixed refill predictor startup so the configured initial batch is applied before the first `batch()` read and the EMA is seeded from that value. Before this, the first refill could request one object, observe one object of demand, and keep the predictor pinned near one.
- Fixed observed-demand learning in refill paths: when refill returns exactly the requested batch and the class is not at max, observed demand is uplifted by +25% (clamped) before EMA update so the predictor can climb back faster after sustained/full-batch pressure.
- Added a separate bulk-fill predictor path, allowing `bulk_fill` to initialize larger batches when the size class allows it instead of sharing the cache-pop predictor's smaller batch behavior.
- Added ABA-tagged mail-cache pointers to make concurrent mail-cache push/pop behavior safer.
- Added raw RSEQ registration fallback and fork-child RSEQ re-registration for environments where libc TLS RSEQ symbols are unavailable or need to be restored after fork.
- Added default RSEQ thread-failure fallback handling for invalid/unregistered CPU IDs, routing recovery through the overflow mail slot instead of indexing per-CPU state directly.
- Fixed low-level RSEQ registration syscall clobber handling on x86-64.
- Fixed big-allocation metadata/ownership handling, including usable-size lookup by original payload pointer and full-range radix tracking for aligned big mappings.
- Updated radix ownership tracking to cover the low canonical 56-bit user address range used by x86-64 LA57 systems, with checked range marking instead of modulo-wrapped indices.
- Fixed `realloc` bookkeeping for small slab remap paths, direct big-allocation in-place remap paths, and buddy-backed big allocation growth paths.
- Added checked size arithmetic in big-allocation requests so oversized allocations fail cleanly instead of wrapping.

These fixes do not make rsmalloc production-ready; they mainly make the alpha release more correct, less fragile under stress workloads, and easier to evaluate.

## EMA Refill Prediction

Small-allocation cache refills use an exponential moving average (EMA) predictor to smooth recent allocation activity and estimate current per-size-class demand. This lets refill batch sizes adapt to workload pressure without reacting too sharply to one-off bursts. When a refill returns exactly the requested batch and the class still has headroom, observed demand is slightly uplifted (+25%, clamped) before EMA update to improve burst recovery.

The predictor is configurable through `EMASettings`, `EmaAlpha`, `RS_EMA_ALPHA`, and `RS_PREDICTOR_INIT_BATCH`. A separate bulk-fill predictor is used so page/list initialization can still happen in practical batches even when cache-pop or steal behavior observes smaller short-term demand.

## Current Limitations

- **This is alpha-quality experimental software with limited test coverage.**
- The crate currently requires nightly Rust features and `rustc 1.96.0` or higher.
- The preload path and the Rust `GlobalAlloc` path are still being separated and stabilized.
- Big allocation metadata still uses an internal hashmap, which is planned for replacement.
- Buddy-cache trimming is available through `malloc_trim(...)` in preload mode when `RS_ENABLE_TRIM=1`, and through opt-in `RSMalloc::rs_trim_buddy(RSMallocTrim::...)` in Rust mode. Full small-allocation/background trimming is not implemented yet.
- Runtime behavior under every libc, loader, and fork/preload combination has not been fully audited.
- Optional canary/extended-header checks are defense-in-depth diagnostics, not a replacement for memory-safety tooling or a high-security sandbox.
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
    PerCacheLimit, RSMalloc, RSMallocConfig, THP, THPSettings,
};

const CONFIG: RSMallocConfig = RSMallocConfig::DEFAULT
    .with_thp_settings(THPSettings::new(THP::Enabled, BuddyTHP::Force))
    .with_ema_settings(EMASettings::new(EmaAlpha::Fast, 16))
    .with_max_refill_retries(4)
    .with_max_per_buddy_cache(PerCacheLimit::Bytes(512 * 1024 * 1024))
    .with_experimental_features(ExperimentalFeatures::DEFAULT.with_buddy_trim())
    .with_foreign_pointer(ForeignPointerSettings::DEFAULT);

#[global_allocator]
static GLOBAL: RSMalloc = RSMalloc::new_with_config(CONFIG);
```

`RSMallocConfig` groups allocator tuning into THP settings, EMA refill-predictor settings, magic-value safety behavior, foreign-pointer behavior, buddy-cache sizing, refill retry limits, and experimental feature flags. The default configuration keeps randomized magic values enabled, aborts on foreign pointers in Rust global-allocator mode, enables general THP support, leaves buddy THP forcing disabled, uses the default buddy cache limit, and starts the refill predictor with allocator defaults.

`THPSettings` uses explicit enums instead of raw booleans: `THP::Enabled` or `THP::Disabled` for general THP behavior, and `BuddyTHP::Disabled` or `BuddyTHP::Force` for buddy-region huge-page requests.

The buddy cache limit is configured with `PerCacheLimit`. `PerCacheLimit::Default` uses the allocator default, while `PerCacheLimit::Bytes(...)` requests an explicit byte limit that is rounded up to a power of two during initialization.

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

The capability snapshot reports the allocator version, whether THP is enabled by the current config, and whether NUMA support is currently available. NUMA support is currently reported as `false`.

## Building For Preload

The preload ABI is behind the `preload` feature.

```sh
cargo build --release --features preload
```

The generated shared object is intended for LD_PRELOAD-style testing. Preload-specific fallback, libc, fork, and errno handling are compiled only in this mode.

Preload builds currently expose these C ABI symbols: `malloc`, `malloc_usable_size`, `malloc_trim`, `calloc`, `free`, `free_sized`, `free_aligned_sized`, `realloc`, `reallocarray`, `recallocarray`, `posix_memalign`, `memalign`, `aligned_alloc`, `valloc`, and `pvalloc`.

## Optional Hardening Features

For allocator experiments and stress testing, rsmalloc provides optional higher-security metadata checks:

```sh
cargo build --release --features canary
cargo build --release --features extended-header
```

- `canary` enables lightweight header canary checks without changing the default release feature set.
- `extended-header` uses wider header metadata for stronger diagnostic checks and implies a larger per-allocation header.

These features are intended as defense-in-depth diagnostics for catching allocator metadata corruption earlier. They should not be treated as high-security isolation or as a substitute for tools such as sanitizers, fuzzing, or Miri where applicable.

Other optional Cargo features:

- `rseq-thread-failure-fallback` enables the default recovery path for invalid/unregistered RSEQ CPU IDs.
- `legacy-glibc-support` enables the raw RSEQ fallback path for environments where libc RSEQ TLS symbols are unavailable.
- `predictor-debug` enables refill-predictor debug logging.
- `debug-predictor-exact` enables exact refill-mispredict accounting instrumentation (higher overhead than normal debug mode).
- `cpu-refill-paths` enables the per-CPU pending-refill metadata path (experimental).
- `disable-thread-pending` disables default thread-exit draining of thread-local pending refill metadata.

## Runtime Configuration For Preload

- `RS_PREDICTOR_INIT_BATCH`: Initial per-size-class predictor batch value for small allocation refills. Defaults to `128`.
- `RS_EMA_ALPHA`: Exponential moving average alpha value for the refill predictor. Defaults to `0.15`.
- `RS_MAX_REFILL_RETRIES`: Maximum number of refill retries. Defaults to `3`.
- `RS_BUDDY_PER_CACHE_SIZE`: Initial buddy allocator region size for big allocations. Defaults to `268435456` bytes and is rounded up to a power of two.
- `RS_BUDDY_ATTEMPT_HUGEPAGE`: Set to `1` to request transparent huge pages for buddy allocator regions.
- `RS_ENABLE_TRIM`: Set to `1` to enable `malloc_trim(...)` for the buddy allocator. Disabled by default.
- `RS_DISABLE_THP`: Set to `1` to disable transparent huge page attempts.
- `RS_DISABLE_RANDOMIZING`: Set to `1` to keep fixed built-in magic values instead of randomizing them at bootstrap.

## Architecture

For a fuller architecture walkthrough, see [`architecture.md`](architecture.md).

The allocator is organized into a few main areas:

- `abi`: C ABI entry points for preload builds.
- `global_alloc`: Rust `GlobalAlloc` integration and direct Rust-facing allocation helpers.
- `core_prim`: bootstrap, RSEQ registration, predictor state, fork handling, and pointer wrappers.
- `inner`: allocator operation implementations such as allocation, free, calloc, realloc, and alignment.
- `big_allocations`: big allocation path and buddy allocator.
- `internals`: internal data structures including the big allocation map, radix ownership tracking, locks, and once primitives.
- `rseq_core`: RSEQ cache structures and inline assembly critical sections.
- `utility`: size classes and shared allocation helpers.

## Design Notes

rsmalloc treats the small allocation fast path as a per-CPU cache problem. RSEQ lets the allocator update CPU-local linked lists without normal lock overhead when the current CPU remains stable through the critical section. If the kernel preempts or migrates the thread during that critical section, the operation is aborted and retried or moved to a fallback path.

Big allocations do not use the same slab path. They are tracked separately, can be mapped directly, and can be served from a buddy allocator cache for eligible sizes.

## Contributing

See [CONTRIBUTING.md](CONTRIBUTING.md) for details on how to contribute to rsmalloc.
