### TODO list for adding lacking features:
1. Add buddy allocator for Big Allocation path - Done
2. Check safety of AI written lines or rewrite entirely - only hashmap left
3. Add ABA Tags for MailCache - Done
4. Rewrite entire RSEQ path in Assembly - Cancelled
5. Find other way than HashMap for big allocations, maybe RB-Trees
6. Change L3 Radix with dynamic radix tree if possible - Done for alpha
7. Add small-allocation/background trimming thread - Done for Alpha-2
8. Add GlobalAlloc support - Done
9. Add NUMA-aware allocation paths - Started development for Alpha-2 release
10. Audit entire allocator
11. Lock-free L3 Radix Tree - Done
12. Lock-free buddy if possible / For stable release
13. Add special benchmark to stress test every subsystem at once - Done

## Beta Target: Add page/span-backed small-class allocator for <4096B reclaim
- Keep rseq cache API as object-list based if possible.
- Move refill/trim backing from linear MetaData spans to page/span metadata.
- Goal: allow page/span-level reclaim for small classes without touching rseq asm.
- Note: main architecture was designed around this assumption in case we need to switch to spans later.
