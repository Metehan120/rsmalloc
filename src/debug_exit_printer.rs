// Written by CODEX but should be good enough for use

use std::sync::atomic::Ordering::Relaxed;

#[cfg(feature = "debug-exact")]
use crate::trim::{TOTAL_TRIMMED_BLOCKS, TOTAL_TRIMMED_TIME};
use crate::{
    ABORTS, AVERAGE_BLOCK_TIMES, BUDDY_AVERAGE_BLOCK_TIMES, CURRENT_STAMP,
    HIGH_WATER_BUDDY_CACHED_VA, HIGH_WATER_SLAB_CACHED_VA, HIGH_WATER_TOTAL_CACHED_VA, NCPU,
    REFILL_OVER_PREDICTS, REFILL_UNDER_PREDICTS, REFILLS_BY_CLASS, START_TIME, TOTAL_CACHED_VA,
    TOTAL_MMAP_BYTES, TOTAL_MMAP_CALLS, TOTAL_REFILL_CALLS,
    big_allocations::buddy::{BIG_BUDDY_MIN_ORDER, BUDDY_BACKEND, BUDDY_TOTAL_CACHED_VA},
    internals::l3_main_radix::{CHUNK_SIZE, RADIX},
    rseq_core::slab_cache::SLAB_CACHE,
    trim::{DISABLE_BUDDY, TOTAL_TRIM_CALLS, TOTAL_TRIMMED_VA},
    utility::SIZE_CLASSES,
};

#[used]
#[unsafe(link_section = ".fini_array")]
static RSMALLOC_DEBUG_EXIT_PRINT: unsafe extern "C" fn() = rsmalloc_debug_exit_print;

fn fmt_bytes(bytes: usize) -> String {
    const KIB: f64 = 1024.0;
    const MIB: f64 = KIB * 1024.0;
    const GIB: f64 = MIB * 1024.0;
    const TIB: f64 = GIB * 1024.0;

    let value = bytes as f64;
    let (scaled, unit) = if value >= TIB {
        (value / TIB, "TiB")
    } else if value >= GIB {
        (value / GIB, "GiB")
    } else if value >= MIB {
        (value / MIB, "MiB")
    } else if value >= KIB {
        (value / KIB, "KiB")
    } else {
        (value, "B")
    };

    if unit == "B" {
        format!("{} B ({} bytes)", bytes, bytes)
    } else {
        format!("{:.2} {} ({} bytes)", scaled, unit, bytes)
    }
}

fn line(report: &mut String, text: &str) {
    report.push_str(text);
    report.push('\n');
}

fn section(report: &mut String, name: &str) {
    report.push('\n');
    line(report, name);
    line(report, "-");
}

fn item(report: &mut String, name: &str, value: impl core::fmt::Display) {
    line(report, &format!("  {:<18} {}", name, value));
}

fn byte_item(report: &mut String, name: &str, bytes: usize) {
    item(report, name, fmt_bytes(bytes));
}

fn fmt_bytes_short(bytes: usize) -> String {
    const KIB: f64 = 1024.0;
    const MIB: f64 = KIB * 1024.0;
    const GIB: f64 = MIB * 1024.0;

    let value = bytes as f64;
    if value >= GIB {
        format!("{:.1} GiB", value / GIB)
    } else if value >= MIB {
        format!("{:.1} MiB", value / MIB)
    } else if value >= KIB {
        format!("{:.1} KiB", value / KIB)
    } else {
        format!("{} B", bytes)
    }
}

unsafe extern "C" fn rsmalloc_debug_exit_print() {
    print_report();
}

pub(crate) unsafe fn print_report() {
    let slab_cached = TOTAL_CACHED_VA.load(Relaxed);
    let buddy_cached = BUDDY_TOTAL_CACHED_VA.load(Relaxed);
    let total_cached = slab_cached.saturating_add(buddy_cached);
    let total_refills = TOTAL_REFILL_CALLS.load(Relaxed);
    let under = REFILL_UNDER_PREDICTS.load(Relaxed);
    let over = REFILL_OVER_PREDICTS.load(Relaxed);
    let misses = under.saturating_add(over);
    let miss_percent = if total_refills == 0 {
        0.0
    } else {
        (misses as f64 * 100.0) / total_refills as f64
    };
    let uptime_ms = START_TIME
        .map(|start| start.elapsed().as_millis())
        .unwrap_or(0);

    let buddy = BUDDY_BACKEND.report();
    let radix = RADIX.report();

    let mut cpu_total = 0usize;
    let mut cpu_min = usize::MAX;
    let mut cpu_max = 0usize;
    let mut cpu_nonzero = 0usize;
    for cpu in 0..NCPU {
        let bytes = SLAB_CACHE.get_rseq_cpu_usage_bytes(cpu);
        cpu_total = cpu_total.saturating_add(bytes);
        cpu_min = cpu_min.min(bytes);
        cpu_max = cpu_max.max(bytes);
        if bytes != 0 {
            cpu_nonzero += 1;
        }
    }
    if NCPU == 0 {
        cpu_min = 0;
    }

    let mut report = String::new();
    line(&mut report, "rsmalloc statistics");

    section(&mut report, "process");
    item(&mut report, "pid", std::process::id());
    item(&mut report, "uptime", format!("{} ms", uptime_ms));
    item(
        &mut report,
        "clock",
        format!("{} ms", CURRENT_STAMP.load(Relaxed)),
    );
    item(&mut report, "cpus", NCPU);

    section(&mut report, "numa");
    let (numa, inner) = SLAB_CACHE.get_numa_and_inner();
    item(&mut report, "enabled", inner.is_numa);
    item(&mut report, "cpus", numa.ncpu);
    item(&mut report, "nodes", numa.nnodes);
    item(&mut report, "cpu ranges", numa.nranges);
    if !numa.node_ids.is_null() {
        for i in 0..numa.nnodes {
            item(&mut report, &format!("node {}", i), *numa.node_ids.add(i));
        }
    }
    if !numa.cpu_ranges.is_null() {
        for i in 0..numa.nranges {
            let range = *numa.cpu_ranges.add(i);
            item(
                &mut report,
                &format!("range {}", i),
                format!(
                    "node {}, cpus {}-{}",
                    range.node_id, range.start_cpu, range.end_cpu
                ),
            );
        }
    }

    section(&mut report, "rseq");
    item(&mut report, "refills", total_refills);
    item(
        &mut report,
        "predictor misses",
        format!("{} ({:.2}%)", misses, miss_percent),
    );
    item(&mut report, "under predicts", under);
    item(&mut report, "over predicts", over);
    item(&mut report, "aborts", ABORTS.load(Relaxed));

    #[cfg(feature = "debug-exact")]
    {
        use crate::{
            GLOBAL_LOCK_RETRIES, GLOBAL_LOCKS, GLOBAL_SPIN_WAITS, GLOBAL_TRY_LOCK_MISSES,
            GLOBAL_TRY_LOCKS,
        };

        section(&mut report, "locks");
        item(&mut report, "lock calls", GLOBAL_LOCKS.load(Relaxed));
        item(
            &mut report,
            "lock retries",
            GLOBAL_LOCK_RETRIES.load(Relaxed),
        );
        item(&mut report, "try locks", GLOBAL_TRY_LOCKS.load(Relaxed));
        item(
            &mut report,
            "try misses",
            GLOBAL_TRY_LOCK_MISSES.load(Relaxed),
        );
        item(&mut report, "spin waits", GLOBAL_SPIN_WAITS.load(Relaxed));
    }

    section(&mut report, "mmap calls");
    item(&mut report, "calls", TOTAL_MMAP_CALLS.load(Relaxed));
    byte_item(&mut report, "requested", TOTAL_MMAP_BYTES.load(Relaxed));

    section(&mut report, "cached virtual memory");
    line(&mut report, "  current");
    byte_item(&mut report, "slab", slab_cached);
    byte_item(&mut report, "buddy", buddy_cached);
    byte_item(&mut report, "total", total_cached);
    line(&mut report, "  high water");
    byte_item(&mut report, "slab", HIGH_WATER_SLAB_CACHED_VA.load(Relaxed));
    byte_item(
        &mut report,
        "buddy",
        HIGH_WATER_BUDDY_CACHED_VA.load(Relaxed),
    );
    byte_item(
        &mut report,
        "total",
        HIGH_WATER_TOTAL_CACHED_VA.load(Relaxed),
    );

    section(&mut report, "rseq cpu cache (exit)");
    byte_item(&mut report, "total", cpu_total);
    byte_item(&mut report, "min", cpu_min);
    byte_item(&mut report, "max", cpu_max);
    item(
        &mut report,
        "non-empty cpus",
        format!("{} / {}", cpu_nonzero, NCPU),
    );
    for cpu in 0..NCPU {
        let bytes = SLAB_CACHE.get_rseq_cpu_usage_bytes(cpu);
        item(&mut report, &format!("cpu {}", cpu), fmt_bytes(bytes));
    }

    section(&mut report, "size classes");
    line(
        &mut report,
        "  cls  size       refills  cached      cpus       min        max        avg",
    );
    for class in 0..SIZE_CLASSES.len() {
        let mut class_cached = 0usize;
        let mut active_cpus = 0usize;
        let mut min_cpu = usize::MAX;
        let mut max_cpu = 0usize;

        for cpu in 0..NCPU {
            let bytes = SLAB_CACHE.get_rseq_cpu_class_usage_bytes(cpu, class);
            class_cached = class_cached.saturating_add(bytes);

            if bytes != 0 {
                active_cpus += 1;
                min_cpu = min_cpu.min(bytes);
                max_cpu = max_cpu.max(bytes);
            }
        }

        let refills = REFILLS_BY_CLASS[class].load(Relaxed);

        let avg_cpu = if active_cpus == 0 {
            0
        } else {
            class_cached / active_cpus
        };
        if active_cpus == 0 {
            min_cpu = 0;
        }

        line(
            &mut report,
            &format!(
                "  {:>3}  {:<9} {:>7}  {:<10} {:>3}/{:<3}  {:<9}  {:<9}  {}",
                class,
                fmt_bytes_short(SIZE_CLASSES[class]),
                refills,
                fmt_bytes_short(class_cached),
                active_cpus,
                NCPU,
                fmt_bytes_short(min_cpu),
                fmt_bytes_short(max_cpu),
                fmt_bytes_short(avg_cpu)
            ),
        );
    }

    #[cfg(feature = "transfer-debug")]
    {
        section(&mut report, "transfer cache");

        #[cfg(feature = "transfer-debug-exact")]
        {
            use crate::{TOTAL_TRANSFER_POP_CALLS, TOTAL_TRANSFER_PUSH_CALLS};

            item(
                &mut report,
                "pop calls",
                TOTAL_TRANSFER_POP_CALLS.load(Relaxed),
            );
            item(
                &mut report,
                "push calls",
                TOTAL_TRANSFER_PUSH_CALLS.load(Relaxed),
            );
        }

        {
            use crate::{DRY_TRANSFER_STEALS, TOTAL_TRANSFER_RETRIES, TOTAL_TRANSFER_STEALS};

            item(&mut report, "steals", TOTAL_TRANSFER_STEALS.load(Relaxed));
            item(&mut report, "dry steals", DRY_TRANSFER_STEALS.load(Relaxed));
            item(
                &mut report,
                "cas retries",
                TOTAL_TRANSFER_RETRIES.load(Relaxed),
            );
        }
    }

    section(&mut report, "transfer class hints");
    line(&mut report, "  1 = hinted nonempty, 0 = no nonempty hint");
    for class in 0..SIZE_CLASSES.len() {
        line(
            &mut report,
            &format!(
                "  {:>3}  {:<9} {}",
                class,
                fmt_bytes_short(SIZE_CLASSES[class]),
                SLAB_CACHE.transfer_hint_bits(class)
            ),
        );
    }

    section(&mut report, "trim and relief");
    item(&mut report, "trim calls", TOTAL_TRIM_CALLS.load(Relaxed));
    byte_item(&mut report, "trimmed", TOTAL_TRIMMED_VA.load(Relaxed));

    #[cfg(feature = "debug-exact")]
    let trimmed_blocks = TOTAL_TRIMMED_BLOCKS.load(Relaxed);
    #[cfg(feature = "debug-exact")]
    let trimmed_time = TOTAL_TRIMMED_TIME.load(Relaxed) / trimmed_blocks.max(1);
    #[cfg(feature = "debug-exact")]
    item(&mut report, "trimmed blocks (small)", trimmed_blocks);
    #[cfg(feature = "debug-exact")]
    item(
        &mut report,
        "average madvise cycles (small)",
        format!("{}", trimmed_time),
    );
    item(
        &mut report,
        "avg small life",
        format!("{} ms", AVERAGE_BLOCK_TIMES.load(Relaxed)),
    );
    item(
        &mut report,
        "avg buddy life",
        format!("{} ms", BUDDY_AVERAGE_BLOCK_TIMES.load(Relaxed)),
    );
    item(&mut report, "buddy disabled", DISABLE_BUDDY.load(Relaxed));

    section(&mut report, "buddy backend");
    let used_bytes = buddy.total_region_bytes.saturating_sub(buddy.free_bytes);
    let free_pct = if buddy.total_region_bytes == 0 {
        0.0
    } else {
        (buddy.free_bytes as f64 * 100.0) / buddy.total_region_bytes as f64
    };
    let used_pct = 100.0 - free_pct;
    let free_blocks: usize = buddy.free_blocks.iter().sum();
    let never_bytes: usize = buddy
        .never_allocated_by_order
        .iter()
        .enumerate()
        .map(|(index, blocks)| blocks.saturating_mul(1usize << (BIG_BUDDY_MIN_ORDER + index)))
        .sum();
    let reused_bytes: usize = buddy
        .reused_by_order
        .iter()
        .enumerate()
        .map(|(index, blocks)| blocks.saturating_mul(1usize << (BIG_BUDDY_MIN_ORDER + index)))
        .sum();
    let trimmed_bytes: usize = buddy
        .trimmed_by_order
        .iter()
        .enumerate()
        .map(|(index, blocks)| blocks.saturating_mul(1usize << (BIG_BUDDY_MIN_ORDER + index)))
        .sum();

    item(&mut report, "regions", buddy.regions);
    item(&mut report, "grow order", buddy.grow_order);
    item(&mut report, "thp", buddy.thp);
    byte_item(&mut report, "region bytes", buddy.total_region_bytes);
    byte_item(&mut report, "used bytes", used_bytes);
    byte_item(&mut report, "free bytes", buddy.free_bytes);
    item(
        &mut report,
        "used / free",
        format!("{:.2}% / {:.2}%", used_pct, free_pct),
    );
    item(&mut report, "free blocks", free_blocks);
    item(&mut report, "never allocated", buddy.never_allocated_blocks);
    byte_item(&mut report, "never alloc bytes", never_bytes);
    item(&mut report, "reused", buddy.reused_blocks);
    byte_item(&mut report, "reused bytes", reused_bytes);
    item(&mut report, "trimmed blocks", buddy.trimmed_blocks);
    byte_item(&mut report, "trimmed bytes", trimmed_bytes);

    line(&mut report, "  free lists by order");
    line(
        &mut report,
        "  order  block size                  blocks  never  reused  trimmed  bytes",
    );
    for (index, blocks) in buddy.free_blocks.iter().enumerate() {
        let order = BIG_BUDDY_MIN_ORDER + index;
        let block_size = 1usize << order;
        let bytes = blocks.saturating_mul(block_size);
        line(
            &mut report,
            &format!(
                "  {:<5}  {:<26} {:>6}  {:>5}  {:>6}  {:>7}  {}",
                order,
                fmt_bytes(block_size),
                blocks,
                buddy.never_allocated_by_order[index],
                buddy.reused_by_order[index],
                buddy.trimmed_by_order[index],
                fmt_bytes(bytes)
            ),
        );
    }

    section(&mut report, "radix");
    let owned_bytes = radix.owned_chunks.saturating_mul(CHUNK_SIZE);
    let metadata_per_owned = if radix.owned_chunks == 0 {
        0.0
    } else {
        radix.metadata_bytes as f64 / radix.owned_chunks as f64
    };
    item(&mut report, "l1 nodes", radix.l1_nodes);
    item(&mut report, "l2 nodes", radix.l2_nodes);
    item(&mut report, "leaves", radix.leaves);
    item(&mut report, "owned chunks", radix.owned_chunks);
    byte_item(&mut report, "chunk size", CHUNK_SIZE);
    byte_item(&mut report, "owned bytes", owned_bytes);
    byte_item(&mut report, "metadata", radix.metadata_bytes);
    item(
        &mut report,
        "metadata / chunk",
        format!("{:.2} bytes", metadata_per_owned),
    );

    eprintln!("{}", report);
}
