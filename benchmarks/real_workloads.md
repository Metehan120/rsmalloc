# Real-Workload Microarchitectural Observations

This page holds raw hardware-performance-counter comparisons gathered from
real applications (not synthetic benchmarks) running under `LD_PRELOAD` with
rsmalloc vs. mimalloc. See [`benchmarks.md`](benchmarks.md) for the synthetic
benchmark snapshot and its caveats — the same "development signal, not a
guarantee" caveat applies here.

## Summary

In tested workloads like Blender, Krita, mimalloc-bench, and C/C++/Rust
compilation (including compiling rsmalloc itself), rsmalloc has shown better
microarchitectural (especially Zen) instruction-retirement efficiency than
other allocators in several runs. Results are workload-dependent: the Krita
run below favors rsmalloc almost across every counter, while the Blender run is
mixed (rsmalloc lower on some counters, higher on others).

## Tested system

- CPU: AMD Ryzen 5 5600X
- RAM: 16GB RAM DDR4 3200MHz
- OS: CachyOS 7.1.4-cachyos-bore
- Desktop Environment: KDE Plasma 6.7.3
- Environment Temperature: ~28-30C
- CPU Cooler: Arctic Freezer 36
- Motherboard: MSI B550M PRO-VDH
- BIOS: 2.M0 (Reported by dmidecode)
- GPU: ASUS ROG Strix OC RX 6600 XT

## Methodology

1. Krita: created a 16384×16384 3-layer canvas and painted each layer black
   using "Flood Fill". (Both runs had different exit times; rsmalloc 18.1s
   for exit, mimalloc 16.5s for exit. Repeated 5 times.)
2. Blender: started Blender, removed the cube, added a UV Sphere, and made it
   256×256. (Both runs had the same exit time. No camera movement. Repeated
   15 times.)

Raw AMD Zen PMC dispatch-stall counters, event `0xAF` (dispatch resource
stalls) and event `0xAE` (integer scheduler / retire token stalls), broken
down by sub-event bit.

## Krita

In Krita, rsmalloc reduced instruction retirement overhead by ~40% compared
to mimalloc (non-repeated single runs may differ from this average):

|Allocator|RSMalloc|mimalloc|
|---|---|---|
|INT_PHY_REG_FILE |2.350.615.088|2.677.560.455|
|LOAD_QUEUE       |763.315.122|730.665.826|
|STORE_QUEUE      |835.548.927|712.661.203|
|FP_REG_FILE      |31.512.193.929|33.789.888.393|
|INT_SCHEDULER_0  |777.675.307|891.293.797|
|INT_SCHEDULER_1  |4.461.471.251|4.922.779.694|
|INT_SCHEDULER_2  |22.176.034.505|26.200.062.522|
|INT_SCHEDULER_3  |2.317.902.484|3.857.713.483|
|RETIRE_TOKEN     |3.656.167.370|7.847.454.178|

## Blender

|Allocator|RSMalloc|mimalloc|
|---|---|---|
|INT_PHY_REG_FILE |215.356.247|216.971.772|
|LOAD_QUEUE       |119.328.261|118.992.621|
|STORE_QUEUE      |104.328.787|104.006.581|
|FP_REG_FILE      |608.713.025|682.492.961|
|INT_SCHEDULER_0  |156.231.590|142.970.156|
|INT_SCHEDULER_1  |312.140.233|312.982.113|
|INT_SCHEDULER_2  |219.066.606|302.994.170|
|INT_SCHEDULER_3  |42.710.209|45.892.070|
|RETIRE_TOKEN     |26.056.876|27.901.051|
