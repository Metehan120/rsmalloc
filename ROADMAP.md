# Roadmap

The planned path to a stable RSMalloc release prioritizes allocator correctness before API stabilization and broader architecture support. These milestones describe intended scope, not fixed release dates.

## Alpha-3 — Allocator correctness and hardening

- Focus on major allocator-correctness work and hardening.
- Strengthen validation of allocation, deallocation, reuse, and trimming behavior.
- Expand regression tests and concurrency/fuzz coverage as correctness issues are identified and fixed.

## Beta — API stabilization and NUMA correctness

- Stabilize the public API and its behavioral guarantees.
- Focus on NUMA correctness, including placement, cross-node allocation/free behavior, and fallback handling.

## Beta-2 — AArch64 support and final cleanup

- Add AArch64 support and validate architecture-specific behavior.
- Complete final implementation, API, and documentation cleanups ahead of stable release.

## Stable release

- Deliver the stable release after the correctness, API, NUMA, and AArch64 milestones have been validated.
