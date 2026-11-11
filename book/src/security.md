# Security and Reliability

**RSMalloc `0.3.0-alpha` is alpha-quality software, not production-ready.** Test coverage and deployment experience are limited, and the allocator has not been comprehensively audited. Allocator bugs can corrupt memory or compromise the host application. Hardening features do not change that status.

## Proper allocator use

Start with defaults and the integration appropriate to your application: [Rust Global Allocator](./rust-global-allocator.md), [Native and Raw APIs](./native-api.md), or [LD_PRELOAD](./preload.md).

- Return an allocation to the allocator that created it. Document ownership across FFI, plugins, shared libraries, and custom allocation APIs; do not assume process-wide interposition covers every allocation source.
- Only deallocate a live allocation at the pointer returned by its allocation API. Interior pointers, stale pointers, and repeated frees are invalid. Follow the Rust API's layout and alignment contract when using unsafe allocation interfaces.
- Do not access freed storage, even if it remains mapped or cached. Reuse can make a stale pointer refer to a different object.
- Configure before initialization. Rust instances share allocator state; the first initializer's configuration wins. Preload environment settings are read at bootstrap, not updated live.
- Test the actual deployment: libc and loader, thread creation, multithreaded fork, plugins, signals, CPU restrictions, and workload memory limits. The presence of fork handlers is not a compatibility guarantee.

Linux `x86_64` with working libc-provided RSEQ TLS is the current deployment contract. RSMalloc uses libc's `__rseq_size` and `__rseq_offset`; it does not register RSEQ itself. Unsupported or disabled RSEQ is not a supported alternative fast-path mode. See [Troubleshooting](./troubleshooting.md) for startup and crash evidence.

## Secure defaults

Bootstrap randomizes header magic values and the aligned-allocation tag. Rust allocator mode defaults to aborting when a foreign pointer reaches the foreign-pointer policy. Magic-based double-free and corruption checks are enabled unless explicitly disabled.

**That policy does not mean every invalid free is safely rejected.** The default free path assumes every non-null input is a live RSMalloc allocation:

1. It rejects addresses at or above `2^56`. This is an address-range check, not proof of mapping or ownership.
2. It reads the alignment tag and header metadata before ownership classification. Matching live magic takes the small or large free path without an upfront ownership lookup.
3. If live magic does not match, it checks coarse ownership before foreign-pointer fallback/policy or corruption handling.

A foreign pointer can fault during the preceding reads, or unrelated bytes can be misclassified. Preload's libc fallback and Rust's foreign-pointer policy are therefore **best-effort in the default mode**, not a license to mix allocators.

### Ownership-first validation

`validate-foreign-first` checks radix ownership before free reads presumed RSMalloc metadata. It also validates a recovered aligned-allocation base before reading its header; a non-owned recovered base aborts as possible metadata injection. The feature is opt-in and included in `extra-checks`.

The radix tracks **4 KiB regions**, not allocation boundaries or live-object identity. A pointer into an owned region can pass this check without being a valid allocation pointer. Neither mode makes arbitrary pointers, interior pointers, double frees, or use-after-free safe. Without ownership-first validation, aligned free additionally relies on valid alignment metadata when recovering the original base.

Magic checks can catch some corruption and double frees; they are not complete temporal or spatial memory safety. In particular, freed-and-reused storage cannot be distinguished from a new live allocation solely by its address.

## Additional checks and security features

All Cargo features are disabled by default. Select protections for testing and measure their cost separately from the uninstrumented default build.

| Feature | Protection and limit | Cost to evaluate |
| --- | --- | --- |
| `validate-foreign-first` | Coarse ownership checks before free metadata reads, including recovered aligned bases; not allocation-boundary validation. | Additional ownership lookups on free. |
| `extended-header` | Wider allocation headers and magic values; not comprehensive corruption detection. | More metadata and changed allocation footprint. |
| `check-owned-on-alloc` | Checks ownership of blocks before returning them from internal allocation paths. | Allocation-path ownership lookups. |
| `zero-small-on-free` | Clears payloads in the 16–64 byte size classes. It does not clear all allocations or provide a general secret-erasure guarantee. | Free-path stores. |
| `extra-checks` | Bundles the four features above. Additional checks and small-free zeroing do not make RSMalloc a hardened allocator. | Combined costs; benchmark the bundle as its own configuration. |
| `explicit-zero` | Uses `explicit_bzero` for calloc zeroing where needed; not general zero-on-free. | Zeroing-path cost. |
| `abort-on-rseq-failure` | Aborts when a sampled RSEQ CPU ID is `u32::MAX`. It does not supply RSEQ support or make unsupported environments safe. | An additional RSEQ-state check. |

There are no `guard-pages-thp` or `guard-pages-ignore-thp` Cargo features in this release. Do not rely on guard-page protection from this allocator.

Do not suppress a correctness failure by enabling `disable-magic-security-checks`, fixing magic values, or ignoring foreign pointers. These are protection-reduction escape hatches, not repairs. `expose-security-critical-settings` exposes advanced Rust configuration that can weaken protection; preload's `RS_DISABLE_RANDOMIZING` can disable magic/tag randomization independently. Keep the actual features and settings in incident reports.

## Heap randomization

Page arenas use anonymous mappings with a null address hint, allowing Linux mmap ASLR to choose their bases. Reservations within an arena use a deterministic atomic bump pointer. This does not randomize every returned block or prevent an attacker from learning reuse patterns. Randomized magic/tag values are metadata checks, not a promise of randomized object placement.

## Operational guidance

Keep the allocator commit/version, enabled features, compiler/profile, configuration, and matching symbols reproducible. Repository builds use `target-cpu=native`; review CPU compatibility before deploying the resulting binary elsewhere.

Exercise failures as well as steady state: allocation pressure, allocator-boundary mistakes, thread and process lifecycle changes, and bursts followed by idle periods. Compare [diagnostic builds](./diagnostics.md) with the original reproducer, since instrumentation changes timing and can hide races. Successful stress runs are useful evidence, not proof of safety.

For suspected vulnerabilities, follow the repository's `SECURITY.md`: report privately to **metehanzafer@proton.me** with a minimal reproducer, impact, affected commit/version, build features (especially `validate-foreign-first`), integration mode, kernel/libc, and crash evidence. The policy supports only the latest `0.3.0-alpha` release on `main`/`development`, with no LTS or backport commitment. Ordinary correctness reports without a suspected security impact can use the regular issue channel. Review core dumps and logs for secrets before sharing them.
