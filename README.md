# rsmalloc

# ⚠️ Warning ⚠️: **Due to great and noble moderators of r/rust, I had to make this repo public before even become stable enough across various loads and missing implemention of GlobalAlloc interface & Trim (both of them non-exist) also Big Allocation path not ready yet. You can watch the development of this project via this repo. Thank you for understanding**.

# ⚠️ Warning ⚠️: **Current developement is extremely early stage and unstable as it gets. Never use it in production**
## ⚠️ Warning ⚠️: **Current code is lack of documentation and comments, you have to understand it by reading the source code for now. Thanks for understanding**

### What rsmalloc is? 
- rsmalloc is a memory allocator for general purpose use in across various loads and designed to handle massive concurrent allocations.
- It is designed to be fast, scalable, and efficient, with a focus on concurrent safety and low overhead.

### What makes rsmalloc different?
1. It's use of RSEQ as main core for slab allocation.
2. Due to usage of RSEQ rsmalloc is essentially per-CPU allocator not per-Thread so its scaling with core count not threads.

### How core allocator works and what is the architecture?
1- Architecture and folder structure:
  - abi: LD_PRELOAD interface:
    - align.rs: memalign implementations
    - calloc.rs: calloc implementation: zero-initialized
    - free.rs: free & free_sized implementation
    - malloc.rs: malloc & malloc_usable_size implementation
    - realloc.rs: realloc, reallocarray, recalloc & reallocarray implementation
  - core_prim: bootstrap etc., main primitives of allocator:
    - bootstrap.rs: initialization and setup
    - wrapper.rs: wrapper for pointers, semi-type-safe interface with zero overhead
  - inner: inner allocator function implementation, e.g: alloc, free
    - align.rs: posix_memalign & memalign implementation
    - calloc.rs: memory allocation zero-initialized
    - free.rs: memory deallocation
    - malloc.rs: memory allocation
    - realloc.rs: memory reallocation
    - libc_int: libc interface for LD_PRELOAD scenarios
    - fallback: fallback to next loaded library for LD_PRELOAD scenarios
  - internals: main data structures and state management:
    - env: environment variable parsing
    - hashmap: hashmap implementation for big allocations
    - l3_main_radix: L3 main radix tree implementation for fast ownership checks
    - lock.rs: Spinlock implementation for concurrent access on slow paths
    - once.rs: Once implementation with atomics for fork-safety
    - once_lock.rs: OnceLock implementation with atomics for thread-safe lazy initialization while keeping fork-safe
  - rseq_core: RSEQ core implementation:
    - bulk_core.rs: per-Thread bulk allocation core
    - bulk_fill.rs: main bulk fill implementation
    - rseq_cache.rs: RSEQ cache implementation alongside MailCache
    - rseq_core.rs: RSEQ core implementation, assembly codes
    - rseq_main.rs: glibc interface
    
2. How RSEQ works, assembly breakdown: Let's give a example with push.
  ```rust
        let res: usize;
        let cs = get_cs_ptr(rseq);

        asm!(
            // Push the section to the read-only data segment to avoid write access, initialized at compile-time
            ".pushsection .data.rel.ro,\"aw\",@progbits",
            ".balign 32",
            "4:",
            // Zero-initialized space for the section
            ".long 0, 0",
            ".quad 1f",
            // Abort section
            ".quad 2f - 1f",
            ".quad 3f",
            ".popsection",

            // Load the section pointer into the cs register so kernel can abort us
            // 
            // using lea to get the section pointer relative to the current instruction pointer with zero overhead
            "lea {tmp}, [rip + 4b]",
            "mov [{cs_ptr}], {tmp}",

            "1:",
            // Move list's head to temporary register
            "mov {tmp}, [{list}]",
            // Move temporary register to header and increment usage count
            "mov [{header}], {tmp}",
            "inc qword ptr [{usage}]",
            // Move header back to list's head
            "mov [{list}], {header}",

            "2:",
            // Clear the cs pointer to prevent kernel from aborting us
            "mov qword ptr [{cs_ptr}], 0",
            // Return success
            "mov {res}, 1",
            // Return to caller
            "jmp 5f",

            // Padding to align to 16 bytes
            ".balign 4",
            // NOPs to pad to 16 bytes
            ".byte 0x0f, 0x1f, 0x05",
            // RSEQ signature
            ".long 0x53053053",
            "3:",
            // Zeroize the cs pointer, kernel aborted us
            "mov qword ptr [{cs_ptr}], 0",
            // Return restart
            "mov {res}, -1",

            "5:",

            cs_ptr = in(reg) cs,
            tmp = out(reg) _,
            list = in(reg) list_ptr,
            header = in(reg) header,
            res = out(reg) res,
            usage = in(reg) usage_ptr,
            options(nostack, preserves_flags),
        );
  ```
  - As you can see from the code this is how RSEQ actually works in practice.
  - We have CS (Critical Section) and a Signature so kernel can abort us if thread preempted.
  - Linked list for O(1) speeds.
  - And zeroing CS pointer when we are done or kernel abort us.

## Contributing
Contributions are welcome!

### You only have to follow these 3 rules for contributions:
1. You can use AI tools but please mention which AI tool you used, why you used and how you prove the code is safe.
2. Use rust-fmt for formatting.
3. Do not bloat the code. Keep it as lean as possible.

Now that you know the rules, please open an issue or submit a pull request on GitHub.
