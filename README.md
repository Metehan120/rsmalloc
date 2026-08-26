# RSMalloc

An RSEQ-based memory allocator for Rust, focused on low-overhead concurrent allocation for real applications rather than benchmark-only patterns. The small-allocation fast path uses Linux Restartable Sequences (RSEQ), so cache ownership follows the CPU, not the thread. Larger allocations go through a separate NUMA-aware buddy-cached path.

**Status: `0.2.0-alpha`. Alpha-quality software — not production-ready.** See [Status & Limitations](#status--limitations) below.

[crates.io](https://crates.io/crates/rsmalloc) · [Architecture](ARCHITECTURE.md) · [Release Notes](UPDATES.md) · [Roadmap](TODO.md) · [Benchmarks](benchmarks/benchmarks.md) · [Contributing](CONTRIBUTING.md)

> **Known issue:** Linux kernel `7.0.10` appears to trigger `SIGBUS` in some workloads when using rsmalloc. If you hit unexplained `SIGBUS` crashes, try a different kernel version before assuming allocator corruption.

## Alpha 2.1: Rust API transition

Alpha 2.1 will introduce a redesigned Rust global allocator API under `rsmalloc::v2`.

- During Alpha 2.1, the existing root API remains usable but is deprecated.
- In Beta 1, the v2 API becomes the primary root API and the legacy API is removed. The `rsmalloc::v2` path remains temporarily available but deprecated.
- In Beta 2, the deprecated `rsmalloc::v2` compatibility path is removed.

The preload C ABI will be unaffected by this transition.

## Quick Start

Requires nightly Rust (`rustc 1.96.0`+) and a libc with RSEQ TLS support (glibc 2.35+ or equivalent) — rsmalloc relies on libc-registered `__rseq_size`/`__rseq_offset` rather than registering RSEQ itself, so an older libc will fail to bootstrap.

```rust
use rsmalloc::RSMalloc;

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
- **NUMA topology is used where available**, as a placement preference rather than a guarantee. Transfer-cache stealing, refill arenas, the buddy backend, and pending-metadata reuse try the current node first before scanning remote nodes. This is preferred placement (`mbind`), not enforced physical placement, and the public capability surface currently reports NUMA support as partial.
- **Adaptive refill sizing.** A small integer predictor grows/shrinks per-class refill batches based on observed demand instead of a static batch size.
- **Background and manual trimming.** Cold small-allocation and buddy-cached pages are returned to the kernel via `madvise`, with per-size-class eligibility tracked by an EMA of observed block lifetimes.
- In early, workload-specific measurements it has performed competitively against mimalloc/glibc on some real applications — see [benchmarks/real_workloads.md](benchmarks/real_workloads.md). This is not a general performance guarantee; results vary by workload (see the Blender numbers there for a mixed case).

None of this has been evaluated at production scale or across a wide range of workloads yet. For the full internals (allocation/free lifecycle, slab cache layout, refill path, buddy backend, ownership tracking) see [ARCHITECTURE.md](ARCHITECTURE.md).

## Status & Limitations

- Alpha-quality software with limited test coverage — expect rough edges, not memory-safety guarantees beyond what's documented.
- Requires nightly Rust and `rustc 1.96.0`+.
- Requires a libc with RSEQ TLS support (glibc 2.35+ or equivalent); rsmalloc reads libc's `__rseq_size`/`__rseq_offset` rather than registering RSEQ itself, so older libc versions won't bootstrap.
- The preload path and the Rust `GlobalAlloc` path are still being separated and stabilized; the public Rust API may still change before a stable release.
- Big-allocation metadata uses an internal lock-protected red-black tree.
- Not yet audited across every libc/loader/fork combination.
- Benchmarks are a development signal, not an authoritative performance claim — test with your own workload.

## Configuration

`RSMallocConfig` groups tuning into a few areas: THP behavior, adaptive refill-predictor settings, magic-value safety, foreign-pointer handling, buddy-cache sizing, slab arena sizing, memory-pressure relief, and refill retry limits.

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

Defaults: randomized magic values enabled, abort on foreign pointers, general THP enabled (buddy THP forcing off), default buddy cache limit, 256 MiB minimum slab arena, memory-pressure relief disabled, allocator-default refill predictor.

Weakening magic-value checks (for debugging/reproducible tests/security research) requires an explicit unsafe acknowledgement token — see `MagicSafety`/`MagicSafetyDisable` in the crate docs.

`RSMalloc` also exposes direct helpers in non-preload builds — `rs_malloc`, `rs_calloc`, `rs_memalign`, `rs_realloc`, `rs_trim`, `rs_free`, `rs_usable_size` — for use only with pointers `RSMalloc` itself returned. `GLOBAL.get_capabilities()` reports version, THP state, and NUMA support without allocating.

### Runtime environment variables (preload builds)

| Variable | Default | Meaning |
|---|---|---|
| `RS_ARENA_SIZE` | `268435456` (256 MiB) | Minimum slab page-backend arena size in bytes. |
| `RS_PREDICTOR_INIT_BATCH` | `128` | Initial per-class refill predictor batch. |
| `RS_MAX_REFILL_RETRIES` | `3` | Max refill retries. |
| `RS_BUDDY_PER_CACHE_SIZE` | `268435456` | Initial buddy region size; clamped to at least this, rounded to a power of two. |
| `RS_BUDDY_ATTEMPT_HUGEPAGE` | `0` | Set `1` to request THP for buddy regions. |
| `RS_DISABLE_TRIM_THREAD` | `0` | Set nonzero to disable the background trim worker (manual `malloc_trim` still works). |
| `RS_TRIMMER_THRESHOLD` | `10485760` | Minimum cached VA (bytes) before the background trim worker starts. |
| `RS_ENABLE_RELIEF` | disabled | Set `0` to enable system-memory-pressure relief (yes, `0` enables it in the current alpha). |
| `RS_BUDDY_RELIEF_DISABLE_PERCENTAGE` | `85` | System memory-usage % at/above which the buddy backend is disabled. |
| `RS_BUDDY_RELIEF_ENABLE_PERCENTAGE` | `80` | System memory-usage % at/below which the buddy backend may re-enable. |
| `RS_DISABLE_THP` | `0` | Set `1` to disable transparent huge page attempts. |
| `RS_DISABLE_RANDOMIZING` | `0` | Set `1` to keep fixed built-in magic values instead of randomizing at bootstrap. |

## Cargo Features

| Feature | Effect |
|---|---|
| `preload` | Builds the C ABI / `LD_PRELOAD` surface. |
| `extended-header` | Wider per-allocation header metadata. |
| `page-backend-no-huge-page` | No-huge-page advice for slab arenas — cuts RSS on THP-aggressive systems (e.g. CachyOS), costs TLB pressure. |
| `page-backend-huge-page` | Huge-page advice for slab arenas (ignored if the above is also set). |
| `check-owned-on-alloc` | Semi-hardening: verifies popped allocations are still `RADIX`-owned before returning them. Adds a lookup to the alloc path. |
| `semi-hardened` | Convenience bundle: `extended-header` + `check-owned-on-alloc`. |
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

See [ARCHITECTURE.md](ARCHITECTURE.md) for the full walkthrough. Short version: `abi` (C ABI), `global_alloc` (Rust `GlobalAlloc`), `inner` (shared alloc/free/realloc/calloc/align ops), `rseq_core` (`SLAB_CACHE`, transfer caches, RSEQ asm, refill), `big_allocations` (`BUDDY_BACKEND`), `internals` (`RADIX` ownership map, `BIG_META_MAP`, NUMA, locks), `backend` (slab page arenas), `core_prim` (bootstrap, predictors, fork handling).

## Contributing

See [CONTRIBUTING.md](CONTRIBUTING.md).
