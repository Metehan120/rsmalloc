# Getting Started

The goal of a first trial is to establish compatibility and a clean baseline, not to find an aggressive configuration.

## 1. Check the platform

```sh
uname -srm
ldd --version
rustc --version
```

Expect Linux `x86_64` and libc-provided RSEQ registration, such as glibc 2.35+. Version output is useful context, not proof that registration is available in a particular process. Disabled RSEQ, unsupported libc layouts, or an incompatible execution environment can prevent bootstrap.

Stable Rust is sufficient unless you choose the optional nightly `allocator-api` interface. When building from this repository, review `.cargo/config.toml`: `target-cpu=native` makes artifacts specific to the build CPU's capabilities. Use suitable target flags before distributing them to other machines.

## 2. Choose one integration path

| Integration | Use when | Read next |
| --- | --- | --- |
| Rust global allocator | Ordinary `Box`, `Vec`, `String`, and collections in an application you build | [Rust global allocator](rust-global-allocator.md) |
| Native allocation API | You need explicit malloc-style buffers, alignment, and resizing without keeping a `Layout` | [Native and raw APIs](native-api.md) |
| `LD_PRELOAD` | You are testing allocator replacement in a compatible dynamically linked executable | [Preload](preload.md) |

For a Rust application:

```toml
[dependencies]
rsmalloc = "0.3.0-alpha"
```

```rust
use rsmalloc::v2::alloc::RSMalloc;

#[global_allocator]
static GLOBAL: RSMalloc = RSMalloc::new_default();

fn main() {
    let values: Vec<u64> = (0..1024).collect();
    assert_eq!(values.len(), 1024);
}
```

For a repository preload build:

```sh
cargo build --release --features preload
LD_PRELOAD=./target/release/librsmalloc.so your-program
```

Here `your-program` denotes your actual executable. The library path is relative to the launch directory; loading the library does not prove that every allocation call uses it. The [preload chapter](preload.md) explains routing and compatibility limits.

## 3. Validate before timing

Run your application's tests and representative workflows. Exercise:

- collection growth, reallocations, zero-size requests, and over-aligned objects;
- multithreaded startup, cross-thread ownership transfer, and shutdown;
- FFI and plugins, checking which allocator owns each pointer;
- repeated bursts, idle periods, and the largest expected live set;
- actual container memory limits and out-of-memory handling;
- loader and fork patterns where relevant, without assuming allocator fork handlers relax POSIX restrictions.

Use release builds for performance, and separate diagnostic builds for investigation. Never intentionally pass invalid pointers to production allocation APIs as a compatibility test.

## 4. Keep a rollback baseline

For preload, run the same executable without RSMalloc preloading. For a Rust global allocator, build a separate system-allocator variant with otherwise equivalent source and flags. Rust's selected global allocator is not switched by changing a preload variable alone.

Record wall-clock time, throughput, tail latency, CPU time, peak and post-idle RSS, page faults, thread count, CPU placement, kernel/libc, compiler flags, allocator revision, and features. Compare a cold start and a sustained warmed workload.

Only proceed if correctness and resource budgets hold. Read [workload fit](workload-fit.md) before tuning, and [benchmarking](benchmarking.md) before interpreting small differences.
