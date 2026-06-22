# Contributing to rsmalloc

Thanks for your interest in contributing to rsmalloc.

rsmalloc is an experimental allocator, so changes should be small, focused, and easy to reason about. Prefer correctness and debuggability over cleverness unless the performance win is measured and clearly justified.

## Before opening a PR

Please try to run the relevant checks locally:

```sh
cargo test
cargo test --features preload
```

If a check cannot be run on your machine, mention that in the PR.

## Contribution guidelines

- Keep changes focused; avoid unrelated rewrites in the same PR.
- Be careful around unsafe code, pointer arithmetic, metadata layout, RSEQ sections, and preload ABI behavior.
- Prefer existing internal patterns before adding new abstractions or dependencies.
- Document behavior changes that affect allocation, free, realloc, alignment, THP, buddy-cache behavior, or foreign-pointer handling.
- Add tests or a reproducible preload/global-allocator run when changing allocator behavior.
- Do not make stable-performance claims without reproducible benchmarks.

## Safety-sensitive changes

For changes touching allocator internals, please include a short note covering:

```md
### Safety notes

- What invariant does this change rely on?
- What pointer ownership/layout assumptions are involved?
- How was the change tested?
- Does this affect preload mode, Rust `GlobalAlloc` mode, or both?
```

## AI-assisted contributions

AI-assisted contributions are allowed, but contributors should review and understand the generated code. If AI tools were used for allocator logic, mention the tool and summarize how the result was checked.
