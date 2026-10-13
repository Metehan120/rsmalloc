# RSMalloc: Design, Use, and Tradeoffs

RSMalloc is an RSEQ-based allocator for Rust and dynamically linked C/C++ applications on Linux `x86_64`. Its slab caches belong to **CPUs rather than threads**. Freed blocks can be reused by the CPU running the freeing thread, or move through shared transfer caches to another CPU with demand.

That design can help concurrent reuse, but it is not a promise that more threads always make RSMalloc faster. Cache hits still perform metadata work and a locked usage update; cache misses, migration, contention, and reclamation have additional costs. Choosing an allocator means choosing tradeoffs in throughput, latency, memory use, and deployment compatibility.

## Status and support

**Version: `0.3.0-alpha`. Not production-ready or comprehensively audited.** APIs and internal policies can change.

- Linux `x86_64` is the supported platform.
- Stable Rust supports the global allocator, native allocation API, and preload build.
- Only the optional `allocator-api` interface requires nightly.
- Libc must provide registered RSEQ TLS, such as glibc 2.35+. RSMalloc reads libc's RSEQ offset and size; it does not register RSEQ itself.

Unsupported libc/RSEQ configurations are a bootstrap failure, not an automatic portable fallback. Test your exact kernel, libc, binary, loader, and features. A reported `SIGBUS` association with kernel `7.0.10` is covered in [troubleshooting](troubleshooting.md); it is not proof of a kernel or allocator bug.

## How to read this guide

| Your question | Start here |
| --- | --- |
| Should I try RSMalloc for this application? | [Workload fit and failure modes](workload-fit.md) |
| How does the allocator work? | [Allocation lifecycle](allocator-design.md) |
| How do I integrate it? | [Getting started](getting-started.md), then the Rust, native, or preload chapter |
| What are the pointer and reliability rules? | [Security and reliability](security.md) |
| How do I reduce memory or improve throughput? | [Performance tuning](performance-tuning.md), after measuring defaults |
| How do I judge a result or diagnose a regression? | [Benchmarking](benchmarking.md), [diagnostics](diagnostics.md), and [troubleshooting](troubleshooting.md) |

## Detailed architecture

Start with [How the Allocator Works](allocator-design.md) for the allocation lifecycle and source reading map, then explore the implementation by subsystem:

1. [Frontends and bootstrap](architecture-bootstrap.md) — public interfaces, process-wide initialization, configuration, and startup failures.
2. [RSEQ and CPU-local slab caches](architecture-rseq.md) — cache layouts, assembly commits, abort handling, and overflow.
3. [Transfer caches and reclamation](architecture-transfer.md) — tagged heads, batch exchange, search hints, NUMA reuse, and trim interaction.
4. [Refill and deferred initialization](architecture-refill.md) — predictor equations, pending spans, and claim/publication rules.
5. [Backing memory and large allocations](architecture-backing-memory.md) — arenas, bitmap regions, direct mappings, and large-object metadata.
6. [Ownership, alignment, and resizing](architecture-ownership.md) — headers, ownership checks, free, calloc, realloc, and fork behavior.
7. [Reclamation and pressure relief](architecture-reclamation.md) — eligible pages, background scheduling, manual trim, and memory-pressure policy.
8. [Concurrency and observation limits](architecture-concurrency.md) — authoritative versus advisory state, current implementation caveats, and diagnostic semantics.

These chapters describe the current alpha implementation with source-path and symbol references. They explain both intended mechanisms and known limitations; they are not a correctness audit or a stable internal API specification.

## The deployment contract

Use one allocator consistently for each allocation's lifetime. The native API and raw pointers require explicit ownership discipline; selecting a Rust global allocator does not make foreign FFI pointers Rust-owned. Hardening checks do not turn invalid pointers into valid ones.

Configuration is **process-wide and first-initializer-wins**. A second `RSMalloc` value is not a separate heap, memory quota, or live reconfiguration mechanism. Preload environment variables and Rust `Config` are separate frontend configuration mechanisms.

Start with defaults, retain a system-allocator baseline, and evaluate complete application outcomes. A throughput improvement that causes unacceptable tail latency, memory pressure, or compatibility failures is not a successful deployment.
