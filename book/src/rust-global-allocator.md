# Rust Global Allocator

Use `rsmalloc::v2::alloc::RSMalloc` when your application should route ordinary Rust heap allocations through RSMalloc. This chapter describes **0.3.0-alpha**, not a stable API or a production-readiness guarantee.

## Before choosing it

RSMalloc targets Linux `x86_64` and relies on libc-provided RSEQ TLS (`__rseq_size` and `__rseq_offset`). The supported setup described by the project is glibc 2.35+ or equivalent RSEQ support. RSMalloc does not register RSEQ itself; unavailable RSEQ state aborts bootstrap. Test the actual libc, kernel, container, and CPU environment you will deploy on.

The allocator is designed around CPU-local RSEQ caches, with transfer caches sharing reusable blocks across CPUs. The slab path handles requests through 2 MiB; larger requests normally use segmented-bitmap regions or direct mappings. Metadata and alignment affect routing. NUMA placement is a preference, not guaranteed physical locality, and not every path is lock-free.

This makes it a candidate to **measure** for concurrent, allocation-heavy applications with repeated allocation and reuse. It is not automatically a better choice for:

- short-lived or allocation-light tools where bootstrap and retained caches may outweigh reuse;
- strict memory budgets or workloads expecting `free` to immediately reduce RSS;
- hard real-time paths requiring bounded allocation latency;
- unsupported platforms, or deployments requiring a mature, comprehensively audited allocator.

Start with defaults and compare application throughput, tail latency, RSS, and startup cost against your existing allocator. See [Benchmarking Correctly](benchmarking.md) and [Security and Reliability](security.md).

## Stable Rust: install the global allocator

In the application's `Cargo.toml`:

```toml
[dependencies]
rsmalloc = "0.3.0-alpha"
```

In `src/main.rs`:

```rust
use rsmalloc::v2::alloc::RSMalloc;

#[global_allocator]
static GLOBAL: RSMalloc = RSMalloc::new_default();

fn main() {
    let values: Vec<u64> = (0..100_000).collect();
    assert_eq!(values.len(), 100_000);
    println!("sum: {}", values.iter().sum::<u64>());
}
```

No Cargo feature or nightly compiler is needed for `GlobalAlloc`. Select one global allocator in the final Rust program; libraries should generally leave that choice to the application. `Box`, `Vec`, and other users of Rust's global allocator use this selection. It does **not** replace every allocator in the process: C libraries calling libc `malloc`, custom heaps, and direct mappings are separate allocation domains. See [LD_PRELOAD](preload.md) for dynamic C-symbol interposition.

Do not enable `preload` on this Rust dependency: that feature selects the C frontend and removes the `v2` Rust module. Cargo features are additive, so check the resolved feature set if another dependency enables it.

## Configuration belongs to the first initializer

`RSMalloc::new` and `new_default` are const constructors, not independent heaps. All instances share allocator-wide state. **The first instance to initialize supplies the process-wide configuration; later instances cannot reconfigure it.** Allocation, manual initialization, and core operations such as trimming can initialize that state.

Rust builds use `Config`, not the preload `RS_*` environment variables. Keep all instances on the same configuration, including instances used through the [native API](native-api.md). Do not depend on the order of two differently configured instances racing to initialize.

This complete alternative to the default example changes only the initial segmented-bitmap region size:

```rust
use rsmalloc::v2::{
    alloc::{RSMalloc, RSMallocCoreAPI},
    config::{Config, PerCacheLimit, Size, Tuning},
};

const CONFIG: Config = Config::new(
    Tuning::DEFAULT.with_max_per_segmented_bitmap_cache(
        PerCacheLimit::Custom(Size::MiB(128)),
    ),
);

#[global_allocator]
static GLOBAL: RSMalloc = RSMalloc::new(CONFIG);

fn main() {
    GLOBAL.manual_init();
    let buffer = vec![0_u8; 4096];
    assert_eq!(buffer.len(), 4096);
}
```

`manual_init()` can move one-time bootstrap work ahead of a request path; allocation also initializes automatically. With a global allocator, runtime allocations may already have initialized it before `main`. Manual initialization neither overrides earlier settings nor warms every size class or guarantees subsequent latency.

Despite its name, `PerCacheLimit` specifies the **initial segmented-bitmap region size**, not a hard cap on total cached memory. Later growth adds 64 MiB regions. A larger setting can increase retention; the example is an API demonstration, not a recommended speedup.

### Defaults and constraints worth knowing

| Setting | Default / constraint |
| --- | --- |
| Initial transfer predictor batch | 128 blocks, subject to size-class limits. Fresh-block refill prediction is a separate policy. |
| Maximum refill retries | 3. |
| Minimum slab arena setting | 256 MiB by default; initialization enforces at least 256 KiB. `ArenaSize::new` accepts zero or multiples of 4 KiB. |
| Initial segmented-bitmap region | 64 MiB. Not an RSS or total-cache limit. |
| Background trim thresholds | 10 MiB small / 512 MiB segmented-bitmap cached virtual address space requested; bootstrap uses `max(requested, initial_cached_bytes + 1)` for the big threshold. |
| THP | General support enabled; segmented-bitmap forcing disabled. Advice is not a promise of huge pages. |
| Memory-pressure relief | Disabled; default disable/re-enable thresholds are 85% / 80%. |
| Protection settings | Bootstrap-randomized magic values and aligned-allocation tag; foreign-pointer policy is abort when that path is reached. |

The initial bitmap footprint includes the normalized initial region, page-rounded region metadata, and page-rounded state/node/hint tables. Lower big-threshold requests are raised to this footprint-dependent minimum. Footprint or `+ 1` overflow logs `RSMallocError::Bootstrap` and aborts. Source: `src/backend/bootstrap.rs` — `PreparedBootstrapConfig::big_trim_threshold`; `src/big_allocations/segmented_bitmap.rs` — `initial_cached_bytes`.

Trimming reclaims eligible cached pages on a best-effort basis; dropping containers does not promise immediate return of all cached memory to the kernel. Avoid changing security-sensitive settings as routine tuning. Those settings require `expose-security-critical-settings` and unsafe acknowledgement, and foreign-pointer checks do not make invalid frees safe. See [Performance Tuning](performance-tuning.md) and [Security and Reliability](security.md).

## Alignment and Rust allocation contracts

`GlobalAlloc` requests with alignment up to 16 bytes use the normal allocation path; higher alignment uses the aligned path. Benchmark over-aligned workloads separately. Ordinary Rust containers handle their own layouts and initialization.

If you call `std::alloc::GlobalAlloc` methods yourself, obey Rust's contracts, including valid nonzero allocation layouts, matching deallocation layouts, and pointer lifetime rules. RSMalloc deriving metadata internally does not waive those contracts. Do not import the native API's null/zero resize semantics into `GlobalAlloc` calls.

## Nightly: allocator-aware collections

The optional `allocator-api` feature implements unstable `std::alloc::Allocator`. It is separate from stable global allocation and from the stable `native-allocation-api` feature.

Use this dependency configuration instead:

```toml
[dependencies]
rsmalloc = { version = "0.3.0-alpha", features = ["allocator-api"] }
```

Then build with `cargo +nightly run`:

```rust
#![feature(allocator_api)]

use rsmalloc::v2::alloc::RSMalloc;

static ALLOCATOR: RSMalloc = RSMalloc::new_default();

fn main() {
    let mut values = Vec::new_in(&ALLOCATOR);
    values.extend(0_u64..1024);
    assert_eq!(values.len(), 1024);
}
```

This selects RSMalloc for that collection without installing it globally. It still shares RSMalloc's process-wide state with other RSMalloc instances; it is not a private arena. Choose the [native allocation API](native-api.md) instead when stable Rust and explicit byte-buffer ownership are the actual requirements.
