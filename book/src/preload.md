# LD_PRELOAD

Preloading lets you evaluate RSMalloc's **0.3.0-alpha** C allocation frontend without rebuilding a dynamically linked application. It is useful for controlled comparisons of malloc-heavy workloads, but it is not a guarantee of drop-in compatibility or improved performance.

## Requirements and boundaries

Use Linux `x86_64` with working libc-provided RSEQ TLS. The project describes glibc 2.35+ or equivalent support; RSMalloc reads libc's `__rseq_size` and `__rseq_offset` rather than registering RSEQ itself. Unavailable RSEQ state aborts bootstrap. Test on the deployment environment, not just the development machine.

`LD_PRELOAD` works through the dynamic loader's symbol resolution. It does not replace allocation in fully static binaries, custom heaps, direct `mmap` calls, or code that bypasses interposable symbols. Secure-execution contexts such as set-user-ID programs restrict preload handling. Symbol binding, other preloads, plugins, and separate loader namespaces can change which allocator handles a call.

For a Rust application that should select its own allocator, prefer the [Rust global allocator](rust-global-allocator.md). `preload` selects a different frontend: it hides the `v2` Rust module and is not a feature to add to the Rust integration examples.

## Build and launch

From the repository root, on stable Rust:

```sh
cargo build --release --features preload
LD_PRELOAD="$PWD/target/release/librsmalloc.so" /usr/bin/printf 'preload smoke test\n'
```

For an actual workload, replace `/usr/bin/printf` and its arguments with the program you want to test. `$PWD` makes the library path absolute when launched from the repository root. Use that resolved absolute path in a service manager or when launching from a different directory; ensure both `LD_PRELOAD` and tuning variables reach the workload process. Child processes can inherit them, so keep experiments scoped to one command rather than exporting preload into your whole shell session.

Cargo emits the shared library because the crate declares `cdylib` alongside `rlib` and `staticlib`. No nightly feature is required. The repository's `.cargo/config.toml` sets `target-cpu=native`: repository builds are optimized for the build CPU. Review or override that configuration before distributing a library to other CPUs.

To remove this integration, run the command without this `LD_PRELOAD` assignment. Do not load or unload the allocator midway through a workload and assume outstanding pointers can migrate to another allocator.

## Confirm loading and call routing

For a long-running process, replace `12345` with its actual PID:

```sh
grep -F librsmalloc.so /proc/12345/maps
```

For a short smoke test, glibc loader diagnostics can show loading and symbol bindings:

```sh
LD_DEBUG=libs,bindings LD_PRELOAD="$PWD/target/release/librsmalloc.so" /usr/bin/printf 'loader test\n'
```

Inspect bindings for calls actually made by the workload. A mapping proves the library is loaded, **not** that every allocation reaches it. Loader diagnostics can be noisy and are not suitable for timing runs. A `debug-preload` build can also provide allocator diagnostics in controlled tests; use a non-debug release build for measurements. See [Diagnostics and Statistics](diagnostics.md) and [Benchmarking Correctly](benchmarking.md).

## C surface and compatibility limits

The preload frontend exports:

- `malloc`, `calloc`, `realloc`, `reallocarray`, and `recallocarray`;
- `free`, `free_sized`, and `free_aligned_sized` (the sized shims ignore the size/alignment arguments and call `free`);
- `posix_memalign`, `memalign`, `aligned_alloc`, `valloc`, and `pvalloc`;
- `malloc_usable_size` and `malloc_trim`.

Do not infer support for every libc allocation extension, statistics API, hook, or allocator-specific behavior from this list. In this source version, `malloc_trim` is exported directly by the preload frontend; it does not require a separate trim feature. Its argument is a requested reclamation amount, **not glibc's retained top-chunk padding**. It trims eligible segmented-bitmap and slab pages and returns 1 if bytes were reclaimed, otherwise 0. It does not guarantee an exact RSS reduction.

`aligned_alloc` checks a nonzero power-of-two alignment and requires size to be a multiple of alignment. `posix_memalign` requires a power-of-two alignment at least pointer-sized, returns an error code, and leaves the output pointer untouched on failure. Check errors before using an aligned allocation.

For nonzero `realloc`, keep the old pointer until success: failure returns null and leaves the original block live. A live pointer resized to zero is freed and returns null; do not mistake that result for a failure requiring another free. `reallocarray` checks multiplication overflow. `recallocarray` also requires an accurate old element count: it zeroes added bytes and explicitly clears discarded storage. Do not use usable capacity as a substitute for that old logical size.

`malloc_usable_size` is for a live allocator-owned allocation. A result larger than the requested size does not mean that the extra bytes are initialized, nor is the call a safe probe for arbitrary pointers.

## Runtime configuration: set it before bootstrap

Preload builds read `RS_*` variables once during allocator bootstrap, which can happen before application `main`. Later `setenv` calls do not reconfigure initialized state. Rust allocator builds instead use the process-wide `Config` described in the [Rust chapter](rust-global-allocator.md#configuration-belongs-to-the-first-initializer).

**Use decimal integers, with sizes in bytes—not `MiB`, `MB`, or `true`/`false`.** The current parser consumes leading decimal digits and ignores trailing nondigits; an empty value, whitespace, sign, or nonnumeric first character becomes zero. Defaults apply only when a variable is absent. For example, `64MiB` parses as **64 bytes**, and `RS_ENABLE_RELIEF=false` parses as zero and **enables** relief. Keep values within `usize` range; the parser is not a validated, overflow-reporting configuration format.

### Reuse and backing memory

| Variable | Default | Meaning |
| --- | --- | --- |
| `RS_ARENA_SIZE` | `268435456` (256 MiB) | Minimum slab arena setting; bootstrap clamps it to at least 256 KiB. Not a process memory limit. |
| `RS_PREDICTOR_INIT_BATCH` | `128` | Initial transfer predictor batch, subject to class limits; distinct from fresh-block refill prediction. |
| `RS_MAX_REFILL_RETRIES` | `3` | Maximum refill retries. |
| `RS_SEGMENTED_BITMAP_PER_CACHE_SIZE` | `67108864` (64 MiB) | Initial region size, clamped to 64 MiB–128 TiB and rounded up to a power of two. Later growth adds 64 MiB regions; this is not a total-cache cap. |

### Trimming and pressure relief

| Variable | Default | Meaning |
| --- | --- | --- |
| `RS_DISABLE_TRIM_THREAD` | `0` | Nonzero disables periodic background trimming, not manual trimming or all worker activity. |
| `RS_TRIMMER_THRESHOLD` | `10485760` (10 MiB) | Cached small-allocation VA threshold for starting the worker. |
| `RS_BIG_TRIMMER_THRESHOLD` | `536870912` (512 MiB) | Requested cached segmented-bitmap VA threshold for starting the worker; bootstrap raises it to at least `initial_cached_bytes + 1`. |
| `RS_ENABLE_RELIEF` | `1` (disabled) | **Exactly zero enables relief; nonzero disables it in this alpha.** |
| `RS_SEGMENTED_BITMAP_RELIEF_DISABLE_PERCENTAGE` | `85` | Pressure percentage disabling segmented-bitmap allocation; capped at 100. |
| `RS_SEGMENTED_BITMAP_RELIEF_ENABLE_PERCENTAGE` | `80` | Re-enable percentage; capped at the disable threshold, with two qualifying checks required. |

The effective big threshold is `max(requested, initial_cached_bytes + 1)`. The footprint includes the normalized initial region, page-rounded region metadata, and page-rounded state/node/hint tables. Lower requests are raised to that minimum; footprint or `+ 1` overflow logs `RSMallocError::Bootstrap` and aborts. Source: `src/backend/bootstrap.rs` — `PreparedBootstrapConfig::big_trim_threshold`; `src/big_allocations/segmented_bitmap.rs` — `initial_cached_bytes`.

The trim thresholds measure cached **virtual address space**, not RSS and not cgroup limits. Disabling periodic trim does not prevent the worker from starting or handling enabled relief. Relief uses system-wide RAM-plus-swap usage from `sysinfo`, not `MemAvailable` or a container's memory budget. It disables segmented-bitmap allocation under pressure and trims that backend; direct mappings remain a fallback. It is not an out-of-memory prevention guarantee.

### Huge pages and protection

| Variable | Default | Meaning |
| --- | --- | --- |
| `RS_SEGMENTED_BITMAP_ATTEMPT_HUGEPAGE` | `0` | Nonzero requests THP for segmented-bitmap regions when general THP is enabled. |
| `RS_DISABLE_THP` | `0` | **Exactly `1`** disables allocator THP attempts; other values do not. |
| `RS_DISABLE_RANDOMIZING` | `0` | **Any nonzero value weakens protection** by retaining fixed magic values and the fixed aligned-allocation tag. Leave unset for normal use. |

THP advice is not a guarantee that the kernel uses huge pages. Build-time page-backend features and kernel policy also matter; see [Performance Tuning](performance-tuning.md).

For a scoped experiment disabling periodic trim, while retaining the default-disabled relief state:

```sh
RS_DISABLE_TRIM_THREAD=1 RS_ENABLE_RELIEF=1 \
LD_PRELOAD="$PWD/target/release/librsmalloc.so" /usr/bin/printf 'configuration smoke test\n'
```

This shows valid syntax, not a recommended performance setting. It does not promise a thread-free allocator. Use an allocation-heavy application and change one setting at a time for meaningful comparisons.

## Ownership and process boundaries

A safe integration pairs allocation and release in the same allocation domain. Preload has best-effort libc fallback paths for foreign pointers, but that is **not** permission to mix allocators: default free handling can read alignment/header metadata before ownership classification, so foreign or invalid pointers can fault or be misclassified before fallback.

`validate-foreign-first` (also included in `extra-checks`) adds ownership checks before those metadata reads. Ownership is tracked at 4 KiB region granularity, not at individual allocation boundaries. It does not make interior pointers, double frees, or stale pointers safe. See [Security and Reliability](security.md).

Before deployment, test the real application with particular attention to:

- plugins, other allocator preloads, private allocation entry points, and ownership passed across shared-library boundaries;
- dynamic loading and unloading, startup and shutdown allocations, and unusual libc extensions;
- multithreaded `fork` and at-fork handler ordering. The preload backend registers fork handlers, but this is not a blanket fork-safety or async-signal-safety guarantee;
- memory limits, THP/NUMA policy, and workloads depending on exact libc trim or usable-size behavior.

Treat preload as an experiment first. Compare correctness, startup, throughput, tail latency, and memory retention against a no-preload baseline, and confirm allocation routing before interpreting benchmark results. Use [Troubleshooting](troubleshooting.md) when loading or bootstrap fails.
