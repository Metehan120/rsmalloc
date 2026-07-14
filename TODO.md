### TODO list for adding lacking features:
1. Add buddy backend for Big Allocation path - Done
2. Check safety of AI written lines or rewrite entirely - only hashmap left
3. Add ABA tags for TransferCache - Done
4. Rewrite entire RSEQ path in Assembly - Cancelled
5. Find other way than HashMap for big allocations, maybe RB-Trees - Planned for next alpha release
6. Change RADIX with dynamic radix tree if possible - Done for alpha
7. Add small-allocation/background trimming thread - Done for Alpha-2
8. Add GlobalAlloc support - Done
9. Add NUMA-aware allocation paths - Things like buddy paths left for cross numa free stability
10. Audit entire allocator
11. Lock-free RADIX tree - Done
12. Lock-free buddy if possible / For stable release
13. Add special benchmark to stress test every subsystem at once - Done
