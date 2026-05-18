### TODO list for adding lacking features:
1. Add buddy allocator for Big Allocation path - WIP
2. Check safety of AI written lines or rewrite entirely
3. Add ABA Tags for MailCache
4. Find other way than HashMap for big allocations, maybe RB-Trees
5. Change L3 Radix with dynamic radix tree if possible
6. Add trimming logic and thread
7. Add GlobalAlloc support
8. Audit entire allocator
