# Native and Raw APIs

Use these interfaces for explicitly managed buffers, allocator experiments, or an integration that genuinely needs malloc-style ownership. Prefer the [Rust global allocator](rust-global-allocator.md) for ordinary `Box` and `Vec` use: native pointers do not provide automatic cleanup, typed initialization, or borrow checking.

All examples here target **0.3.0-alpha**, Linux `x86_64`, and working libc RSEQ registration. The same platform and alpha limitations apply as for global allocation.

## Stable Rust: enable the native API

```toml
[dependencies]
rsmalloc = { version = "0.3.0-alpha", features = ["native-allocation-api"] }
```

`native-allocation-api` enables `rsmalloc::v2::allocation_api`. It does not require nightly or the separate `allocator-api` feature, and does not require `#[global_allocator]`. Do not combine this Rust integration with `preload`, which removes the `v2` module.

A static `RSMalloc` is an access point to shared allocator state, not an isolated heap. Its configuration wins only if it initializes first; subsequent instances do not reconfigure the process. Use one consistent `Config` across global, native, and raw use. See [process-wide configuration](rust-global-allocator.md#configuration-belongs-to-the-first-initializer).

## Byte counts are not layouts

`AllocationSize::from_bytes(bytes)` carries only a byte count. `AllocationSize::array_bytes::<T>(count)` checks `size_of::<T>() * count` for overflow and returns `SizeOverflow` on failure; it does **not** carry `T`'s alignment or initialize any values.

`allocate_aligned` takes a `NonZero<usize>`, not a plain `usize`. Nonzero alone is insufficient: the alignment must be a supported power of two. Ordinary allocation uses the allocator's 16-byte alignment; request explicit alignment for over-aligned data. Strong alignment can add padding and affect backend routing.

This complete example allocates aligned storage, initializes `u64` values before reading them, then resizes with a stronger alignment. It keeps the old pointer available for cleanup if resizing fails:

```rust
use std::{mem::align_of, num::NonZero};
use rsmalloc::v2::{
    alloc::RSMalloc,
    allocation_api::{AllocationAPI, AllocationError, AllocationSize},
};

static ALLOCATOR: RSMalloc = RSMalloc::new_default();

fn main() -> Result<(), AllocationError> {
    let count = 128;
    let size = AllocationSize::array_bytes::<u64>(count)?;
    let pointer = ALLOCATOR.allocate_aligned(
        size,
        NonZero::new(align_of::<u64>()).unwrap(),
    )?;

    unsafe {
        let values = pointer.as_ptr().cast::<u64>();
        for index in 0..count {
            values.add(index).write(index as u64);
        }

        let next = match ALLOCATOR.aligned_reallocate(
            pointer.as_ptr(),
            AllocationSize::from_bytes(2048),
            NonZero::new(64).unwrap(),
        ) {
            Ok(next) => next,
            Err(error) => {
                // A failed resize leaves the original allocation live.
                ALLOCATOR.deallocate(pointer.as_ptr());
                return Err(error);
            }
        };

        assert!(!next.is_null());
        assert_eq!((next as usize) % 64, 0);
        for index in 0..count {
            assert_eq!(next.cast::<u64>().add(index).read(), index as u64);
        }
        // The added bytes are uninitialized; do not read them yet.
        ALLOCATOR.deallocate(next);
    }
    Ok(())
}
```

This example uses trivially destructible values. For types with destructors, the caller must also arrange initialization tracking and dropping live values before releasing storage. Zero bytes are not a valid value of every Rust type.

## Operations and ownership

| Operation | Contract |
| --- | --- |
| `allocate`, `allocate_aligned` | Fallible allocation returning `NonNull<u8>` with uninitialized contents. |
| `allocate_zeroed` | Zeroes the requested bytes using default alignment; extra usable capacity is not guaranteed zeroed. |
| `allocate_zeroed_nmem` | Checks the product of its two byte-count tokens and zeroes that product. Both multiplication overflow and allocation failure currently report `SomethingWentWrong`. |
| `usable_size` | **Unsafe** query for the payload capacity of a live allocation; aligned results exclude the prefix before the returned pointer. |
| `deallocate` | **Unsafe** free taking `*mut u8`, with no original layout required. Null is a no-op. |
| `reallocate` | **Unsafe** resize preserving existing alignment. |
| `aligned_reallocate` | **Unsafe** resize satisfying at least the new requested alignment; may move just to improve alignment. |

Dropping `NonNull` or a raw pointer does not free the block. Retain an owner until explicit deallocation or a successful resize, and handle every early return. A reusable abstraction should provide RAII cleanup rather than scatter manual frees through application code.

For non-null existing pointers, pass only the **start of a currently live allocation from an equivalent RSMalloc instance**. Do not pass stack storage, interior pointers, freed pointers, or memory from libc or another allocator. `usable_size` returning `NotOwned` on some inputs is not a safe ownership test for arbitrary addresses. Do not assume `Box::from_raw` or `Vec::from_raw_parts` can adopt a native block: those constructors have their own allocator and layout contracts. For FFI, provide a matching RSMalloc release function instead of expecting the receiver to call its own `free`.

Only read initialized bytes. A usable-size result may exceed the requested count, but does not establish initialized values or update an existing Rust slice's bounds. Do not retain references or slices across a resize that may invalidate their storage.

### Resize success, failure, and zero size

These are the native API's rules, not interchangeable with Rust trait contracts or every libc's zero-size behavior:

| Input | Result and ownership |
| --- | --- |
| Live pointer, nonzero new size | Success returns a non-null raw pointer and invalidates the old pointer **even if the address is unchanged**. Preserves contents through the smaller old/new requested size. Failure leaves the old allocation live and unmodified. |
| Null pointer, nonzero new size | Allocation request. |
| Live pointer, zero new size | Frees it and returns `Ok(null_mut())`. Do not free the old pointer again. |
| Null pointer, zero new size | Returns `Err(AllocationError::NotSupported)`. |

`aligned_reallocate` validates alignment before its zero-size handling. An invalid alignment returns `InvalidAlignment` without freeing the old block, even for a zero-size request.

For a nonzero resize, never overwrite your only pointer before checking the result. For a zero resize, an `Ok` null result is deliberate deallocation, not out-of-memory. Zero-byte *allocation* has a different contract: it can return an error or a deallocatable non-null block; do not assume a dangling sentinel or permission to dereference it.

## Core operations: initialization and best-effort trim

`RSMallocCoreAPI` is available without `native-allocation-api`. Import the trait to call its methods:

```rust
use rsmalloc::v2::alloc::{RSMalloc, RSMallocCoreAPI, SimpleTrimSize};

static ALLOCATOR: RSMalloc = RSMalloc::new_default();

fn main() {
    ALLOCATOR.manual_init();
    match ALLOCATOR.trim(SimpleTrimSize::All) {
        Some(bytes) => println!("reclaimed {bytes} bytes"),
        None => println!("nothing reclaimed"),
    }
}
```

- `manual_init()` requests allocator-wide bootstrap ahead of ordinary allocation; it cannot override an earlier initializer or warm all allocation paths.
- `trim(SimpleTrimSize::All)` scans eligible slab caches, then segmented-bitmap blocks. `SimpleTrimSize::Bytes(NonZero<usize>)` requests a byte target. Both are best-effort; the result may fall short and live allocations remain valid. This is not a promise to unmap all cached VA or produce an equal RSS drop.
- `unsafe rs_usable_size(NonNull<u8>) -> Option<usize>` is another usable-capacity query with the same live-pointer and initialization responsibilities. It is **not safe** merely because the trait also contains safe maintenance methods.

Explicit trimming can be useful between workload phases. Trimming on every request or free can sacrifice reuse and add work; measure it rather than treating it as mandatory cleanup. See [Performance Tuning](performance-tuning.md).

## Raw interface: only when needed

`RSMalloc::raw()` returns a borrowed `RSMallocRaw` access point; import `RawInterface` from `rsmalloc::v2::alloc` to use it. No optional API feature is needed. It exposes unsafe `rs_alloc`, `rs_zeroed`, `rs_aligned`, `rs_realloc`, `rs_free`, and `rs_trim` operations. It creates neither an independent heap nor an automatic owner.

Raw allocation uses null for failure rather than `AllocationError`. `rs_realloc(old, size, None)` preserves existing alignment; `Some(alignment)` must be nonzero and a power of two. Invalid alignment or allocation failure returns null without freeing the old pointer. With valid alignment, a live pointer resized to zero is freed and also returns null: distinguish these cases using the request, not just the return value.

`AdvancedTrimSize` selects slab, segmented-bitmap, or combined trimming; `TrimReport` separates `slab_bytes` and `segmented_bitmap_bytes`. Prefer safe core trimming unless that backend-specific control is necessary. Prefer the fallible native API for managed byte buffers and ordinary Rust collections for typed application data. Neither raw access nor optional ownership-validation features make double frees, stale pointers, or allocator mixing safe; see [Security and Reliability](security.md).
