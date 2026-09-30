
# RSMalloc

An RSEQ-based memory allocator for Rust, focused on low-overhead concurrent allocation for real applications rather than benchmark-only patterns. The small-allocation fast path uses Linux Restartable Sequences (RSEQ), so cache ownership follows the CPU, not the thread. Larger allocations use a NUMA-aware segmented-bitmap cache or direct mappings.

**Status: `0.3.0-alpha`. Alpha-quality software — not production-ready.** See [Status & Limitations](#status--limitations) below.

**Major milestone:** RSMalloc's default Rust `GlobalAlloc` and C `LD_PRELOAD` configurations now build on stable Rust. The allocator no longer depends on Rust thread-local storage: refill metadata and adaptive batching are maintained per CPU, and overflow refill metadata uses an ABA-tagged lock-free queue. The optional `allocator-api` feature remains nightly-only until Rust stabilizes `std::alloc::Allocator`.

[crates.io](https://crates.io/crates/rsmalloc) · [Architecture](ARCHITECTURE.md) · [Release Notes](UPDATES.md) · [Roadmap](ROADMAP.md) · [Todo](TODO.md) · [Benchmarks](benchmarks/benchmarks.md) · [Contributing](CONTRIBUTING.md)

> **Known issue:** Linux kernel `7.0.10` appears to trigger `SIGBUS` in some workloads when using rsmalloc. If you hit unexplained `SIGBUS` crashes, try a different kernel version before assuming allocator corruption.

## Quick Start

Requires stable Rust and a libc with RSEQ TLS support (glibc 2.35+ or equivalent) — rsmalloc relies on libc-registered `__rseq_size`/`__rseq_offset` rather than registering RSEQ itself, so an older libc will fail to bootstrap. The optional `allocator-api` feature still requires nightly Rust because `std::alloc::Allocator` remains unstable.

```rust
use rsmalloc::v2::alloc::RSMalloc;

#[global_allocator]
static GLOBAL: RSMalloc = RSMalloc::new_default();
```

That's it — `RSMalloc::new_default()` is a reasonable starting configuration. See [Configuration](#configuration) below to tune it.

### Or preload it into any binary

```sh
cargo build --release --features preload
LD_PRELOAD=./target/release/librsmalloc.so your-program
```

Preload builds provide the standard C ABI: `malloc`, `calloc`, `realloc`, `reallocarray`, `recallocarray`, `free`, sized-free shims, `posix_memalign`, `memalign`, `aligned_alloc`, `valloc`, `pvalloc`, `malloc_usable_size`, and opt-in `malloc_trim`.

## Design Approach

- **CPU-local caching via RSEQ.** The small-allocation fast path mutates per-CPU freelists without normal lock overhead as long as the thread stays on the same CPU through the critical section; on migration the operation retries or falls back to a transfer cache.
- **NUMA topology is used where available**, as a placement preference rather than a guarantee. Transfer-cache stealing, refill arenas, the segmented-bitmap backend, and pending-metadata reuse try the current node first before scanning remote nodes. This is preferred placement (`mbind`), not enforced physical placement, and the public capability surface currently reports NUMA support as partial.
- **Adaptive refill sizing.** A small integer predictor grows/shrinks per-class refill batches based on observed demand instead of a static batch size.
- **Designed to avoid relying on aggressive trimming.** Adaptive refill sizing, cross-CPU reuse, and age-aware reclamation aim to keep reusable memory productive without repeatedly discarding and refaulting hot pages.
- **Background and manual trimming.** Cold small-allocation and segmented-bitmap cached pages are returned to the kernel via `madvise`, with per-size-class eligibility tracked by an EMA of observed block lifetimes.
- In early, workload-specific measurements it has performed competitively against mimalloc/glibc on some real applications — see [benchmarks/real_workloads.md](benchmarks/real_workloads.md). This is not a general performance guarantee; results vary by workload (see the Blender numbers there for a mixed case).

None of this has been evaluated at production scale or across a wide range of workloads yet. For the full internals (allocation/free lifecycle, slab cache layout, refill path, segmented-bitmap backend, ownership tracking) see [ARCHITECTURE.md](ARCHITECTURE.md).

## Status & Limitations

- Alpha-quality software with limited test coverage — expect rough edges, not memory-safety guarantees beyond what's documented.
- The default and preload configurations support stable Rust. The optional `allocator-api` feature requires nightly Rust.
- Requires a libc with RSEQ TLS support (glibc 2.35+ or equivalent); rsmalloc reads libc's `__rseq_size`/`__rseq_offset` rather than registering RSEQ itself, so older libc versions won't bootstrap.
- The preload path and the Rust `GlobalAlloc` path are still being separated and stabilized; the public Rust API may still change before a stable release.
- Big-allocation metadata uses an internal sharded, lock-protected hash map.
- Not yet audited across every libc/loader/fork combination.
- Benchmarks are a development signal, not an authoritative performance claim — test with your own workload.

## Configuration

The v2 API separates ordinary performance and memory-retention tuning from security-critical settings. Build a `Tuning`, wrap it in `Config`, and pass that configuration to `RSMalloc::new`.

```rust
use rsmalloc::v2::{
    alloc::RSMalloc,
    config::{
        Config, PerCacheLimit, Percentage, ReliefSettings, ReliefState, SegmentedBitmapTHP,
        THP, THPSettings, Tuning,
    },
};

const CONFIG: Config = Config::new(
    Tuning::DEFAULT
        .with_thp(THPSettings::new(THP::Enabled, SegmentedBitmapTHP::Force))
        .with_refill_init_batch(16)
        .with_max_refill_retries(4)
        .with_max_per_segmented_bitmap_cache(PerCacheLimit::Bytes(512 * 1024 * 1024))
        .with_relief(ReliefSettings::new(
            ReliefState::Enabled,
            Percentage::new(85),
            Percentage::new(80),
        )),
);

#[global_allocator]
static GLOBAL: RSMalloc = RSMalloc::new(CONFIG);
```

Defaults: randomized magic values enabled, abort on foreign pointers, general THP enabled (segmented-bitmap THP forcing off), a 64 MiB initial segmented-bitmap region, a 256 MiB minimum slab arena, 10 MiB small and 512 MiB big background-trim thresholds, memory-pressure relief disabled, and the allocator-default refill prediction.

Security-sensitive configuration is hidden unless the `expose-security-critical-settings` feature is enabled. Keeping fixed magic values additionally requires the explicit unsafe `MagicSafetyDisable::acknowledge_safety_risk()` token.

### Migrating legacy Rust configuration

For an existing root-level `RSMallocConfig`, the temporary `legacy_config_to_v2!` macro converts ordinary tuning into a v2 `Config` in a const context:

```rust
use rsmalloc::{RSMallocConfig, legacy_config_to_v2};
use rsmalloc::v2::alloc::RSMalloc;

const CONFIG: rsmalloc::v2::config::Config =
    legacy_config_to_v2!(RSMallocConfig::DEFAULT.with_max_refill_retries(4));

#[global_allocator]
static GLOBAL: RSMalloc = RSMalloc::new(CONFIG);
```

The macro is available in Rust allocator builds, not `preload` builds. It uses v2's 512 MiB default big-allocation trim threshold because the legacy config only specifies a small-allocation threshold. It **rejects** fixed magic, ignored foreign pointers, and arena sizes above 512 KiB that are not multiples of 512 KiB; these cannot be migrated silently. Convert security-sensitive choices explicitly with v2's `expose-security-critical-settings` feature and its required unsafe acknowledgement. The legacy configuration type and this macro are migration aids, not the preferred API for new code.

### Native allocation interface

The native allocation interface is optional and disabled by default. Enable the `native-allocation-api` Cargo feature (for example, `rsmalloc = { version = "0.3.0-alpha", features = ["native-allocation-api"] }`). It works on stable Rust and is independent of the nightly-only `allocator-api` feature.

`AllocationAPI` is for callers that want allocator-owned metadata without retaining a Rust `Layout`. `AllocationSize` records bytes only: `array_bytes::<T>(count)` checks multiplication, but does not request `T`'s alignment. Use `allocate_aligned` for typed storage whose alignment matters. A returned pointer stays allocated until you deallocate it or successfully reallocate it; dropping the raw pointer does nothing.

This example requests a typed alignment, handles a failed resize without losing the original allocation, and frees the final pointer:

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

    // Request more space and a stronger alignment without retaining a Layout.
    // A successful reallocation invalidates the old pointer, even if unchanged.
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

The main operations are:

| Operation | Result and ownership |
|---|---|
| `allocate`, `allocate_aligned` | Return uninitialized storage as `NonNull<u8>`; alignment must be a supported power of two. |
| `allocate_zeroed`, `allocate_zeroed_nmem` | Zero the requested bytes, not necessarily any extra usable capacity. The `nmem` form checks multiplication and currently reports either overflow or allocation failure as `SomethingWentWrong`. |
| `usable_size` | Reports usable payload bytes for a live pointer. Extra capacity may exist but is not automatically initialized. |
| `deallocate` | Frees a live pointer without its original size or alignment; null is a no-op. |
| `reallocate` | Preserves the existing alignment. A successful resize invalidates the old pointer even if the address stays the same. |
| `aligned_reallocate` | Requests a result with at least the supplied alignment; it may move solely to satisfy that alignment. |

Both resize methods accept a null input as an allocation request for a nonzero size. A **null** input with zero size returns `Err(AllocationError::NotSupported)` after alignment validation, if applicable. A zero-sized request with a **non-null** input frees the old allocation and returns `Ok(null_mut())`. For nonzero sizes, `Ok` contains a non-null pointer; on `Err`, the original non-null allocation remains live and must still be freed or retried. The aligned methods take `NonZero<usize>`, but still require a supported power of two. `usable_size`, `deallocate`, and both resize methods require a live pointer from an equivalent rsmalloc instance when the pointer is non-null. Do not pass a pointer owned by another allocator.


For lower-level malloc-style operations, `RSMalloc::raw()` exposes `v2::alloc::RawInterface`. Manual trimming and the `rs_usable_size` helper are available through `v2::alloc::RSMallocCoreAPI`. These interfaces are separate from Rust's `GlobalAlloc` and `Allocator` contracts.

### Runtime environment variables (preload builds)

| Variable | Default | Meaning |
|---|---|---|
| `RS_ARENA_SIZE` | `268435456` (256 MiB) | Minimum slab page-backend arena size in bytes. |
| `RS_PREDICTOR_INIT_BATCH` | `128` | Initial per-class refill predictor batch. |
| `RS_MAX_REFILL_RETRIES` | `3` | Max refill retries. |
| `RS_SEGMENTED_BITMAP_PER_CACHE_SIZE` | `67108864` (64 MiB) | Initial segmented-bitmap region size; clamped to at least 64 MiB and rounded to a power of two. Later growth adds 64 MiB regions. |
| `RS_SEGMENTED_BITMAP_ATTEMPT_HUGEPAGE` | `0` | Set `1` to request THP for segmented-bitmap regions. |
| `RS_DISABLE_TRIM_THREAD` | `0` | Set nonzero to disable the background trim worker (manual `malloc_trim` still works). |
| `RS_TRIMMER_THRESHOLD` | `10485760` | Minimum cached small-allocation VA (bytes) before the background trim worker starts. |
| `RS_BIG_TRIMMER_THRESHOLD` | `536870912` | Minimum cached big-allocation VA (bytes) before the background trim worker starts. |
| `RS_ENABLE_RELIEF` | disabled | Set `0` to enable system-memory-pressure relief (yes, `0` enables it in the current alpha). |
| `RS_SEGMENTED_BITMAP_RELIEF_DISABLE_PERCENTAGE` | `85` | System memory-usage % at/above which segmented-bitmap allocation is disabled. |
| `RS_SEGMENTED_BITMAP_RELIEF_ENABLE_PERCENTAGE` | `80` | System memory-usage % at/below which segmented-bitmap allocation may re-enable. |
| `RS_DISABLE_THP` | `0` | Set `1` to disable transparent huge page attempts. |
| `RS_DISABLE_RANDOMIZING` | `0` | Set `1` to keep fixed built-in magic values instead of randomizing at bootstrap. |

## Cargo Features

| Feature | Effect |
|---|---|
| `preload` | Builds the C ABI / `LD_PRELOAD` surface. |
| `native-allocation-api` | Enables the optional `v2::allocation_api::AllocationAPI` malloc-style Rust interface; disabled by default. |
| `expose-security-critical-settings` | Exposes the v2 configuration knobs that can weaken magic randomization or foreign-pointer handling. |
| `extended-header` | Wider per-allocation header metadata. |
| `page-backend-no-huge-page` | No-huge-page advice for slab arenas — cuts RSS on THP-aggressive systems (e.g. CachyOS), costs TLB pressure. |
| `page-backend-huge-page` | Huge-page advice for slab arenas (ignored if the above is also set). |
| `check-owned-on-alloc` | Semi-hardening: verifies popped allocations are still `RADIX`-owned before returning them. Adds a lookup to the alloc path. |
| `zero-small-on-free` | Zeroes 16–64B allocations (cryptographic-key sized) on free; cheap enough for security without a big performance penalty. |
| `guard-pages-thp` | Lazily places a `PROT_NONE` guard page at the last 4KB of every **2MB-aligned** page-allocator block, materialized only as the bump pointer reaches it. Catches some OOB bugs; size classes up to 1MB are guaranteed never to straddle a guard (denied outright if they would), while larger requests only get a guard consumed at their leading edge, not dense coverage through their body. |
| `guard-pages-ignore-thp` | Shrinks `guard-pages-thp`'s interval from 2MB to 64KB for denser coverage; fragments page tables more often. |
| `semi-hardened` | Convenience bundle: `extended-header` + `check-owned-on-alloc` + `zero-small-on-free` + `guard-pages-ignore-thp`. |
| `lazy-page-trim` | Lazy page-free advice for small-allocation trim instead of immediate `MADV_DONTNEED`. |
| `trim-aggressively` | Skips the idle-class ceiling nudge in trim's average-lifetime tracking, keeping trim eligibility tighter. |
| `disable-magic-security-checks` | Compile-time-only: disables magic-value double-free/corruption checks. |
| `print-cpu-on-double-free` | Includes the current RSEQ CPU id in fatal double-free/corruption reports. |
| `abort-on-rseq-failure` | Aborts if RSEQ reports an impossible CPU id (`u32::MAX`), signaling a kernel/hardware failure, instead of leaving it unchecked. |
| `explicit-zero` | Zeroes `calloc` memory with `explicit_bzero` instead of a plain byte-fill, so the zeroing can't be optimized away. |

### Debug/diagnostic tiers

Each tier below enables the previous one plus more. Higher tiers add real overhead — use them for diagnosing behavior, not for benchmarking.

| Feature | Adds |
|---|---|
| `debug` | Base internal counters (RSEQ/refill). |
| `debug-print` | Exit-time allocator report via `eprintln!`. |
| `debug-printer-thread` | Background thread for live report snapshots. |
| `debug-exact` | Lock call/retry/spin-wait counters. |
| `debug-predictor-exact` | More intrusive refill over/under-prediction accounting. |
| `predictor-debug` | Per-decision predictor logging. |
| `transfer-debug` | Transfer-cache steal/dry-steal/CAS-retry counters. |
| `transfer-debug-exact` | Transfer-cache push/pop call counters. |
| `debug-full` | Convenience bundle: broad transfer/debug instrumentation. |
| `debug-full-critic` | `debug-full` plus exact predictor diagnostics. |

## Architecture

See [ARCHITECTURE.md](ARCHITECTURE.md) for the full walkthrough. Short version: `abi` (C ABI), `global_alloc` (Rust `GlobalAlloc`), `inner` (shared alloc/free/realloc/calloc/align ops), `rseq_core` (`SLAB_CACHE`, transfer caches, RSEQ asm, refill), `big_allocations` (`SEGMENTED_BITMAP_BACKEND` and direct mappings), `internals` (`RADIX` ownership map, `BIG_META_MAP`, NUMA, locks), `backend` (slab page arenas), `core_prim` (bootstrap, predictors, fork handling).

## Contributing

See [CONTRIBUTING.md](CONTRIBUTING.md).
