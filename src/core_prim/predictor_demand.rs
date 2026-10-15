#![cfg(feature = "experimental-predictive-demand")]

use portable_atomic::AtomicU128;
use std::sync::atomic::{AtomicU64, Ordering};

#[path = "predictor_ml.rs"]
mod ml;
use ml::LinearDemand;

const TICK_MASK: u64 = (1 << 46) - 1;
const OVERLAP: u64 = 1 << 62;
const BUSY: u64 = 1 << 63;
const GENERATION_MASK: u64 = OVERLAP - 1;
const IDLE_NS: u64 = 100_000_000;
const HORIZON_NS: u64 = 50_000;
const UPDATE_INTERVAL_NS: u64 = 50_000;
const BYTE_BUDGET: usize = 64 * 1024;

// Mmap zeroing supplies cold model/control words; no allocator recursion is
// required. The learned coefficients and their feature history publish together.
#[repr(C, align(16))]
pub(super) struct DemandTracker {
    model: AtomicU128,
    sample: AtomicU64,
    generation: AtomicU64,
    last_update: AtomicU64,
    completed_at: AtomicU64,
}

const _: () = assert!(size_of::<DemandTracker>() == 48);

#[inline]
pub(super) fn clock_ns() -> u64 {
    crate::global_vals::get_clock().elapsed().as_nanos() as u64
}

impl DemandTracker {
    pub(super) const fn new() -> Self {
        Self {
            model: AtomicU128::new(0),
            sample: AtomicU64::new(0),
            generation: AtomicU64::new(0),
            last_update: AtomicU64::new(0),
            completed_at: AtomicU64::new(0),
        }
    }

    #[inline]
    pub(super) fn expected(&self, init: usize) -> usize {
        let forecast = (self.model.load(Ordering::Relaxed) >> 80) as u16 as usize;
        if forecast == 0 { init.max(1) } else { forecast }
    }

    pub(super) fn begin(
        &self,
        now: u64,
        init: usize,
        max: usize,
        stride: usize,
        cache_empty: bool,
    ) -> DemandSample<'_> {
        let cap = max
            .max(1)
            .min(u16::MAX as usize)
            .min((BYTE_BUDGET / stride.max(1)).max(1));
        let previous = self.sample.load(Ordering::Acquire);
        let owner = if previous & BUSY != 0 {
            self.mark_overlap(previous);
            0
        } else {
            let generation = self
                .generation
                .fetch_add(1, Ordering::Relaxed)
                .wrapping_add(1)
                & GENERATION_MASK;
            let candidate = BUSY | generation;
            match self.sample.compare_exchange(
                previous,
                candidate,
                Ordering::Acquire,
                Ordering::Relaxed,
            ) {
                Ok(_) => candidate,
                Err(seen) => {
                    self.mark_overlap(seen);
                    0
                }
            }
        };
        DemandSample {
            tracker: self,
            owner,
            previous,
            previous_finished: if owner != 0 {
                self.completed_at.load(Ordering::Relaxed)
            } else {
                0
            },
            started: now,
            start_empty: cache_empty,
            initial: init.max(1).min(cap) as u16,
            cap: cap as u16,
            #[cfg(any(test, feature = "predictor-debug"))]
            demand: LinearDemand::decode(self.model.load(Ordering::Relaxed), init, cap).forecast(),
        }
    }

    #[inline(never)]
    fn update(
        &self,
        previous: u64,
        previous_finished: u64,
        started: u64,
        finished: u64,
        init: usize,
        cap: usize,
    ) {
        let old = self.model.load(Ordering::Relaxed);
        let elapsed = started.wrapping_sub(previous_finished);
        let mut model = LinearDemand::decode(old, init, cap);
        if elapsed >= IDLE_NS {
            model = LinearDemand::cold(init, cap);
        } else {
            // The label describes the previous supply's depletion up to this
            // refill's START. It never uses the requested count, current refill
            // latency, or the current delivery as a training label.
            let delivered = (previous >> 46) as u16 as u64;
            let observed = (delivered * HORIZON_NS / elapsed.max(1)).clamp(1, cap as u64);
            model.observe(observed as usize, cap);
        }
        if self
            .model
            .compare_exchange(old, model.encode(), Ordering::Relaxed, Ordering::Relaxed)
            .is_ok()
        {
            // Time + 1 reserves zero for a model that has not updated yet.
            self.last_update
                .store(finished.wrapping_add(1), Ordering::Relaxed);
        }
    }

    fn mark_overlap(&self, seen: u64) {
        if seen & BUSY != 0 && seen & OVERLAP == 0 {
            let _ = self.sample.compare_exchange(
                seen,
                seen | OVERLAP,
                Ordering::Relaxed,
                Ordering::Relaxed,
            );
        }
    }

    #[cfg(feature = "preload")]
    pub(super) fn reset_after_fork(&self) {
        self.model.store(0, Ordering::Relaxed);
        self.sample.store(0, Ordering::Relaxed);
        self.last_update.store(0, Ordering::Relaxed);
        self.completed_at.store(0, Ordering::Relaxed);
        // Preserve generations so stale inherited guards cannot affect a child
        // lease even when clock values repeat. Generation wrap is finite (2^62).
    }
}

pub struct DemandSample<'a> {
    tracker: &'a DemandTracker,
    owner: u64,
    previous: u64,
    previous_finished: u64,
    started: u64,
    initial: u16,
    cap: u16,
    start_empty: bool,
    #[cfg(any(test, feature = "predictor-debug"))]
    demand: usize,
}

impl DemandSample<'_> {
    #[cfg(any(test, feature = "predictor-debug"))]
    pub(crate) fn demand(&self) -> usize {
        self.demand
    }

    pub fn finish(mut self, now: u64, delivered: usize, reliable: bool) {
        let complete =
            if self.start_empty && reliable && (1..=u16::MAX as usize).contains(&delivered) {
                (now & TICK_MASK) | ((delivered as u64) << 46)
            } else {
                0
            };
        if complete != 0
            && self.owner != 0
            && self.tracker.sample.load(Ordering::Relaxed) == self.owner
        {
            let last = self.tracker.last_update.load(Ordering::Relaxed);
            if (last == 0 || now.wrapping_sub(last.wrapping_sub(1)) >= UPDATE_INTERVAL_NS)
                && (self.previous >> 46) as u16 != 0
            {
                self.tracker.update(
                    self.previous,
                    self.previous_finished,
                    self.started,
                    now,
                    self.initial as usize,
                    self.cap as usize,
                );
            }
            // Publish the full timestamp through the lease's release/acquire
            // handoff. Truncated sample ticks alone alias after about 19.5h.
            self.tracker.completed_at.store(now, Ordering::Relaxed);
        }
        // Every accepted delivery is retained, even when the 50us gate skips
        // training. A late overlap can discard this sample after advisory work.
        self.release(complete);
    }

    fn release(&mut self, complete: u64) {
        let owner = self.owner;
        self.owner = 0;
        if owner == 0 {
            return;
        }
        if self
            .tracker
            .sample
            .compare_exchange(owner, complete, Ordering::Release, Ordering::Relaxed)
            .is_err()
        {
            let _ = self.tracker.sample.compare_exchange(
                owner | OVERLAP,
                0,
                Ordering::Release,
                Ordering::Relaxed,
            );
        }
    }
}

impl Drop for DemandSample<'_> {
    fn drop(&mut self) {
        self.release(0);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn deliver(tracker: &DemandTracker, start: u64, finish: u64, count: usize) {
        tracker
            .begin(start, 128, 1000, 1, true)
            .finish(finish, count, true);
    }

    #[test]
    fn learns_only_on_finish_and_excludes_refill_latency() {
        let tracker = DemandTracker::new();
        deliver(&tracker, 0, 100, 20);
        let current = tracker.begin(10_100, 128, 1000, 1, true);
        assert_eq!(current.demand(), 128);
        assert_eq!(tracker.expected(128), 128);
        assert_eq!(tracker.last_update.load(Ordering::Relaxed), 0);
        current.finish(20_100, 10, true);
        assert_eq!(tracker.expected(128), 100);
        assert_eq!(tracker.last_update.load(Ordering::Relaxed), 20_101);
    }

    #[test]
    fn skipped_training_still_publishes_latest_actual_supply() {
        let tracker = DemandTracker::new();
        deliver(&tracker, 0, 0, 2);
        deliver(&tracker, 1000, 1000, 10);
        let before = tracker.model.load(Ordering::Relaxed);
        deliver(&tracker, 2000, 2000, 4);
        assert_eq!(tracker.model.load(Ordering::Relaxed), before);
        assert_eq!(tracker.last_update.load(Ordering::Relaxed), 1001);
        let completed = tracker.sample.load(Ordering::Relaxed);
        assert_eq!((completed >> 46) as u16, 4);
        assert_eq!(completed & TICK_MASK, 2000);
        // Start precedes the training deadline; finish crosses it. Normalize the
        // latest four blocks over 4us, not over the entire 50us training window.
        let current = tracker.begin(6000, 128, 1000, 1, true);
        assert_eq!(tracker.model.load(Ordering::Relaxed), before);
        current.finish(51_000, 1, true);
        let mut reference = LinearDemand::cold(128, 1000);
        reference.observe(100, 1000);
        reference.observe(50, 1000);
        assert_eq!(tracker.model.load(Ordering::Relaxed), reference.encode());
        assert_eq!(tracker.last_update.load(Ordering::Relaxed), 51_001);
    }

    #[test]
    fn gate_handles_zero_time_and_exact_50us_boundary() {
        let tracker = DemandTracker::new();
        deliver(&tracker, 0, 0, 1);
        deliver(&tracker, 0, 0, 1);
        assert_eq!(tracker.last_update.load(Ordering::Relaxed), 1);
        deliver(&tracker, 49_000, 49_999, 1);
        assert_eq!(tracker.last_update.load(Ordering::Relaxed), 1);
        deliver(&tracker, 50_000, 50_000, 1);
        assert_eq!(tracker.last_update.load(Ordering::Relaxed), 50_001);
    }

    #[test]
    fn invalid_overlap_and_error_drop_do_not_train() {
        let tracker = DemandTracker::new();
        deliver(&tracker, 0, 0, 2);
        deliver(&tracker, 1000, 1000, 10);
        let before = tracker.model.load(Ordering::Relaxed);
        let owner = tracker.begin(100_000, 128, 1000, 1, true);
        let overlapping = tracker.begin(100_000, 128, 1000, 1, true);
        owner.finish(110_000, 10, true);
        drop(overlapping);
        tracker
            .begin(200_000, 128, 1000, 1, true)
            .finish(210_000, 10, false);
        drop(tracker.begin(300_000, 128, 1000, 1, true));
        assert_eq!(tracker.model.load(Ordering::Relaxed), before);
        assert_eq!(tracker.last_update.load(Ordering::Relaxed), 1001);
        assert_eq!(tracker.sample.load(Ordering::Relaxed), 0);
    }

    #[test]
    fn idle_resets_learned_history_and_weights_without_retraining_next_fill() {
        let tracker = DemandTracker::new();
        deliver(&tracker, 0, 0, 2);
        deliver(&tracker, 1000, 1000, 10);
        let resumed = 1000 + IDLE_NS;
        deliver(&tracker, resumed, resumed, 4);
        let reset = tracker.model.load(Ordering::Relaxed);
        assert_eq!(reset, LinearDemand::cold(128, 1000).encode());
        deliver(&tracker, resumed + 1, resumed + 2, 8);
        assert_eq!(tracker.model.load(Ordering::Relaxed), reset);
    }

    #[test]
    fn long_idle_cannot_alias_to_a_short_interval_at_packed_clock_wrap() {
        let tracker = DemandTracker::new();
        deliver(&tracker, 0, 0, 2);
        let resumed = (1_u64 << 46) + 1000;
        deliver(&tracker, resumed, resumed, 10);
        assert_eq!(
            tracker.model.load(Ordering::Relaxed),
            LinearDemand::cold(128, 1000).encode()
        );
        assert_eq!(tracker.expected(128), 128);
        assert_eq!(tracker.completed_at.load(Ordering::Relaxed), resumed);
    }

    #[test]
    fn actual_class_bounds_and_zero_mapping_layout() {
        use crate::{
            Header,
            utility::{Alignment, ITERATIONS, SIZE_CLASSES},
        };
        assert_eq!(size_of::<DemandTracker>(), 48);
        assert_eq!(align_of::<DemandTracker>(), 16);
        for (class, payload) in SIZE_CLASSES.iter().enumerate() {
            let tracker: DemandTracker = unsafe { std::mem::zeroed() };
            let stride = (payload + Header::SIZE).align_to(16);
            let cap = ITERATIONS[class].min((BYTE_BUDGET / stride).max(1));
            tracker
                .begin(0, usize::MAX, ITERATIONS[class], stride, true)
                .finish(0, cap, true);
            tracker
                .begin(1, usize::MAX, ITERATIONS[class], stride, true)
                .finish(1, cap, true);
            let forecast = tracker.expected(128);
            assert!((1..=cap).contains(&forecast));
        }
    }

    #[test]
    fn concurrent_same_clock_claims_release_busy_state() {
        let tracker = DemandTracker::new();
        std::thread::scope(|scope| {
            for _ in 0..8 {
                let tracker = &tracker;
                scope.spawn(move || {
                    for i in 0..2000 {
                        let sample = tracker.begin(i * 1000, 128, 1000, 1, true);
                        if i % 3 != 0 {
                            sample.finish(i * 1000 + 1, 10, true);
                        }
                    }
                });
            }
        });
        assert_eq!(tracker.sample.load(Ordering::Relaxed) & BUSY, 0);
        assert_ne!(tracker.begin(3_000_000, 128, 1000, 1, true).owner, 0);
    }

    #[cfg(feature = "preload")]
    #[test]
    fn fork_reset_clears_training_deadline_but_preserves_generations() {
        let tracker = DemandTracker::new();
        deliver(&tracker, 0, 0, 2);
        deliver(&tracker, 1000, 1000, 10);
        let inherited = tracker.begin(2000, 128, 1000, 1, true);
        tracker.reset_after_fork();
        let child = tracker.begin(2000, 128, 1000, 1, true);
        drop(inherited);
        assert_eq!(tracker.sample.load(Ordering::Relaxed), child.owner);
        child.finish(2000, 1, true);
        deliver(&tracker, 3000, 3000, 1);
        assert_eq!(tracker.expected(128), 50);
    }
}
