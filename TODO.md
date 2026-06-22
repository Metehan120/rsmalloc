### TODO list for adding lacking features:
1. Add buddy allocator for Big Allocation path - Done
2. Check safety of AI written lines or rewrite entirely - Mostly complete
3. Add ABA Tags for MailCache - Done
4. Rewrite entire RSEQ path in Assembly - Planned after Trim
5. Find other way than HashMap for big allocations, maybe RB-Trees
6. Change L3 Radix with dynamic radix tree if possible - Done for alpha
7. Add small-allocation/background trimming thread; requested-size buddy trim is done
8. Add GlobalAlloc support - Done
9. Audit entire allocator
