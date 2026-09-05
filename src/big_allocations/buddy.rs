// Small note for developers considering this design:
//
// First of all this is proof-of-concept not a release-ready buddy. Designed by GPT-6-Astra and published on development branch.
//
// GPT designed a way better buddy than I ever could (I've never been good with buddy allocators) so I decided to publish as proof-of-concept before release
// this design is going to change in future, not a final product or a pure generation.
//
// This version of buddy designed for least performance one-core performance impact with high core scalability,
// this design skip most of the linear searches old buddy had.
//
// I did audit the buddy design; there is still some aggressive ordering where a weaker ordering can be used, a few optimization spots for arithmetics etc.
// which can be fixed easily. Overall a good design worth considering.
//
// Oh also ate my 5-hour limit for breakfast, it was pretty hungry I guess. I know this is such a dad joke.
//
// - Metehan

use crate::{
    BUDDY_AVERAGE_BLOCK_TIMES, BUDDY_INIT, CURRENT_STAMP, Flags, GLOBAL_TRIM_LOCK,
    add_buddy_cached_va,
    backend::page_allocator::{ARENA_SIZE, PAGE_ALLOCATOR},
    core_prim::predictor::EMA_ALPHA,
    global_vals::{BIG_TRIM_THRESHOLD, SMALL_TRIM_THRESHOLD, TOTAL_CACHED_VA},
    inner::alloc::MAX_REFILL_RETRIES,
    internals::{
        binder::NumaBind,
        lock::{LockGuard, SpinLock},
        once::Once,
        radix_tree::RADIX,
    },
    record_mmap_call,
    rseq_core::slab_cache::SLAB_CACHE,
    traits::Lock,
    utility::Alignment,
};
use rustix::{
    mm::{Advice, MapFlags, ProtFlags, madvise, mmap_anonymous, munmap},
    thread::sched_getcpu,
};
use std::{
    mem::size_of,
    os::raw::c_void,
    ptr::null_mut,
    sync::atomic::{AtomicPtr, AtomicUsize, Ordering},
};

mod atomics {
    pub(super) use std::sync::atomic::{AtomicU32, AtomicU64, Ordering};
    #[cfg(all(test, feature = "debug-exact"))]
    pub(super) fn probe() {
        super::SEGMENT_PROBES.fetch_add(1, Ordering::Relaxed);
    }
    #[cfg(all(test, feature = "debug-exact"))]
    pub(super) fn retry() {
        super::CAS_RETRIES.fetch_add(1, Ordering::Relaxed);
    }
}

#[cfg(all(test, feature = "debug-exact"))]
static SEGMENT_PROBES: AtomicUsize = AtomicUsize::new(0);
#[cfg(all(test, feature = "debug-exact"))]
static CAS_RETRIES: AtomicUsize = AtomicUsize::new(0);

mod segment;
use segment::{SEGMENT_BYTES, SLOT_BYTES, Segment};

pub static BUDDY_TOTAL_CACHED_VA: AtomicUsize = AtomicUsize::new(0);
pub const BIG_BUDDY_MIN_ORDER: usize = 22;
pub const BIG_BUDDY_MAX_ORDER: usize = 26;
pub const BUDDY_NUM_ORDERS: usize = BIG_BUDDY_MAX_ORDER - BIG_BUDDY_MIN_ORDER + 1;
const PAGE_SIZE: usize = 4096;
const MAX_LANES: usize = 96;
type Allocation = (usize, usize, Flags, usize);

#[repr(C, align(64))]
struct HintLane {
    orders: [AtomicPtr<Segment>; BUDDY_NUM_ORDERS],
}

impl HintLane {
    fn new() -> Self {
        Self {
            orders: std::array::from_fn(|_| AtomicPtr::new(null_mut())),
        }
    }
}

#[repr(C, align(64))]
struct Node {
    head: AtomicPtr<Region>,
    growth: SpinLock<()>,
    lanes: *mut HintLane,
}

impl Node {
    fn new(lanes: *mut HintLane) -> Self {
        Self {
            head: AtomicPtr::new(null_mut()),
            growth: SpinLock::new(()),
            lanes,
        }
    }

    unsafe fn alloc(&self, order: usize, lane: usize) -> Option<Allocation> {
        let hint = &(*self.lanes.add(lane)).orders[order];
        let preferred = hint.load(Ordering::Acquire);
        if !preferred.is_null()
            && let Some((addr, flags)) = (*preferred).alloc(order)
        {
            return Some((addr, order + BIG_BUDDY_MIN_ORDER, flags, preferred as usize));
        }
        let mut region = self.head.load(Ordering::Acquire);
        while let Some(region_ref) = region.as_ref() {
            let count = region_ref.segment_count;
            let mut index = lane % count;
            for _ in 0..count {
                let segment = region_ref.segments().add(index);
                if segment != preferred
                    && let Some((addr, flags)) = (*segment).alloc(order)
                {
                    hint.store(segment, Ordering::Release);
                    return Some((addr, order + BIG_BUDDY_MIN_ORDER, flags, segment as usize));
                }
                index += 1;
                if index == count {
                    index = 0;
                }
            }
            region = region_ref.next;
        }
        None
    }

    /// The caller holds growth, or is constructing an unpublished node table.
    unsafe fn publish(&self, region: *mut Region) {
        (*region).next = self.head.load(Ordering::Relaxed);
        self.head.store(region, Ordering::Release);
    }
}

#[repr(C, align(64))]
struct Region {
    next: *mut Region,
    base: usize,
    bytes: usize,
    metadata_bytes: usize,
    segment_count: usize,
}

impl Region {
    unsafe fn segments(&self) -> *mut Segment {
        (self as *const Self as *mut u8)
            .add(size_of::<Self>())
            .cast()
    }
}

#[repr(C, align(64))]
struct State {
    nodes: *mut Node,
    node_count: usize,
    lane_count: usize,
    active_ids: *const u16,
    active_count: usize,
    thp: bool,
    is_numa: bool,
}

impl State {
    unsafe fn node(&self, id: usize) -> &Node {
        &*self.nodes.add(id)
    }

    unsafe fn regions(&self) -> impl Iterator<Item = &Region> {
        (0..self.node_count).flat_map(move |id| {
            let mut next = self.node(id).head.load(Ordering::Acquire);
            std::iter::from_fn(move || {
                let region = next.as_ref()?;
                next = region.next;
                Some(region)
            })
        })
    }
}

pub struct BuddyAllocator {
    state: AtomicPtr<State>,
    once: Once,
}

unsafe impl Sync for BuddyAllocator {}

impl BuddyAllocator {
    pub const fn new() -> Self {
        Self {
            state: AtomicPtr::new(null_mut()),
            once: Once::new(),
        }
    }

    fn normalize_region_size(size: usize) -> usize {
        size.checked_next_power_of_two()
            .unwrap_or(SEGMENT_BYTES)
            .max(SEGMENT_BYTES)
    }

    #[inline(always)]
    fn lane_count(ncpu: usize) -> usize {
        ncpu.clamp(1, MAX_LANES)
    }

    #[inline(always)]
    fn lane_for_cpu(cpu_id: usize, lane_count: usize) -> usize {
        if cpu_id < lane_count {
            cpu_id
        } else {
            cpu_id % lane_count
        }
    }

    #[inline(never)]
    pub unsafe fn init(&self, size: usize, thp: bool) {
        self.once.call_once(|| {
            let (numa, inner) = SLAB_CACHE.get_numa_and_inner();
            let count = numa.nranges.max(1);
            let lane_count = Self::lane_count(numa.ncpu);
            let Some(node_bytes) = size_of::<Node>().checked_mul(count) else {
                return;
            };
            let Some(lane_bytes) = count
                .checked_mul(lane_count)
                .and_then(|lanes| lanes.checked_mul(size_of::<HintLane>()))
            else {
                return;
            };
            let Some(bytes) = size_of::<State>()
                .checked_add(node_bytes)
                .and_then(|bytes| bytes.checked_add(lane_bytes))
                .and_then(|bytes| bytes.checked_align_to(PAGE_SIZE))
            else {
                return;
            };
            let Some(mem) = reserve(bytes, 0, true, inner.is_numa) else {
                return;
            };
            let state = mem.cast::<State>();
            let nodes = mem.cast::<u8>().add(size_of::<State>()).cast::<Node>();
            let lanes = nodes.cast::<u8>().add(node_bytes).cast::<HintLane>();
            for id in 0..count {
                let node_lanes = lanes.add(id * lane_count);
                for lane in 0..lane_count {
                    node_lanes.add(lane).write(HintLane::new());
                }
                nodes.add(id).write(Node::new(node_lanes));
            }
            state.write(State {
                nodes,
                node_count: count,
                lane_count,
                active_ids: numa.node_ids,
                active_count: numa.nnodes,
                thp,
                is_numa: inner.is_numa,
            });
            let id = SLAB_CACHE.node_for_cpu(sched_getcpu() as usize, inner) as usize;
            let id = if id < count { id } else { 0 };
            let Some(region) = create_region(Self::normalize_region_size(size), id as u16, &*state)
            else {
                let _ = munmap(mem, bytes);
                return;
            };
            (*state).node(id).publish(region);
            add_buddy_cached_va(bytes);
            self.state.store(state, Ordering::Release);
            BUDDY_INIT = true;
        });
    }

    #[inline]
    pub unsafe fn alloc(&self, size: usize, node_id: u16, cpu_id: usize) -> Option<Allocation> {
        if size > SEGMENT_BYTES {
            return None;
        }
        let order = size.max(SLOT_BYTES).next_power_of_two().trailing_zeros() as usize
            - BIG_BUDDY_MIN_ORDER;
        let state = self.state.load(Ordering::Acquire).as_ref()?;
        let id = if (node_id as usize) < state.node_count {
            node_id as usize
        } else {
            0
        };
        let lane = Self::lane_for_cpu(cpu_id, state.lane_count);
        let node = state.node(id);
        node.alloc(order, lane)
            .or_else(|| self.expand(state, id, order, lane))
    }

    #[cold]
    #[inline(never)]
    unsafe fn expand(
        &self,
        state: &State,
        id: usize,
        order: usize,
        lane: usize,
    ) -> Option<Allocation> {
        let node = state.node(id);
        {
            let _growth = node.growth.lock();
            if let Some(block) = node.alloc(order, lane) {
                return Some(block);
            }
            if let Some(region) = create_region(SEGMENT_BYTES, id as u16, state) {
                let segment = (*region).segments();
                let (addr, flags) = (*segment).alloc(order).unwrap();
                node.publish(region);
                (*node.lanes.add(lane)).orders[order].store(segment, Ordering::Release);
                return Some((addr, order + BIG_BUDDY_MIN_ORDER, flags, segment as usize));
            }
        }

        for index in 0..state.active_count {
            let remote = *state.active_ids.add(index) as usize;
            if remote != id
                && remote < state.node_count
                && let Some(block) = state.node(remote).alloc(order, lane)
            {
                return Some(block);
            }
        }
        None
    }

    #[inline]
    pub unsafe fn free(&self, region: usize, addr: usize, order: usize) {
        if !(BIG_BUDDY_MIN_ORDER..=BIG_BUDDY_MAX_ORDER).contains(&order) {
            return;
        }
        if let Some(segment) = (region as *const Segment).as_ref() {
            segment.free(
                addr,
                order - BIG_BUDDY_MIN_ORDER,
                CURRENT_STAMP.load(Ordering::Relaxed),
            );
        }
    }

    #[inline]
    pub unsafe fn try_grow_inplace(
        &self,
        region: usize,
        addr: usize,
        order: usize,
    ) -> Option<(usize, usize)> {
        if !(BIG_BUDDY_MIN_ORDER..BIG_BUDDY_MAX_ORDER).contains(&order) {
            return None;
        }
        let segment = (region as *const Segment).as_ref()?;
        segment
            .grow(addr, order - BIG_BUDDY_MIN_ORDER)
            .then_some((addr, order + 1))
    }

    pub unsafe fn trim(&self, requested: usize) -> usize {
        self.trim_inner(requested, true)
    }

    pub unsafe fn trim_old(&self, requested: usize) -> usize {
        self.trim_inner(requested, false)
    }

    unsafe fn trim_inner(&self, requested: usize, force: bool) -> usize {
        if !force
            && TOTAL_CACHED_VA.load(Ordering::Relaxed) < SMALL_TRIM_THRESHOLD
            && BUDDY_TOTAL_CACHED_VA.load(Ordering::Relaxed) < BIG_TRIM_THRESHOLD
        {
            return 0;
        }
        let Some(state) = self.state.load(Ordering::Acquire).as_ref() else {
            return 0;
        };
        let LockGuard::Free(_guard) = GLOBAL_TRIM_LOCK.try_lock() else {
            return 0;
        };
        #[cfg(feature = "debug")]
        crate::backend::trim::TOTAL_TRIM_CALLS.fetch_add(1, Ordering::Relaxed);
        let now = CURRENT_STAMP.load(Ordering::Relaxed);
        let average = BUDDY_AVERAGE_BLOCK_TIMES.load(Ordering::Relaxed);
        let mut stats = TrimStats::default();
        'regions: for region in state.regions() {
            for index in 0..region.segment_count {
                let segment = &*region.segments().add(index);
                trim_segment(
                    segment,
                    now,
                    average,
                    force,
                    requested.saturating_sub(stats.bytes),
                    &mut stats,
                    |addr, bytes| {
                        #[cfg(feature = "lazy-page-trim")]
                        let advice = Advice::LinuxFree;
                        #[cfg(not(feature = "lazy-page-trim"))]
                        let advice = Advice::LinuxDontNeed;
                        madvise(addr as *mut c_void, bytes, advice).is_ok()
                    },
                );
                if requested != 0 && stats.bytes >= requested {
                    break 'regions;
                }
            }
        }
        stats.finish(average);
        stats.bytes
    }

    #[cfg(feature = "preload")]
    pub unsafe fn lock_all_for_fork(&self) {
        if let Some(state) = self.state.load(Ordering::Acquire).as_ref() {
            for id in 0..state.node_count {
                std::mem::forget(state.node(id).growth.lock());
            }
        }
    }

    #[cfg(feature = "preload")]
    pub unsafe fn reset_locks_on_fork(&self) {
        if let Some(state) = self.state.load(Ordering::Acquire).as_ref() {
            for id in 0..state.node_count {
                state.node(id).growth.unlock();
            }
        }
    }
}

/// Metadata always uses page allocation; payloads use the configured threshold.
unsafe fn reserve(bytes: usize, node: u16, metadata: bool, is_numa: bool) -> Option<*mut c_void> {
    for _ in 0..MAX_REFILL_RETRIES {
        if metadata || bytes < ARENA_SIZE {
            if let Some(mem) = PAGE_ALLOCATOR.alloc(node, bytes) {
                return Some(mem);
            }
        } else {
            record_mmap_call(bytes);
            if let Ok(mem) = mmap_anonymous(
                null_mut(),
                bytes,
                ProtFlags::READ | ProtFlags::WRITE,
                MapFlags::PRIVATE,
            ) {
                if is_numa {
                    NumaBind.prefer_node(mem, bytes, node);
                }
                return Some(mem);
            }
        }
    }
    None
}

unsafe fn create_region(bytes: usize, node: u16, state: &State) -> Option<*mut Region> {
    let count = bytes / SEGMENT_BYTES;
    let metadata_bytes = count
        .checked_mul(size_of::<Segment>())?
        .checked_add(size_of::<Region>())?
        .checked_align_to(PAGE_SIZE)?;
    let data = reserve(bytes, node, false, state.is_numa)?;
    let Some(metadata) = reserve(metadata_bytes, node, true, state.is_numa) else {
        let _ = munmap(data, bytes);
        return None;
    };
    let region = metadata.cast::<Region>();
    region.write(Region {
        next: null_mut(),
        base: data as usize,
        bytes,
        metadata_bytes,
        segment_count: count,
    });
    for index in 0..count {
        (*region)
            .segments()
            .add(index)
            .write(Segment::new(data as usize + index * SEGMENT_BYTES));
    }
    if state.thp {
        let _ = madvise(data, bytes, Advice::LinuxHugepage);
    }
    RADIX.set_range(data as usize, bytes, true);
    add_buddy_cached_va(bytes);
    add_buddy_cached_va(metadata_bytes);
    Some(region)
}

#[derive(Default)]
struct TrimStats {
    bytes: usize,
    ages: u64,
    samples: u64,
}
impl TrimStats {
    fn finish(&self, previous: u32) {
        if self.samples != 0 {
            let average = (self.ages / self.samples).clamp(10, 600);
            let blended = (EMA_ALPHA * average as f32 + (1.0 - EMA_ALPHA) * previous as f32)
                .round()
                .clamp(10.0, 600.0) as u32;
            BUDDY_AVERAGE_BLOCK_TIMES.store(blended, Ordering::Relaxed);
        }
        #[cfg(feature = "debug")]
        crate::backend::trim::TOTAL_TRIMMED_VA.fetch_add(self.bytes, Ordering::Relaxed);
    }
}

fn trim_segment(
    segment: &Segment,
    now: u32,
    average: u32,
    force: bool,
    requested: usize,
    stats: &mut TrimStats,
    mut advise: impl FnMut(usize, usize) -> bool,
) {
    let mut pending = Segment::dirty_free(segment.snapshot());
    let initial_bytes = stats.bytes;
    while pending != 0 {
        let slot = pending.trailing_zeros() as usize;
        let age = segment.age(slot, now);
        if !force && age <= average {
            stats.ages += age as u64;
            stats.samples += 1;
            pending &= !(1 << slot);
            continue;
        }
        // Claim a consecutive candidate range; all ages are rechecked afterwards.
        let count = (pending >> slot).trailing_ones() as usize;
        let mask = (((1u32 << count) - 1) << slot) as u16;
        pending &= !mask;
        if !segment.claim_trim(mask) {
            continue;
        }
        let mut eligible = 0u16;
        for index in slot..slot + count {
            let age = segment.age(index, now);
            stats.ages += age as u64;
            stats.samples += 1;
            if force || age > average {
                eligible |= 1 << index;
            }
        }
        segment.finish_trim(mask & !eligible, false, None);
        while eligible != 0 {
            let start = eligible.trailing_zeros() as usize;
            let mut count = (eligible >> start).trailing_ones() as usize;
            if requested != 0 {
                let remaining = requested.saturating_sub(stats.bytes - initial_bytes);
                count = count.min(remaining.div_ceil(SLOT_BYTES).max(1));
            }
            let range = (((1u32 << count) - 1) << start) as u16;
            let bytes = count * SLOT_BYTES;
            let success = advise(segment.base() + start * SLOT_BYTES, bytes);
            segment.finish_trim(range, success, Some(now));
            eligible &= !range;
            if success {
                stats.bytes += bytes;
            }
            if requested != 0 && stats.bytes - initial_bytes >= requested {
                segment.finish_trim(eligible, false, None);
                return;
            }
        }
    }
}

pub static BUDDY_BACKEND: BuddyAllocator = BuddyAllocator::new();

#[cfg(feature = "debug")]
pub struct BuddyBackendReport {
    pub regions: usize,
    pub total_region_bytes: usize,
    pub free_bytes: usize,
    pub free_blocks: [usize; BUDDY_NUM_ORDERS],
    pub never_allocated_blocks: usize,
    pub reused_blocks: usize,
    pub trimmed_blocks: usize,
    pub never_allocated_by_order: [usize; BUDDY_NUM_ORDERS],
    pub reused_by_order: [usize; BUDDY_NUM_ORDERS],
    pub trimmed_by_order: [usize; BUDDY_NUM_ORDERS],
    pub grow_order: usize,
    pub thp: bool,
}

#[cfg(feature = "debug")]
impl BuddyAllocator {
    pub unsafe fn report(&self) -> BuddyBackendReport {
        let state = self.state.load(Ordering::Acquire).as_ref();
        let mut report = BuddyBackendReport {
            regions: 0,
            total_region_bytes: 0,
            free_bytes: 0,
            free_blocks: [0; BUDDY_NUM_ORDERS],
            never_allocated_blocks: 0,
            reused_blocks: 0,
            trimmed_blocks: 0,
            never_allocated_by_order: [0; BUDDY_NUM_ORDERS],
            reused_by_order: [0; BUDDY_NUM_ORDERS],
            trimmed_by_order: [0; BUDDY_NUM_ORDERS],
            grow_order: BIG_BUDDY_MAX_ORDER,
            thp: state.is_some_and(|state| state.thp),
        };
        let Some(state) = state else {
            return report;
        };
        for region in state.regions() {
            report.regions += 1;
            report.total_region_bytes = report.total_region_bytes.saturating_add(region.bytes);
            for index in 0..region.segment_count {
                let word = (*region.segments().add(index)).snapshot();
                let mut free = !(word as u16);
                while free != 0 {
                    let slot = free.trailing_zeros() as usize;
                    let order = (0..BUDDY_NUM_ORDERS)
                        .rev()
                        .find(|&order| {
                            let mask = segment::slot_mask(slot, order);
                            slot % (1 << order) == 0 && free & mask == mask
                        })
                        .unwrap();
                    let mask = segment::slot_mask(slot, order);
                    free &= !mask;
                    report.free_blocks[order] += 1;
                    report.free_bytes += SLOT_BYTES << order;
                    match segment::classify(word, mask) {
                        Flags::NotAllocated => {
                            report.never_allocated_blocks += 1;
                            report.never_allocated_by_order[order] += 1;
                        }
                        Flags::Trimmed => {
                            report.trimmed_blocks += 1;
                            report.trimmed_by_order[order] += 1;
                        }
                        _ => {
                            report.reused_blocks += 1;
                            report.reused_by_order[order] += 1;
                        }
                    }
                }
            }
        }
        report
    }
}
