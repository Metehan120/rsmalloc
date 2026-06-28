### TODO list for adding lacking features:
1. Add buddy allocator for Big Allocation path - Done
2. Check safety of AI written lines or rewrite entirely - only hashmap left
3. Add ABA Tags for MailCache - Done
4. Rewrite entire RSEQ path in Assembly - Planned after Trim
5. Find other way than HashMap for big allocations, maybe RB-Trees
6. Change L3 Radix with dynamic radix tree if possible - Done for alpha
7. Add small-allocation/background trimming thread; requested-size buddy trim is done
8. Add GlobalAlloc support - Done
9. Add NUMA-aware allocation paths
10. Audit entire allocator
11. Lock-free L3 Radix Tree - Done
