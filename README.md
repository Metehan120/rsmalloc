# <img src="img/logo.png" alt="" width="64" height="64" align="absmiddle"> RSMalloc (Rust/RSEQ Slab memory allocator)

An RSEQ-based memory allocator for Rust and C applications on Linux `x86_64`. RSMalloc focuses on low-overhead concurrent allocation for real workloads: small-allocation caches belong to CPUs, not threads, and reusable blocks can move between CPUs as demand changes. Larger allocations use a NUMA-aware segmented-bitmap backend or direct mappings.

**Status: `0.3.0-alpha`. Alpha-quality software — not production-ready.** Performance and memory retention are workload-dependent. See [Status & Limitations](#status--limitations) before using it outside development.

[crates.io](https://crates.io/crates/rsmalloc) · [User and Performance Guide](book/src/introduction.md) · [Architecture](ARCHITECTURE.md) · [Release Notes](UPDATES.md) · [Roadmap](ROADMAP.md) · [Todo](TODO.md) · [Benchmarks](benchmarks/benchmarks.md) · [Contributing](CONTRIBUTING.md)

The [mdBook guide](book/src/introduction.md) explains the allocation lifecycle, safe integration, tuning, and [where RSMalloc can help or fall behind](book/src/workload-fit.md). Build it locally with `mdbook build book`, or preview it with `mdbook serve book`.

> **Alpha-3 ownership change:** By default, non-null pointers passed to `free` must be live RSMalloc allocations. Foreign-pointer handling is best-effort because metadata is read before ownership is checked. Enable `validate-foreign-first` for ownership checks before those reads; it is also included in `semi-hardened`. Neither mode makes arbitrary or interior pointers safe to free. See [Free ownership assumptions](#free-ownership-assumptions).

**Major milestone:** The default Rust `GlobalAlloc` and C `LD_PRELOAD` configurations build on stable Rust. RSMalloc no longer keeps refill metadata or adaptive batching in allocator-owned thread-local storage: both are maintained per CPU. It still uses libc's RSEQ TLS state. Only the optional `allocator-api` interface requires nightly Rust.

## Quick Start

### Requirements

| Requirement | Notes |
|---|---|
| Linux `x86_64` | The current RSEQ assembly and allocator design target this platform. |
| Stable Rust | Supported for `GlobalAlloc`, the native allocation API, and preload builds. `allocator-api` requires nightly. |
| Libc-provided RSEQ TLS | glibc 2.35+ or equivalent support. RSMalloc reads libc's `__rseq_size` and `__rseq_offset`; it does not register RSEQ itself. Older or unsupported libc configurations will not bootstrap. |

### Use it as a Rust global allocator

Add RSMalloc to your application's `Cargo.toml`:

```toml
[dependencies]
rsmalloc = "0.3.0-alpha"
```

Then select it as the global allocator:

```rust
use rsmalloc::v2::alloc::RSMalloc;

#[global_allocator]
static GLOBAL: RSMalloc = RSMalloc::new_default();
```

That's it — `new_default()` is the starting configuration. Ordinary allocations through `Box`, `Vec`, and other users of Rust's global allocator now go through RSMalloc. See [Configuration](#configuration) to tune memory retention and refill behavior.

### Or preload it into a dynamically linked program

From the repository root:

```sh
cargo build --release --features preload
LD_PRELOAD=./target/release/librsmalloc.so your-program
```

The preload build provides `malloc`, `calloc`, `realloc`, `reallocarray`, `recallocarray`, `free`, sized-free shims, `posix_memalign`, `memalign`, `aligned_alloc`, `valloc`, `pvalloc`, `malloc_usable_size`, and opt-in `malloc_trim`.

Preload tuning uses [environment variables](#runtime-environment-variables-preload-builds). Rust allocator builds use the v2 configuration API. These are separate ways to configure the same allocator internals.

**Build portability:** This repository's `.cargo/config.toml` uses `target-cpu=native`. Repository builds are optimized for the build machine; review that setting before distributing binaries to different CPUs.

## Design Approach

### CPU-local allocation, shared reuse

The slab path handles requests through **2 MiB**, using 34 size classes:

1. **Try the current CPU's RSEQ cache.** A cache hit avoids the usual mutex-based cache operation. CPU migration invalidates the protected operation, which retries or uses a fallback path.
2. **Refill from transfer caches.** Reuse locally available blocks, then try other CPUs, preferring the current NUMA node.
3. **Initialize a batch when reuse is insufficient.** Pending refill spans are kept per CPU and class; overflow spans use an ABA-tagged lock-free queue.
4. **Return one block and cache the remainder.** Excess blocks go back to transfer caches for reuse elsewhere.

A free block belongs to the cache holding it, not permanently to the thread or CPU that originally allocated it. This lets memory follow changing allocation demand rather than remain tied to a thread-local heap.

### Separate policies for separate jobs

- **Inventory-aware transfer batching.** The default predictor uses the supplying transfer list's sampled inventory, including cross-CPU steals. Its target is one quarter of that inventory, within class limits; each update closes one quarter of the gap to the target.
- **Independent bulk-refill sizing.** Fresh-block initialization has its own per-CPU/per-class predictor. Reusing initialized blocks and obtaining fresh blocks do not share one batch policy.
- **Optional batching policies.** `confidence-predictor` adds confidence-based transfer sizing. `experimental-predictive-demand` adds refill-only demand feedback. Both are opt-in; neither is a universal performance improvement.
- **NUMA-aware placement and reuse.** Transfer stealing, refill arenas, pending spans, and the large-object backend prefer the current node before trying remote nodes. Placement uses preferred `mbind` policy, not guaranteed physical locality; the public capability surface reports NUMA support as partial.

### Large allocations and reclamation

Larger requests normally use power-of-two blocks from NUMA-aware **64 MiB segmented-bitmap regions**. Requests outside that backend's range, or requests it cannot satisfy, fall back to direct mappings. Routing depends on the adjusted allocation size, including metadata and alignment.

RSMalloc aims to keep reusable memory productive without relying on aggressive trimming. Background and manual trimming reclaim cold slab and segmented-bitmap pages with `madvise`; small-allocation trim eligibility uses an EMA of observed block lifetimes. Cached memory is not necessarily returned to the kernel immediately after `free`.

These are design goals, not guarantees of low RSS or high throughput. See [ARCHITECTURE.md](ARCHITECTURE.md) for the allocation lifecycle, RSEQ commit protocol, cache layout, and ownership rules.

## Status & Limitations

- **Alpha software.** Test coverage and deployment experience are limited. The allocator has not been comprehensively audited.
- **Platform-specific.** Linux `x86_64` and working libc RSEQ registration are part of the deployment contract.
- **APIs may change.** The Rust and preload frontends are still being separated and stabilized before a stable release.
- **Not every path is lock-free.** Large-allocation metadata uses a sharded, lock-protected hash map; some backend operations also use locks.
- **Loader and process interactions need testing.** Libc, dynamic loading, fork behavior, and application configuration can affect compatibility.
- **Benchmarks are development signals.** Test your own application, allocator features, kernel, THP policy, and NUMA environment before drawing conclusions.

## Configuration

The v2 API separates performance and memory-retention tuning from settings that weaken allocator protection. Build a `Tuning`, wrap it in `Config`, and pass it to `RSMalloc::new`.

Configuration is **process-wide**: the first allocator instance to initialize supplies the settings. Creating another instance does not create an isolated heap or reconfigure the initialized allocator.

```rust
use rsmalloc::v2::{
    alloc::RSMalloc,
    config::{
        Config, PerCacheLimit, Percentage, ReliefSettings, ReliefState,
        SegmentedBitmapTHP, Size, THP, THPSettings, Tuning,
    },
};

const CONFIG: Config = Config::new(
    Tuning::DEFAULT
        .with_thp(THPSettings::new(THP::Enabled, SegmentedBitmapTHP::Force))
        .with_refill_init_batch(16)
        .with_max_refill_retries(4)
        .with_max_per_segmented_bitmap_cache(PerCacheLimit::Custom(Size::MiB(512)))
        .with_relief(ReliefSettings::new(
            ReliefState::Enabled,
            Percentage::new(85),
            Percentage::new(80),
        )),
);

#[global_allocator]
static GLOBAL: RSMalloc = RSMalloc::new(CONFIG);
```

This is a tuning example, not a recommended configuration for every workload. In particular, `PerCacheLimit` sets the **initial segmented-bitmap region size**, not a hard cap on all cached memory; later growth adds 64 MiB regions.

### Default settings

| Setting | Default |
|---|---|
| Initial transfer predictor batch | 128 blocks, subject to size-class limits |
| Maximum refill retries | 3 |
| Minimum slab arena setting | 256 MiB |
| Initial segmented-bitmap region | 64 MiB |
| Background trim thresholds | 10 MiB small / 512 MiB segmented-bitmap cached VA |
| Transparent huge pages | General support enabled; segmented-bitmap forcing disabled |
| Memory-pressure relief | Disabled; thresholds are 85% to disable segmented-bitmap allocation and 80% to re-enable it |
| Magic values and aligned-allocation tag | Randomized at bootstrap |
| Foreign-pointer policy | Abort when the foreign-pointer path is reached; see the ownership caveats below |

`ArenaSize::new` accepts zero and multiples of 4 KiB. Initialization enforces a **256 KiB minimum**, while the default setting remains **256 MiB**. Big-allocation trim thresholds must be at least **128 MiB**; smaller values panic during bootstrap.

Security-sensitive settings require `expose-security-critical-settings`. Applying them with `Config::with_security_critical` is unsafe; fixed magic values additionally require `MagicSafetyDisable::acknowledge_safety_risk()`. These settings are not ordinary performance knobs.

### Migrating legacy Rust configuration

For an existing root-level `RSMallocConfig`, `legacy_config_to_v2!` converts ordinary tuning to a v2 `Config` in a const context:

```rust
use rsmalloc::{RSMallocConfig, legacy_config_to_v2};
use rsmalloc::v2::alloc::RSMalloc;

const CONFIG: rsmalloc::v2::config::Config =
    legacy_config_to_v2!(RSMallocConfig::DEFAULT.with_max_refill_retries(4));

#[global_allocator]
static GLOBAL: RSMalloc = RSMalloc::new(CONFIG);
```

The legacy type and macro are migration aids, not the preferred API for new code:

- The macro is available in Rust allocator builds, not preload builds.
- It uses v2's 512 MiB big trim threshold because legacy configuration only specifies the small-allocation threshold.
- Legacy arena sizes below 256 KiB are clamped to that historical minimum.
- Fixed magic, ignored foreign pointers, and larger arena sizes that are not multiples of 4 KiB are rejected rather than silently migrated.

Convert security-sensitive choices explicitly using the v2 security settings and their unsafe acknowledgements.

### Native allocation interface

Enable `native-allocation-api` when you want malloc-style Rust operations without retaining a `Layout`:

```toml
[dependencies]
rsmalloc = { version = "0.3.0-alpha", features = ["native-allocation-api"] }
```

This interface works on stable Rust and is independent of the nightly-only `allocator-api` feature. Returned storage remains allocated until you explicitly free it or successfully resize it; dropping a raw pointer does not free anything.

`AllocationSize` describes **bytes only**. `array_bytes::<T>(count)` checks multiplication but does not request `T`'s alignment. Use `allocate_aligned` when alignment matters.

```rust
use std::{mem::align_of, num::NonZero};

use rsmalloc::v2::{
    alloc::RSMalloc,
    allocation_api::{AllocationAPI, AllocationError, AllocationSize},
};

static ALLOCATOR: RSMalloc = RSMalloc::new_default();

fn main() -> Result<(), AllocationError> {
    let size = AllocationSize::array_bytes::<u64>(128)?;
    let pointer = ALLOCATOR.allocate_aligned(size, NonZero::new(align_of::<u64>()).unwrap())?;

    // On success, the old pointer is invalid even if the address stays the same.
    // On failure, it remains live and must still be freed or retried.
    let pointer = unsafe {
        match ALLOCATOR.aligned_reallocate(
            pointer.as_ptr(),
            AllocationSize::from_bytes(2048),
            NonZero::new(64).unwrap(),
        ) {
            Ok(next) => next,
            Err(error) => {
                ALLOCATOR.deallocate(pointer.as_ptr());
                return Err(error);
            }
        }
    };
    assert_eq!((pointer as usize) % 64, 0);
    unsafe { ALLOCATOR.deallocate(pointer) };
    Ok(())
}
```

| Operation | Behavior |
|---|---|
| `allocate`, `allocate_aligned` | Return uninitialized storage as `NonNull<u8>`. Explicit alignment must be a supported power of two. |
| `allocate_zeroed`, `allocate_zeroed_nmem` | Zero requested bytes, not necessarily all extra usable capacity. The `nmem` form checks multiplication. |
| `usable_size` | Reports usable payload capacity for a live allocation. Extra capacity is not automatically initialized. |
| `deallocate` | Frees a live allocation without its original size or alignment. Null is a no-op. |
| `reallocate` | Resizes while preserving the existing alignment. |
| `aligned_reallocate` | Resizes with at least the requested alignment; may move solely to satisfy it. |

**Resize ownership rules:**

- Nonzero size: success returns a non-null pointer and invalidates the old one; failure leaves the old allocation live.
- Null pointer with nonzero size: treated as an allocation request.
- Non-null pointer with zero size: frees the allocation and returns `Ok(null_mut())`.
- Null pointer with zero size: returns `Err(AllocationError::NotSupported)`, after alignment validation when applicable.

The `nmem` zeroed-allocation method currently reports multiplication overflow and allocation failure as `SomethingWentWrong`. Alignment arguments use `NonZero<usize>` but must still be supported powers of two. Operations on existing allocations require live pointers from an equivalent RSMalloc instance—not pointers owned by another allocator.

For lower-level operations, `RSMalloc::raw()` exposes `RawInterface`. Manual trimming and `rs_usable_size` are available through `RSMallocCoreAPI`. These malloc-style interfaces have contracts separate from Rust's `GlobalAlloc` and `Allocator` traits.

### Runtime environment variables (preload builds)

Set these **before the process initializes the allocator**. Size values are decimal byte counts. Environment variables configure preload builds; use `Config` for Rust allocator builds.

#### Refill and backing memory

| Variable | Default | Meaning |
|---|---|---|
| `RS_ARENA_SIZE` | `268435456` (256 MiB) | Minimum slab arena setting; clamped to at least 256 KiB. |
| `RS_PREDICTOR_INIT_BATCH` | `128` | Initial transfer predictor batch, subject to class limits. |
| `RS_MAX_REFILL_RETRIES` | `3` | Maximum refill retries. |
| `RS_SEGMENTED_BITMAP_PER_CACHE_SIZE` | `67108864` (64 MiB) | Initial segmented-bitmap region size; clamped to 64 MiB–128 TiB and rounded up to a power of two. Later growth adds 64 MiB regions. |

#### Trimming and memory pressure

| Variable | Default | Meaning |
|---|---|---|
| `RS_DISABLE_TRIM_THREAD` | `0` | Nonzero disables periodic background trimming. Manual trimming remains available; the worker can still handle enabled memory-pressure relief. |
| `RS_TRIMMER_THRESHOLD` | `10485760` (10 MiB) | Cached small-allocation VA threshold for starting the background worker. |
| `RS_BIG_TRIMMER_THRESHOLD` | `536870912` (512 MiB) | Cached segmented-bitmap VA threshold for starting the worker. Must be at least 128 MiB. |
| `RS_ENABLE_RELIEF` | `1` (disabled) | **Set `0` to enable** memory-pressure relief; nonzero disables it in the current alpha. |
| `RS_SEGMENTED_BITMAP_RELIEF_DISABLE_PERCENTAGE` | `85` | Memory-usage percentage at which segmented-bitmap allocation is disabled. Capped at 100. |
| `RS_SEGMENTED_BITMAP_RELIEF_ENABLE_PERCENTAGE` | `80` | Percentage at which allocation may re-enable after two qualifying checks. Capped at the disable threshold. |

Relief uses system-wide RAM-plus-swap usage from `sysinfo`, not a `MemAvailable`-based estimate. The separate enable/disable thresholds provide hysteresis; direct mappings remain the fallback when segmented-bitmap allocation is disabled.

#### Huge pages and randomization

| Variable | Default | Meaning |
|---|---|---|
| `RS_SEGMENTED_BITMAP_ATTEMPT_HUGEPAGE` | `0` | Nonzero requests THP for segmented-bitmap regions when general THP is enabled. |
| `RS_DISABLE_THP` | `0` | Set exactly `1` to disable transparent huge-page attempts. |
| `RS_DISABLE_RANDOMIZING` | `0` | **Weakens protection:** nonzero keeps fixed magic values and the fixed aligned-allocation tag. |

Use valid decimal values: the current parser consumes leading decimal digits, treats an empty or nonnumeric start as zero, and ignores trailing nondigits. Defaults apply when a variable is absent, not when its value is malformed. This matters especially for `RS_ENABLE_RELIEF`, where zero enables the feature.

## Cargo Features

All features are **disabled by default**. Enable only what you need with `--features feature-name`; combine features with commas. In a Rust dependency, use the `features = [...]` list in `Cargo.toml`.

### Allocation interfaces

| Feature | What it does |
|---|---|
| `preload` | Enables the C API and `LD_PRELOAD` support. |
| `native-allocation-api` | Enables the malloc-style Rust `AllocationAPI` interface. Works on stable Rust. |
| `allocator-api` | Enables Rust's `Allocator` interface. Requires nightly Rust. |

### Batching and memory usage

| Feature | What it does |
|---|---|
| `confidence-predictor` | Uses confidence feedback to size transfer batches, up to each size class's CPU-cache capacity. |
| `experimental-predictive-demand` | Samples the refilling class's cache occupancy to track expected demand and previous unused blocks. Feeds either transfer predictor without bookkeeping on allocation/free cache hits. |
| `lazy-page-trim` | Uses lazy page reclamation instead of immediately discarding small-allocation pages. |
| `trim-aggressively` | Keeps idle-class trimming eligibility tighter. |
| `page-backend-no-huge-page` | Discourages huge pages for slab arenas. May reduce RSS at the cost of more TLB pressure. |
| `page-backend-huge-page` | Requests huge pages for slab arenas. Ignored when `page-backend-no-huge-page` is also enabled. |

`confidence-predictor` and `experimental-predictive-demand` work independently or together. Neither is included in `semi-hardened`. They may improve or worsen throughput, latency, and memory usage—benchmark your workload before enabling them. The demand policy is experimental; disabled builds omit its state and sampling entirely.

For example, to try both in a preload build:

```sh
cargo build --release --features preload,confidence-predictor,experimental-predictive-demand
```

### Hardening and advanced settings

| Feature | What it does |
|---|---|
| `semi-hardened` | Enables `extended-header`, `check-owned-on-alloc`, `zero-small-on-free`, and `validate-foreign-first`. |
| `extended-header` | Uses wider allocation headers. |
| `check-owned-on-alloc` | Checks ownership before returning allocations. Adds an ownership lookup. |
| `validate-foreign-first` | Checks ownership before metadata reads during free, including recovered aligned pointers. Adds ownership lookups. |
| `zero-small-on-free` | Clears 16–64B allocation payloads when freed. |
| `explicit-zero` | Uses `explicit_bzero` for `calloc`, preventing removal of the zeroing operation. |
| `abort-on-rseq-failure` | Aborts when RSEQ reports the invalid CPU ID `u32::MAX`. |
| `disable-magic-security-checks` | **Weakens protection:** disables magic-based double-free and corruption checks. |
| `expose-security-critical-settings` | **Advanced:** exposes settings that can weaken magic randomization and foreign-pointer handling. |

### Free ownership assumptions

**Only free live pointers owned by RSMalloc.** By default, free checks that non-null input addresses are below `2^56`, then reads alignment and header metadata before ownership classification. The address-range check does not prove that memory is mapped, owned, or an allocation boundary. Foreign-pointer fallback/policy is best-effort: a foreign or invalid pointer can fault or be misclassified before reaching it.

`validate-foreign-first` checks input ownership before those metadata reads and checks recovered aligned-allocation bases before reading their headers. A non-owned recovered base aborts as possible metadata injection. The feature is disabled by default and included in `semi-hardened`.

The ownership radix tracks **4 KiB regions**, not individual allocation boundaries. These checks do not make arbitrary pointers, interior pointers, double frees, or use-after-free safe. Without the feature, aligned free relies on valid alignment metadata and does not ownership-check the recovered base. See [ARCHITECTURE.md](ARCHITECTURE.md) and [SECURITY.md](SECURITY.md) for the detailed assumptions.

### Debug/diagnostic tiers

Use these for troubleshooting, **not performance benchmarks**. Features enable their required dependencies; these rows are not a single cumulative ladder.

| Feature | Adds |
|---|---|
| `debug` | Base internal counters for RSEQ and refills. |
| `debug-print` | Exit-time allocator reports; includes `debug`. |
| `debug-printer-thread` | Live report snapshots from a background thread; includes `debug-print`. |
| `debug-preload` | Combines `preload` with debug counters and reports. |
| `debug-exact` | Lock call, retry, and spin-wait counters. |
| `debug-predictor-exact` | More intrusive refill over/under-prediction accounting. |
| `predictor-debug` | Refill predictor logging; independent of the counter/report tiers. |
| `transfer-debug` | Transfer-cache steal, dry-steal, and CAS-retry counters. |
| `transfer-debug-exact` | Transfer-cache push/pop call counters. |
| `debug-full` | Convenience bundle for broad transfer/debug instrumentation. |
| `debug-full-critic` | `debug-full` plus exact predictor diagnostics. |

## Benchmarks

Start with the [benchmark overview](benchmarks/benchmarks.md), or use the [detailed results](benchmarks/benchmarks_detailed.md) for per-workload tables and elapsed-time/RSS charts. The [raw measurements](benchmarks/benchmark_overall.txt) remain available for inspection.

The current comparison covers 21 workloads and five allocators, using column-wise medians of ten runs. Different column medians need not represent one actual run, and small differences do not by themselves establish statistical significance.

A reproducible [MariaDB/sysbench harness](benchmarks/mariadb/README.md) and [rstress results](benchmarks/rstress_results.md) are also available. Kernel, scheduling, compiler flags, THP, NUMA layout, and allocator settings can materially change results. Compare both speed and memory use with your own workload; do not treat one synthetic ranking as a universal allocator ranking.

## Architecture

[ARCHITECTURE.md](ARCHITECTURE.md) is the full walkthrough. The source is organized around these responsibilities:

| Area | Responsibility |
|---|---|
| `src/frontend/` | Rust allocator APIs, v2 configuration, stats, and the preload C ABI. |
| `src/inner/` | Shared alloc, free, calloc, realloc, aligned allocation, and preload fallback behavior. |
| `src/rseq_core/` | RSEQ assembly, CPU caches, transfer reuse, pending spans, and refill. |
| `src/big_allocations/` | Segmented-bitmap reuse and direct large mappings. |
| `src/backend/` | Page arenas, bootstrap, background trimming, and relief. |
| `src/internals/` | Ownership radix, large-allocation metadata, NUMA support, locks, and initialization helpers. |
| `src/core_prim/` | Predictors, hardware helpers, fork support, and pointer wrappers. |
| `src/utility.rs` | Size classes, batch limits, alignment, and size matching. |

## Contributing

See [CONTRIBUTING.md](CONTRIBUTING.md) for development guidance, [ROADMAP.md](ROADMAP.md) for planned work, and [TODO.md](TODO.md) for current tasks.

Report suspected vulnerabilities privately using [SECURITY.md](SECURITY.md), rather than opening a public issue. RSMalloc is licensed under [MIT](LICENSE).
