# RSMalloc Architecture

This document is a working architecture draft for `rsmalloc` `0.2.0-alpha`. It describes the allocator as it exists today, not as a final stable design. Some pieces are intentionally experimental and may change before a production-ready release.

## Design Goal

`rsmalloc` is built around a simple idea:

> Allocation ownership is temporary and follows the hot CPU cache, not the thread or the original allocation source.

For small allocations, the allocator uses Linux Restartable Sequences (RSEQ) to manipulate CPU-local caches with very little synchronization overhead. Larger allocations use a separate mapping/buddy path because they have different locality, metadata, and trimming requirements.

## High-Level Layout

```mermaid
flowchart TD
    API[Public entry points]
    ABI[C ABI / preload mode]
    GLOBAL[Rust GlobalAlloc mode]
    INNER[inner allocation operations]
    SMALL[small allocation path]
    RSEQ[RSEQ per-CPU caches]
    REFILL[bulk refill / EMA prediction]
    PENDING[per-node pending metadata queue]
    TLS[thread-local pending metadata]
    NUMA[NUMA topology and preferred node policy]
    BIG[big allocation path]
    BUDDY[NUMA-aware buddy allocator cache]
    RADIX[L3 radix ownership map]
    MAP[big allocation metadata map]

    API --> ABI
    API --> GLOBAL
    ABI --> INNER
    GLOBAL --> INNER
    INNER --> SMALL
    INNER --> BIG
    SMALL --> RSEQ
    SMALL --> REFILL
    REFILL --> TLS
    TLS --> REFILL
    TLS --> PENDING
    PENDING --> REFILL
    REFILL --> RSEQ
    NUMA --> RSEQ
    NUMA --> REFILL
    NUMA --> PENDING
    NUMA --> BUDDY
    NUMA --> BIG
    BIG --> BUDDY
    BIG --> MAP
    SMALL --> RADIX
    BIG --> RADIX
```

Main source areas:

| Area | Role |
| --- | --- |
| `src/abi` | C ABI entry points for `LD_PRELOAD` builds. |
| `src/global_alloc.rs` | Rust `GlobalAlloc` integration, Rust-facing configuration, capabilities, stats, and direct helper methods. |
| `src/inner` | Shared allocation operations: alloc, free, realloc, calloc, alignment, fallback/free handling. |
| `src/rseq_core` | RSEQ cache layout, inline assembly critical sections, bulk refill metadata, pending queue, RSEQ TLS access. |
| `src/big_allocations` | Big allocation path and NUMA-aware buddy allocator. |
| `src/internals` | Radix ownership map, big allocation map, NUMA parsing/binding helpers, locks, once primitives, env parsing. |
| `src/core_prim` | Bootstrap, fork handling, predictor state, pointer wrappers. |
| `src/utility.rs` | Size classes, refill targets, lookup tables, shared helpers. |

## Allocation Classes

Small allocations are size-classed up to `2 MiB`.

Current size classes live in `src/utility.rs` and are grouped approximately as:

- tiny: `16`, `32`, `48`, `64`, `80`, `96`, `128`
- small: `160`, `192`, `256`, `320`, `384`, `512`
- medium: `768`, `1024`, `1280`, `1536`, `1792`, `2048`, `2560`, `3072`
- larger small/slab classes: `3840`, `4096`, `8192`, `12288`, `16384`, `24576`, `32768`, ... up to `2097152`

`match_size_class(size)` uses a fast LUT for sizes `<= 4096` and a simple slow scan above that. A request of size `0` maps to the smallest class. Requests above the largest small class fall through to the big allocation path.

Each allocated block has a `Header` placed before the returned payload. Header layout is deliberately constrained because the RSEQ assembly and list operations depend on `Header::next` being where the list code expects it.

## Bootstrap

Initialization differs slightly between preload and Rust global allocator modes, but both set up the same core allocator state.

Bootstrap initializes:

1. RSEQ availability through libc-provided RSEQ TLS state.
2. Runtime knobs:
   - `RS_MAX_REFILL_RETRIES`,
   - `RS_EMA_ALPHA`,
   - `RS_PREDICTOR_INIT_BATCH`,
   - buddy cache size / THP / trim options in preload mode.
3. `L3_RADIX`, the ownership map.
4. `RSEQ_CACHE`, the per-CPU cache array and NUMA topology snapshot.
5. `PENDING_QUEUE`, the per-node/per-class global pending metadata queue.
6. `BIG_BUDDY_ALLOCATOR`, when configured.
7. Fork handlers for preload/fallback state.
8. Randomized magic values and aligned-allocation tag, unless randomization is explicitly disabled.

In non-preload Rust mode, this is driven through `RSMalloc::init()` from `global_alloc.rs`. In preload mode, C ABI entry points bootstrap on first use.

## Small Allocation Fast Path

The fast path for a small allocation is:

```mermaid
flowchart TD
    A["rs_alloc(size)"] --> B{"match_size_class?"}
    B -- "no" --> BIG["big_malloc"]
    B -- "yes" --> C["RSEQ_CACHE.pop(class)"]
    C -- "hit" --> D["mark MAGIC"]
    C -- "miss" --> E["fill(class)"]
    E --> F{"local mail hit?"}
    F -- "yes" --> D
    F -- "no" --> N["same-node victim scan"]
    N --> R{"remote scan if NUMA?"}
    R -- "hit" --> D
    R -- "miss" --> G["bulk_fill"]
    G --> P{"thread/local-node pending metadata?"}
    P -- "hit" --> I["initialize batch"]
    P -- "miss" --> M["mmap and prefer current node"]
    M --> I
    I --> H["push remainder to RSEQ cache"]
    H --> D
    D --> RET["return payload"]
```

Important details:

- The hot path tries `RSEQ_CACHE.pop(class)` first.
- RSEQ pop/push uses inline assembly critical sections.
- If the thread is migrated or preempted inside an RSEQ critical section, the kernel aborts the sequence and control jumps to the abort handler.
- After a few RSEQ aborts, code falls back to mailbox paths rather than spinning forever.
- The per-CPU `usage` counter is an approximate pressure signal, not exact accounting. Stale-low drift is preferred over stale-high drift because stale-high pushes too much traffic into mailboxes.

## RSEQ Cache Layout

`RSEQ_CACHE` owns an mmap-backed array of `MainCache`, one per configured CPU plus one extra overflow/fallback slot. With the default `rseq-thread-failure-fallback` feature, invalid or unregistered RSEQ CPU IDs use that extra slot instead of indexing CPU-local state directly.

```rust
#[repr(C, align(4096))]
pub struct MainCache {
    cache: [ClassCache; NUM_SIZE_CLASSES],
    mail: [SelfMail; NUM_SIZE_CLASSES],
}
```

The 4096-byte alignment is intentional. It keeps each CPU's cache structure page-separated, which reduces false-sharing risk and leaves room for future NUMA-aware policy.

### `ClassCache`

`ClassCache` is the primary per-CPU freelist for one size class:

- `list`: RSEQ-managed linked list of free `Header`s.
- `usage`: approximate pressure counter.

### `SelfMail`

`SelfMail` is the overflow and fallback queue for one CPU/class pair:

- used when a per-CPU cache is over pressure limits,
- used when RSEQ retry count is exceeded,
- used by victim stealing when the local CPU has no cached block,
- used by the default RSEQ thread-failure fallback when the kernel/libc reports an invalid CPU ID.

The mail list uses an ABA-tagged pointer word. It is still a fallback/pressure path, not the ideal hot allocation path. Victim scans are NUMA-aware: local CPU mail is tried first, then CPUs in the same node range, then remote node ranges when NUMA is active.

`SelfMail` is expected to remain fast enough for overflow, fallback, and occasional cross-CPU recovery. Normal traffic should mostly hit the RSEQ cache, but occasional mail usage should not meaningfully slow the allocator down. If a workload spends a large fraction of time in `SelfMail`, that usually points to refill pressure, cache sizing, migration, or workload shape rather than `SelfMail` being intrinsically too slow.

## Refill Path

When RSEQ cache and mailbox/victim stealing cannot satisfy an allocation, `fill()` calls `refill()`, which calls `bulk_fill()`.

`bulk_fill()` maps a slab-like chunk:

```text
[ MetaData ][ Header + payload ][ Header + payload ] ...
```

`MetaData` tracks:

- pending queue link,
- mapping start,
- mapping end,
- next uninitialized block position,
- NUMA node id for the mapping.

Blocks are initialized lazily in batches. The initialized batch is returned to allocation code, one block is used immediately, and the remainder is pushed into the per-CPU RSEQ cache.

### Thread-Local Pending Metadata

Default refill behavior uses thread-local pending metadata:

- if a mapped slab has uninitialized blocks left after a refill batch, the leftover `MetaData` is stored in `THREAD_BULK.free[class]`,
- the next refill for the same thread/class continues initializing from that pending metadata,
- `ThreadBulk::get_or_init(class)` lazily registers the thread cleanup hook on first refill use,
- a thread-exit destructor drains pending metadata into the global pending queue so another thread on the same NUMA node can continue initializing it later.

The cleanup hook is intentionally off the allocation hot path after first touch. It registers a low-level thread-exit callback for the TLS destructor slot because raw `#[thread_local]` storage is used for allocator state.

This model avoids a shared refill lock on the hot path while reducing stranded pending refill state when threads exit.

### Global Pending Metadata Queue

`PENDING_QUEUE` is a global lock-free Treiber stack of pending `MetaData` pages, indexed by NUMA node and size class:

```text
[node_id][class] -> MetaData stack
```

The queue is initialized during `RSEQ_CACHE` setup from the parsed NUMA topology. On non-NUMA systems, all traffic uses node slot `0`. On NUMA systems, each `MetaData` carries its original `node_id`, thread-exit drain pushes to that node's stack, and `alloc_metadata()` only pops from the current CPU's local node before mapping fresh memory.

Queue heads are ABA-tagged in the low 12 bits. This relies on `MetaData` being placed at the base of an mmap-backed page-aligned mapping. The queue is a cold/slow refill structure, not part of the normal RSEQ block pop/push path.

## EMA Refill Prediction

Small refill sizes are adaptive instead of fixed. The goal is to avoid two bad extremes:

- refilling too little, which causes repeated slow-path trips and extra RSEQ/mailbox traffic,
- refilling too much, which increases virtual-memory retention, cache/TLB pressure, and cross-CPU spillover.

There are two independent thread-local, per-size-class predictors:

- `PREDICTOR`: predicts how many blocks allocation code should try to pull from cache/mail/victim sources before returning one block to the caller and pushing the rest locally.
- `BULK_FILL_PREDICTOR`: predicts how many blocks a fresh or pending bulk-fill slab should initialize at once.

They are separate because the two costs are different. Pulling already-initialized blocks mostly changes cache pressure and local freelist depth; initializing from bulk metadata touches fresh memory, writes headers, and may expose more mapped pages to the working set.

Current predictor state is intentionally small:

```rust
struct Predictor {
    ema: f32,
    batch: usize,
    once: Once,
    is_fill: bool,
    _class: usize,
}
```

The predictor is initialized lazily on first use. Normal cache/mail prediction starts from `PREDICTOR_INIT_BATCH` (`RS_PREDICTOR_INIT_BATCH` in runtime config paths). Bulk-fill prediction starts from `BULK_FILL_PREDICTOR_INIT_BATCH`. `EMA_ALPHA` controls responsiveness and currently defaults to `0.15`.

The update rule is the standard exponential moving average:

```text
ema_next = alpha * observed_demand + (1 - alpha) * ema_old
batch_next = ceil(ema_next).clamp(1, ITERATIONS[class])
```

`ITERATIONS[class]` is the hard per-class maximum derived from refill target bytes and block size. This keeps predictor output bounded even if a workload keeps asking for more.

### Observed Demand

The predictor does not observe application allocation demand directly. It observes what the allocator managed to obtain during a refill step:

- if only a few blocks were available, the observation is small and future batches shrink gradually,
- if the requested batch was fully satisfied, that is treated as a signal that demand may be at least as large as the request.

The second case matters because a plain EMA can get stuck too low. Example: if `batch == 8` and every refill gets exactly 8 blocks, feeding `8` back into the EMA forever never lets the predictor discover that the workload could use `16`, `32`, or more.

To avoid that, when a refill returns exactly the requested batch and the class still has headroom, the observed value is lifted by `+25%` before updating the EMA:

```text
if returned == requested && requested < ITERATIONS[class]:
    observed = min(requested + max(requested / 4, 1), ITERATIONS[class])
else:
    observed = returned
```

This is deliberately conservative. It lets the predictor climb out of too-small batches during sustained pressure, but avoids doubling into large over-refills after one successful refill.

### Why EMA Instead Of Instant Batch Changes?

Refill behavior is noisy:

- RSEQ aborts and mailbox pressure can make one refill look artificially small,
- victim/mail hits can temporarily hide true demand,
- bursty workloads may allocate heavily for a short phase and then stop,
- thread migration means CPU-local cache state is not a perfect demand signal.

EMA smooths those transient observations. With `alpha = 0.15`, recent refill results matter, but one odd refill does not immediately rewrite the batch size. The tradeoff is that the predictor reacts over multiple refill calls rather than instantly.

### Debugging Prediction Quality

Debug features provide approximate and exact prediction miss accounting:

- `debug` gives low-overhead approximate over/under-prediction counters suitable for normal benchmark runs,
- `debug-predictor-exact` probes more aggressively to classify misses more accurately and is higher overhead,
- `predictor-debug` can print predictor batch choices for direct inspection.

The counters should be interpreted as tuning signals, not correctness requirements. Under-prediction usually means more refill trips. Over-prediction usually means more retained/free cached memory. The right balance depends on workload locality and whether the benchmark is latency-, throughput-, RSS-, or TLB-sensitive.

### Current Limitations

- Predictors are thread-local, not global. This avoids atomics on the hot path but means new threads start from initial settings.
- The predictor sees allocator-side refill results, not future application demand.
- The `+25%` uplift helps sustained full-batch pressure, but it is still intentionally slower than an aggressive doubling strategy.
- Very synchronized refill storms can still bottleneck in the refill path, but pending refill metadata stays thread-local by default to avoid shared refill locks.

## Free Path

Freeing follows this shape:

```mermaid
flowchart TD
    A["rs_free(ptr)"] --> B{"null?"}
    B -- "yes" --> RET["return"]
    B -- "no" --> C{"L3_RADIX owns ptr?"}
    C -- "no" --> F["foreign pointer handling"]
    C -- "yes" --> D["find_original_ptr"]
    D --> E["read Header"]
    E --> S{"header magic"}
    S -- "MAGIC" --> P["mark FREED_MAGIC and push"]
    S -- "BIG_MAGIC" --> Q["big_free"]
    S -- "FREED_MAGIC / corrupt" --> R["abort unless magic disabled"]
```

After the radix ownership check, `rs_free()` calls `find_original_ptr()` before reading the header. Normal pointers pass through unchanged. Aligned allocations may return an interior aligned pointer, so `find_original_ptr()` checks for the randomized alignment tag stored just before the returned aligned address and recovers the original allocation pointer.

Rust `GlobalAlloc::dealloc` currently delegates to the normal `rs_free` path. Preload `free_sized` and `free_aligned_sized` are compatibility shims over normal `free`.

## Aligned Allocations

The alignment path overallocates enough space for:

- requested payload,
- alignment slop,
- a tag and original-pointer slot.

The returned aligned pointer has metadata immediately before it:

```text
[ original allocation ... ][ ALIGN_TAG ][ original_ptr ][ aligned payload ]
```

The tag is randomized at bootstrap unless randomization is disabled. Free/realloc/usable-size paths recover the original pointer through this tag.

## Realloc Path

`rs_realloc` handles several cases:

- `null` pointer -> allocate,
- new size `0` -> free and return null,
- aligned pointer -> allocate with observed alignment, copy, free original,
- small allocation shrinking within class -> return same pointer,
- small allocation growing within class -> return same pointer,
- small allocation growing across classes -> allocate/copy/free,
- large small-class slab mappings may try in-place `mremap` when mapping shape allows,
- direct big allocations may try in-place `mremap`,
- buddy-backed big allocations may try in-place buddy growth before falling back to allocate/copy/free.

## Big Allocation Path

Requests that do not match a small size class use `big_malloc()`.

Big allocation behavior:

1. Add `Header::SIZE`.
2. Align mapping size to 4096, or to 2 MiB when close enough and THP is enabled.
3. If buddy allocator is initialized and the original request is `<= 64 MiB`, try `BIG_BUDDY_ALLOCATOR`; internally the buddy path rounds up to at least the `4 MiB` minimum order.
4. Otherwise mmap directly.
5. Write a `BIG_MAGIC` header.
6. Record metadata in the big allocation map keyed by payload pointer.
7. Mark ownership in `L3_RADIX`.

Direct big allocations are unmapped on free. Buddy allocations are returned to the buddy pool.

## Buddy Allocator

The buddy allocator caches large regions for big allocations.

Current range:

- min order: `22` (`4 MiB`),
- max order: `26` (`64 MiB`).

Each region has:

- region metadata,
- base address and total size,
- NUMA node id,
- free lists for each order,
- a per-region nonempty bitmap for quick order selection,
- a region lock.

Allocation first tries regions on the caller's local NUMA node, grows a new local-node region if needed, and only then scans remote node ranges when NUMA is active. Within a region, `nonempty_mask` skips empty order lists and selects the first usable order with bit operations instead of linearly probing every order. Freeing coalesces with free buddies where possible and keeps the bitmap in sync.

`trim(requested_size)` uses `madvise(MADV_DONTNEED)`-style advice on free buddy blocks. `requested_size == 0` means trim all currently free buddy blocks; nonzero requests trim until at least the requested byte target is reached or no more eligible free blocks remain. The trim path takes the global trim lock and each region's free-list lock while advising blocks so allocation/free do not race with page advice over the same free lists.

## NUMA Awareness

NUMA topology is parsed from Linux sysfs into `NumaTopology`:

- `cpu_to_node[cpu] -> node_id`,
- compact `node_ids`,
- `cpu_ranges[node_id] -> NumaCpuRange` for fast range lookup by node id.

Invalid or missing CPU entries fall back to node `0`. Sparse node ids are supported by sizing the CPU range table so direct `cpu_ranges[node_id]` indexing remains valid.

NUMA policy is applied at mapping time using `prefer_node(ptr, len, node_id)`, currently implemented through the `syscalls` crate's `mbind` syscall wrapper with `MPOL_PREFERRED | MPOL_F_STATIC_NODES`. This sets preferred placement; it does not pre-touch pages or force migration of already-faulted pages.

Current NUMA-aware paths:

- RSEQ victim stealing prefers same-node CPUs before remote node ranges.
- Small refill `mmap` calls prefer the current CPU's node.
- Pending metadata queues are per-node/per-class and only pop local-node metadata.
- Buddy regions are tagged with node id, prefer-node bound when mapped, and allocated local-node first.
- Direct big allocation fallback prefers the current CPU's node.

## Ownership Tracking: `L3_RADIX`

`L3_RADIX` is the allocator ownership map. It answers: "does this address look owned by rsmalloc?"

Uses:

- distinguish rsmalloc pointers from foreign pointers,
- decide whether free/realloc/usable-size should use internal logic or fallback/abort,
- mark small slab mappings,
- mark direct and aligned big mappings,
- mark buddy regions.

The radix implementation is a lazy multi-level bitmap tree covering the low canonical 56-bit user address range used on x86-64 LA57 systems. It uses 4 KiB chunks, an 8-bit top level, two 12-bit pointer levels, and a 12-bit bitmap leaf. Range marking validates overflow and bounds explicitly instead of wrapping indices.

The radix implementation uses acquire/release atomics for reader/writer synchronization. Writers mutate under `SerialLock` and publish new radix nodes or bitmap updates with release operations; readers use acquire loads and may observe either the old or new ownership state during a race. The allocator only requires eventual visibility here, not a perfectly up-to-date ownership snapshot.

## Trim

Trimming is best-effort and uses `madvise` to return physical page pressure to the kernel while keeping allocator virtual mappings and cache structure intact.

There are two trim sources:

- explicit trim through `malloc_trim(...)` in preload mode or `RSMalloc::rs_trim(...)` in Rust mode,
- the optional background trim worker.

Both paths share a global non-blocking trim lock. If another trim pass is already active, the new trim attempt returns without waiting. This avoids overlapping manual trim, background trim, small-cache trim, and buddy trim passes.

### Small-allocation trim

Small-allocation trim scans the RSEQ mail caches for size classes equal to or greater than `4096` bytes. Smaller classes are intentionally left to normal reuse because they do not have enough page-aligned interior payload to make page advice useful.

For each CPU and eligible size class:

1. The class mail list is detached under that mail list's trim lock.
2. Detached blocks are inspected outside the mail lock.
3. Blocks older than the current average lifetime and marked trim-eligible are passed to `release_memory(...)`.
4. Blocks are pushed back into the target mail cache in small batches.

`release_memory(...)` only advises the page-aligned interior of a block:

```text
[ header ][ unaligned payload prefix ][ page-aligned trim range ][ unaligned suffix ]
```

This means small trim avoids corrupting allocator metadata or partial user-cache-line fragments. Successful trim clears the block's trim eligibility until it is reused/freed again. The trim pass also updates an average lifetime estimate so background trim adapts to observed cache age.

### Buddy trim

Buddy trim scans free buddy blocks. Each free block tracks:

- lifetime stamp,
- trim state: never allocated, allocated/reused, or trimmed.

Manual buddy trim can force trimming up to a requested byte target. Background buddy trim only trims blocks older than the buddy average lifetime. Buddy trim holds the global trim lock and the region free-list lock while walking free lists so allocation, free, coalescing, and page advice do not race over the same region state.

Buddy trim advises all but the first page of each free block. The first page remains resident because it stores the free-list node metadata. Trim accounting and trim-state updates are only applied when `madvise` succeeds.

### Lazy vs eager page trim

Without `lazy-page-trim`, trim uses `MADV_DONTNEED`-style advice (`Advice::LinuxDontNeed`), so advised pages fault back as zero-filled memory.

With `lazy-page-trim`, trim uses lazy free advice (`Advice::LinuxFree`). Lazy-free pages may retain old contents until the kernel reclaims them, so calloc paths must still zero memory that came from lazy-trimmed blocks.

Allocation headers carry zero/reuse/trim state flags so `calloc` can skip zeroing only when the allocator can prove the returned payload is already zeroed.

## Public Modes

### Preload Mode

Enabled with `preload` feature.

Provides C ABI symbols such as:

- `malloc`, `calloc`, `free`, `realloc`,
- `reallocarray`, `recallocarray`,
- `posix_memalign`, `memalign`, `aligned_alloc`, `valloc`, `pvalloc`,
- `malloc_usable_size`,
- `malloc_trim`,
- sized-free compatibility shims.

Foreign pointers can fall back to libc behavior in preload mode where fallback support is compiled.

### Rust Global Allocator Mode

Default non-preload mode exposes:

- `RSMalloc`,
- `RSMallocConfig`,
- `GlobalAlloc` implementation,
- raw helper methods (`rs_malloc`, `rs_free`, `rs_realloc`, etc.),
- capabilities snapshot,
- debug stats when enabled.

Foreign pointer behavior is configured through `ForeignPointerSettings`; global allocator mode defaults toward aborting on foreign pointers unless configured otherwise.

## Feature Flags

Important architecture-affecting features:

| Feature | Effect |
| --- | --- |
| `preload` | Builds C ABI / preload support. |
| `rseq-thread-failure-fallback` | Enables the default overflow-slot recovery path for invalid/unregistered RSEQ CPU IDs. |
| `extended-header` | Uses wider metadata. |
| `debug` | Enables low-overhead stats/debug counters. |
| `debug-exact` | Adds exact global lock counters and debug printing. |
| `debug-predictor-exact` | Uses higher-overhead exact refill prediction miss accounting. |
| `lazy-page-trim` | Uses lazy page-free advice for trim paths instead of eager `MADV_DONTNEED`-style advice. |

## Known Architectural Tradeoffs

- The small allocation fast path is optimized around CPU-locality and RSEQ, not thread ownership.
- Victim stealing currently scans CPU mailboxes and is intentionally simple, with NUMA-local ranges preferred before remote ranges.
- The extra RSEQ cache slot is reserved for fallback/overflow handling, not normal CPU-local traffic.
- `SelfMail` is a relief valve and fallback path; too much traffic there usually means refill/capacity pressure should be inspected.
- Thread-local pending refill metadata avoids shared refill locks but can temporarily strand pending slabs until reuse or thread-exit drain. Drained metadata enters the per-node global pending queue, not RSEQ mailboxes.
- The big allocation map is an internal hashmap and is planned for future replacement.
- Buddy trimming uses `madvise`, not `munmap`, so it returns physical pressure to the kernel while keeping the virtual region structure.
- NUMA policy is preferred placement rather than guaranteed physical placement; first-touch behavior still matters.

## Allocation Lifecycle Summary

```mermaid
sequenceDiagram
    participant User
    participant Inner as inner::*
    participant RSEQ as RSEQ_CACHE
    participant NUMA as NUMA topology
    participant Pending as PENDING_QUEUE
    participant Refill as bulk_fill
    participant Big as big_malloc/buddy
    participant Radix as L3_RADIX

    User->>Inner: malloc / GlobalAlloc::alloc
    Inner->>Inner: match_size_class
    alt small allocation
        Inner->>RSEQ: pop class
        alt cache hit
            RSEQ-->>Inner: Header
        else miss
            RSEQ->>NUMA: find local node/ranges
            Inner->>RSEQ: try_pop local mail and node-local victims
            alt mail or victim hit
                RSEQ-->>Inner: batch
            else refill
                Inner->>Refill: bulk_fill class
                Refill->>NUMA: current CPU node
                Refill->>Pending: pop local-node metadata
                alt pending miss
                    Refill->>Refill: mmap and prefer node
                    Refill->>Radix: mark slab owned
                end
                Refill-->>Inner: initialized batch
                Inner->>RSEQ: push remainder
            end
        end
        Inner-->>User: payload
    else big allocation
        Inner->>NUMA: current CPU node
        Inner->>Big: big_malloc
        Big->>Big: try local buddy then remote fallback
        Big->>Radix: mark mapping/region owned
        Big-->>Inner: payload
        Inner-->>User: payload
    end
```

## Things To Verify / Review

This draft intentionally leaves a few review points explicit:

- whether the current low-level TLS destructor registration for pending refill drain is final,
- whether the current size class set and refill byte targets are final enough to document as stable,
- whether buddy trim should be documented as API-stable or explicitly experimental,
- whether future small-class page/span metadata is needed for reclaim below 4096 bytes.
