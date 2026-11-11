# Security Policy

rsmalloc is alpha-stage software, not production-ready, and has not been comprehensively audited. Bugs in an allocator can affect the security of its host application, including memory corruption or code execution. Hardening features and successful test/fuzz runs do not establish memory safety. Please report suspected vulnerabilities privately rather than opening a public issue.

## Supported versions

Only the latest `0.3.0-alpha` release on the `main`/`development` branches is supported. There is no LTS or backport policy at this stage; fixes land as new alpha releases. Include an exact commit when reporting a development build, since branch contents and feature behavior can change.

| Version | Supported |
|---|---|
| `0.3.0-alpha` (latest) | Yes |
| `0.2.0-alpha` and earlier | No |

## Ownership contract

- Return an allocation to the allocator that created it, using the appropriate API. Installing a Rust global allocator or preloading rsmalloc does not prove that every pointer in the process came from rsmalloc; plugins, shared libraries, and explicit allocator APIs can use other allocation sources.
- A non-null pointer passed for deallocation or reallocation must identify a live allocation at the address returned by the allocation API. Interior pointers, already-freed pointers, and forged pointers are invalid inputs. Follow the relevant Rust layout and alignment contracts when calling `GlobalAlloc` or `Allocator` methods.
- Usable-size queries are not arbitrary-pointer validation. They require a valid allocation pointer under the selected API's contract; additional reported capacity is not automatically initialized.
- Do not access an allocation after freeing it or successfully reallocating it. A failed nonzero reallocation leaves the original allocation live. Follow each interface's zero-size rules rather than assuming Rust and C interfaces have identical behavior.

### Default free: metadata first

The default Cargo feature set is empty. For `free` and the corresponding Rust deallocation paths, rsmalloc assumes non-null inputs are live rsmalloc allocations:

1. Null is a no-op. Non-null addresses at or above `2^56` abort before metadata reads; this range check does not establish mapping, ownership, or allocation validity.
2. Free reads the aligned-allocation tag and presumed header before checking radix ownership. Matching live magic selects the small- or large-allocation free path without an upfront ownership lookup.
3. If live magic does not match, the cold path performs an ownership lookup before foreign-pointer handling or corruption reporting.

**Foreign-pointer handling is best-effort in this default mode.** A foreign pointer can fault during the earlier reads or unrelated bytes can be misclassified before any foreign-pointer policy or fallback is reached. An abort policy is not a guarantee that every invalid pointer is safely recognized and rejected.

For Rust v2, the default policy is to abort when a pointer is detected as foreign. Preload mode instead attempts libc fallback for pointers classified as foreign. This fallback is compatibility machinery, not permission to pass arbitrary pointers or mix allocation APIs without an ownership contract. Ignoring a detected foreign pointer does not free it or transfer ownership.

### Opt-in ownership-first free

`validate-foreign-first` performs radix ownership classification before free reads presumed rsmalloc alignment/header metadata. It also checks a recovered aligned-allocation base before dereferencing its header; a non-owned recovered base aborts as possible metadata injection. This feature is disabled by default and included in `extra-checks`.

The radix records ownership at **4 KiB granularity**, not exact allocation starts, bounds, or live-object identity. An interior or stale pointer into an owned page can pass this check. A valid recovered page address also does not prove that an alignment prefix is authentic or that the recovered pointer is an allocation start.

Neither ownership mode makes invalid frees, double frees, use-after-free, or arbitrary-pointer probing safe. Ownership-first validation changes the order and coverage of checks; it is not a complete spatial or temporal memory-safety mechanism.

## Security defaults and additional checks

Default bootstrap randomizes header magic values and the aligned-allocation tag. Magic-based checks can detect some double frees and corruption, but they are metadata checks, not cryptographic authentication. The normal header uses 16-bit magic values; `extended-header` widens them to 64 bits. Reuse can make a stale address refer to a different live allocation with valid metadata.

| Feature | Protection and limitations |
|---|---|
| `validate-foreign-first` | Coarse ownership checks before free metadata reads and on recovered aligned bases. Does not establish allocation boundaries or liveness. |
| `check-owned-on-alloc` | Coarse ownership checks on blocks selected by internal allocation paths. Does not validate the entire freelist or protect every earlier metadata access. |
| `extended-header` | Widens headers and magic values. Does not prevent payload overflows from reaching metadata or establish complete corruption detection. |
| `zero-small-on-free` | Clears payloads in the 16, 32, 48, and 64 byte size classes. Does not clear all allocation sizes or provide a general secret-erasure guarantee. |
| `extra-checks` | Enables the four features above. Provides additional checks and small-free zeroing; does not make rsmalloc a hardened allocator or establish production readiness. |
| `explicit-zero` | Uses `explicit_bzero` for explicit calloc zeroing where needed. Does not enable general zero-on-free. |
| `abort-on-rseq-failure` | Adds an abort check for the sampled uninitialized RSEQ CPU-ID sentinel. Does not provide RSEQ registration or make unsupported RSEQ environments usable. |

Hardening features can affect allocation footprint, memory use, and throughput; evaluate the actual enabled combination. The allocator does not provide general guard-page isolation, quarantine-based UAF protection, or cryptographically authenticated freelists. Mapping reuse and arena bump allocation are not randomized object-placement guarantees.

### Configuration and protection-reduction controls

Rust v2 keeps security-sensitive settings separate from ordinary tuning. Its defaults randomize magic values and select foreign-pointer abort handling. `expose-security-critical-settings` exposes overrides; applying them through `Config::with_security_critical` requires an unsafe acknowledgement, and fixed magic additionally requires `MagicSafetyDisable::acknowledge_safety_risk()`.

The deprecated Rust configuration API and preload environment settings have their own controls. In particular, preload's `RS_DISABLE_RANDOMIZING` disables magic/tag randomization when set to a nonzero value. `disable-magic-security-checks` reduces magic-based validation independently of the v2 configuration gates. Do not assume enabling `extra-checks` cancels another enabled protection-reduction setting.

Fixed magic values, ignored foreign pointers, and disabled magic checks are deliberate escape hatches for controlled debugging/research, not fixes for allocator failures. Their intentional availability is not itself a vulnerability. Do not suppress an unexplained crash by disabling these checks and then treat the result as evidence that the underlying issue is resolved.

Configuration is process-wide: the first Rust initializer's settings win, and preload environment settings are read during bootstrap rather than updated live. Report the configuration actually applied, including whether the legacy or v2 Rust interface was used.

## Reporting a vulnerability

Email **metehanzafer@proton.me** with:

- A description of the issue and suspected impact, such as heap corruption, out-of-bounds access, premature reuse, metadata forgery, RSEQ/CPU-migration errors, or size-computation overflow.
- Reproduction steps or a minimal PoC when available. A source-level finding with a precise call path or concurrency interleaving is also useful; do not publish exploit details to obtain a reproducer.
- The affected version/commit, compiler/profile, target architecture, kernel/libc versions, integration mode (`GlobalAlloc`, native/raw API, or `LD_PRELOAD`), and enabled Cargo features.
- Ownership-related settings, especially `validate-foreign-first`, `check-owned-on-alloc`, `extra-checks`, `disable-magic-security-checks`, and any security overrides or preload environment changes.
- Whether the issue occurs with valid allocator inputs, requires application misuse or weakened settings, or appears exploitable beyond a crash. If this is uncertain, say so; a working exploit is not required for a report.
- Relevant crash traces, logs, or minimized fuzz inputs. Review core dumps and other artifacts for application secrets before sharing them.

You should get an acknowledgment within a few days. Please give a reasonable amount of time to investigate and ship a fix before public disclosure; a disclosure timeline can be agreed once the report is triaged.

## Scope notes

- Allocator-caused memory corruption, overlapping live allocations, premature reuse, invalid reclamation of live storage, and correctness failures in ownership, metadata, synchronization, or size handling can have security impact and are in scope.
- Caller-originated double frees, invalid frees, out-of-bounds accesses, and use-after-free are application bugs. Failure to detect every such misuse is not itself an allocator vulnerability unless it violates an explicit protection guarantee or reveals a separate allocator defect. In particular, rsmalloc does not distinguish freed-and-reused storage from the new allocation solely by its address.
- Reports under protection-reduction features are still useful, but include those settings and explain which allocator invariant is violated. Default-configuration reports are especially actionable; configured reductions alone are not vulnerabilities.
- If security impact is suspected or uncertain, use the private reporting channel. Ordinary correctness reports without suspected security impact, performance regressions, and test/coverage improvements can use regular issues or PRs.
