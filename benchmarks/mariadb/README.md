# MariaDB allocator memory benchmark

This harness compares allocator memory behavior in a real multithreaded database server. It runs the same MariaDB datadir snapshot with the system allocator or an allocator injected through `LD_PRELOAD`, drives it with sysbench, and samples Linux process memory throughout warmup, read/write traffic, and an idle period.

The primary comparison is **PSS and private/anonymous memory**, not virtual address space. RSMalloc deliberately reserves page arenas and segmented-bitmap regions, so `VmSize` can be substantially larger without an equivalent resident-memory cost.

## Why MariaDB

MariaDB is a useful allocator workload because one server process combines:

- many concurrent connection threads;
- temporary query and transaction objects;
- long-lived engine caches;
- cross-thread and phase-dependent allocation traffic;
- mixed startup, steady-state, and idle behavior.

It is not a pure allocator benchmark. InnoDB, the kernel page cache, storage, and query execution also affect results. The harness fixes the buffer-pool size and restores the same prepared datadir for each variant to reduce those differences.

## Requirements

Install:

- MariaDB server and client tools;
- `mariadb-install-db`;
- sysbench with the `oltp_read_only` and `oltp_read_write` Lua workloads;
- Linux `/proc` with `smaps_rollup` support.

The exact package names depend on the distribution. The script does not use or modify the system MariaDB service.

If commands use different names, override them:

```sh
MARIADBD=mysqld \
INSTALL_DB=mysql_install_db \
MARIADB=mysql \
MARIADB_ADMIN=mysqladmin \
./run.sh prepare
```

## Build RSMalloc

From the repository root:

```sh
cargo build --release --features preload
```

The resulting preload library is normally:

```text
target/release/librsmalloc.so
```

Use an optimized non-debug build for memory and throughput comparisons. Diagnostic features change timing and can allocate memory for reporting.

## Prepare the database

Run once:

```sh
cd benchmarks/mariadb
./run.sh prepare
```

By default this creates an isolated template under:

```text
/tmp/rsmalloc-mariadb-bench/template
```

The template contains eight tables with 250,000 rows each. Every allocator run receives a reflink copy when the filesystem supports it, or an ordinary copy otherwise.

## Run allocator variants

System-default allocator:

```sh
./run.sh run system
```

RSMalloc:

```sh
./run.sh run rsmalloc ../../target/release/librsmalloc.so
```

Mimalloc:

```sh
./run.sh run mimalloc /absolute/path/to/libmimalloc.so
```

The script records `/proc/<pid>/maps` and warns if a requested preload library is not visible. Check `maps.txt` when MariaDB was built with, or starts through, an allocator/runtime arrangement that may defeat symbol interposition.

## Default workload

Each run uses:

1. a fresh copy of the prepared datadir;
2. MariaDB startup with a 256 MiB InnoDB buffer pool;
3. 30 seconds of read-only warmup;
4. 60 seconds of read/write OLTP with 32 client threads;
5. 15 seconds idle, allowing allocator trimming behavior to appear;
6. clean server shutdown.

The database listens only on a private Unix socket and starts with `--skip-networking`.

Override parameters through the environment:

```sh
THREADS=64 \
TABLES=16 \
TABLE_SIZE=500000 \
WARMUP_SECONDS=60 \
RUN_SECONDS=180 \
IDLE_SECONDS=30 \
BUFFER_POOL_SIZE=512M \
./run.sh run rsmalloc ../../target/release/librsmalloc.so
```

Important variables:

| Variable | Default | Meaning |
| --- | ---: | --- |
| `WORK_ROOT` | `/tmp/rsmalloc-mariadb-bench` | Datadirs, sockets, and results root. |
| `TABLES` | `8` | Number of sysbench tables prepared. |
| `TABLE_SIZE` | `250000` | Rows per table. |
| `THREADS` | `32` | Concurrent sysbench client threads. |
| `WARMUP_SECONDS` | `30` | Read-only warmup duration. |
| `RUN_SECONDS` | `60` | Read/write measurement duration. |
| `IDLE_SECONDS` | `15` | Post-workload idle measurement. |
| `SAMPLE_INTERVAL` | `0.20` | `/proc` sampling interval in seconds. |
| `ADMIN_TIMEOUT` | `10` | Maximum seconds allowed for the shutdown client itself. |
| `SHUTDOWN_TIMEOUT` | `30` | Seconds to wait for the server before terminating it. |
| `BUFFER_POOL_SIZE` | `256M` | Fixed InnoDB buffer pool. |
| `MAX_CONNECTIONS` | `256` | MariaDB connection limit. |

Use the same values for every allocator variant.

## Results

Each variant writes to:

```text
/tmp/rsmalloc-mariadb-bench/results/<label>/
```

The script also prints the latest RSS, PSS, anonymous, private, and swap sample after each phase. Files include:

| File | Contents |
| --- | --- |
| `memory.csv` | Time series for RSS, high-water RSS, virtual size, PSS, anonymous, private, and swap memory. |
| `memory-summary.csv` | Per-phase peaks extracted from the time series. |
| `workload.txt` | Sysbench throughput and latency report. |
| `warmup.txt` | Warmup report. |
| `maps.txt` | Server mappings, used to verify allocator injection. |
| `mariadb.log` | Server log. |
| `environment.txt` | Parameters, kernel, and tool versions. |

Memory values are in KiB.

### Metrics to compare

Prioritize:

1. workload peak PSS;
2. workload peak private and anonymous memory;
3. idle PSS after the same idle duration;
4. transactions per second;
5. p95 and p99 latency from the sysbench report;
6. major and minor faults if collected separately.

Treat `VmSize` as supporting information. It includes reserved but untouched allocator arenas and database mappings.

## Experimental discipline

For useful results:

- run all variants on the same kernel and MariaDB build;
- keep THP policy unchanged;
- stop the system MariaDB service;
- avoid unrelated memory/IO workloads;
- run each allocator several times in rotated order;
- compare medians and spread, not only the best run;
- verify allocator mappings for every preloaded run;
- keep throughput within a comparable range before interpreting memory differences.

A slower allocator can appear to use less peak memory merely because it completes less work concurrently. Conversely, a faster run may retain more active query state. Report memory and throughput together.

## Extending allocation pressure

The built-in OLTP workload is a realistic starting point, but the InnoDB buffer pool dominates total memory. To emphasize allocator behavior further, repeat the experiment with:

- more client connections;
- connection churn;
- queries that create in-memory temporary tables;
- sorting and grouping on non-indexed columns;
- prepared-statement churn;
- a smaller fixed buffer pool while keeping the dataset larger than it.

Those should be separate named workloads rather than silently changing the baseline.
