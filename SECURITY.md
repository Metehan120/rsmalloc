# Security Policy

rsmalloc is alpha-stage software. As a memory allocator, it sits on every `malloc`/`free` call in a process that links it, so a bug here can affect the security of anything built on top of it, up to and including memory corruption or code execution in the host application. Please report suspected vulnerabilities privately rather than opening a public issue.

## Supported versions

Only the latest `0.2.0-alpha` release on the `main`/`development` branches is supported. There is no LTS or backport policy at this stage; fixes land as new alpha releases.

| Version | Supported |
|---|---|
| `0.2.0-alpha` (latest) | Yes |
| `0.1.0-alpha` and earlier | No |

## Reporting a vulnerability

Email **metehanzafer@proton.me** with:

- A description of the issue and its impact (e.g. heap corruption, OOB read/write, double-free exploitation path, magic-value/metadata forgery, RSEQ/CPU-migration race, integer overflow in size computation).
- Steps to reproduce, or a minimal PoC (Rust `GlobalAlloc` usage or `LD_PRELOAD` reproduction is easiest to work with).
- Affected version/commit, build features (e.g. `preload`, `check-owned-on-alloc`), and target platform/kernel version.
- Whether you consider this exploitable beyond a crash (worth flagging explicitly, since allocator bugs are often severity-ambiguous until analyzed).

You should get an acknowledgment within a few days. Please give a reasonable amount of time to investigate and ship a fix before any public disclosure — happy to agree on a disclosure timeline once the report is triaged.

## Scope notes

- rsmalloc is explicitly alpha-quality with limited test coverage; correctness bugs found through fuzzing/stress testing that don't have a clear security impact are still welcome, but as regular issues/PRs rather than through this private channel.
- `disable-magic-security-checks` and other opt-in hardening-reduction features are intentional escape hatches for debugging/research, not vulnerabilities in themselves — reports assuming default configuration are most actionable.
- Foreign-pointer handling, double-free/corruption detection (magic values), and RSEQ critical-section correctness are the areas most likely to have real security impact; issues there are especially appreciated.
- Use-after-free is out of scope for this policy: rsmalloc cannot detect or prevent UAF on freed-and-reused memory by design (like most allocators), so UAF reports won't be treated as allocator vulnerabilities unless they stem from a specific allocator bug (e.g. premature reuse while still logically live).
