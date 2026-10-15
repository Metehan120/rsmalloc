#![cfg(feature = "experimental-predictive-demand")]
#![allow(unused)]

use std::sync::atomic::{AtomicU64, Ordering};

use rsmalloc_macro::assert_sizes;

use crate::utility::unlikely;

const TICK_MASK: u64 = (1 << 46) - 1;
const OVERLAP: u64 = 1 << 62;
const BUSY: u64 = 1 << 63;
const GENERATION_MASK: u64 = OVERLAP - 1;
const IDLE_NS: u64 = 100_000_000;
const HORIZON_NS: u64 = 50_000;
const PREDICTION_INTERVAL_NS: u64 = 200_000;
const BYTE_BUDGET: usize = 64 * 1024;
const INITIALIZED: u64 = 1 << 32;

#[assert_sizes(32)]
#[repr(C)]
pub(super) struct DemandTracker {
    model: AtomicU64,
    sample: AtomicU64,
    generation: AtomicU64,
    last_prediction: AtomicU64,
}

#[derive(Clone, Copy)]
struct Model {
    previous: u16,
    next: u16,
    initialized: bool,
}

impl Model {
    fn unpack(word: u64, init: usize, cap: usize) -> Self {
        Self {
            previous: word as u16,
            next: if word == 0 {
                init.max(1).min(cap) as u16
            } else {
                ((word >> 16) as u16 as usize).clamp(1, cap) as u16
            },
            initialized: word & INITIALIZED != 0,
        }
    }

    fn pack(self) -> u64 {
        self.previous as u64
            | ((self.next as u64) << 16)
            | if self.initialized { INITIALIZED } else { 0 }
    }

    fn observe(&mut self, demand: u16, cap: usize) {
        let observed = demand as i64;
        let trend = if self.initialized {
            let limit = (observed / 2).max(1);
            (observed - self.previous as i64).clamp(-limit, limit)
        } else {
            0
        };
        self.next = (observed + trend).clamp(1, cap as i64) as u16;
        self.previous = demand;
        self.initialized = true;
    }
}

impl DemandTracker {
    #[cfg(test)]
    pub(super) const fn new() -> Self {
        Self {
            model: AtomicU64::new(0),
            sample: AtomicU64::new(0),
            generation: AtomicU64::new(0),
            last_prediction: AtomicU64::new(0),
        }
    }

    #[inline]
    pub(super) fn expected(&self, init: usize) -> usize {
        let demand = (self.model.load(Ordering::Relaxed) >> 16) as u16 as usize;
        if unlikely(demand == 0) {
            init.max(1)
        } else {
            demand
        }
    }

    pub(super) fn begin(
        &self,
        now: u64,
        init: usize,
        max: usize,
        block_stride: usize,
        cache_empty: bool,
    ) -> DemandSample<'_> {
        let cap = max
            .max(1)
            .min(u16::MAX as usize)
            .min((BYTE_BUDGET / block_stride.max(1)).max(1));
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
            start_empty: cache_empty,
            previous,
            started: now,
            initial: init.max(1).min(cap) as u16,
            cap: cap as u16,
            #[cfg(any(test, feature = "predictor-debug"))]
            demand: Model::unpack(self.model.load(Ordering::Relaxed), init, cap).next as usize,
        }
    }

    #[inline(never)]
    fn update_prediction(
        &self,
        previous: u64,
        started: u64,
        finished: u64,
        init: usize,
        cap: usize,
    ) {
        let old = self.model.load(Ordering::Relaxed);
        let mut model = Model::unpack(old, init, cap);
        // Forecast at finish, but measure consumption only up to refill start;
        // current refill work must not lengthen the previous depletion interval.
        let elapsed = started.wrapping_sub(previous & TICK_MASK) & TICK_MASK;
        if elapsed >= IDLE_NS {
            model = Model::unpack(0, init, cap);
        } else {
            let delivered = (previous >> 46) as u16;
            let demand =
                (delivered as u64 * HORIZON_NS / elapsed.max(1)).clamp(1, cap as u64) as u16;
            model.observe(demand, cap);
        }
        if self
            .model
            .compare_exchange(old, model.pack(), Ordering::Relaxed, Ordering::Relaxed)
            .is_ok()
        {
            // Zero is uninitialized; time + 1 also represents an update at t=0.
            self.last_prediction
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
        self.last_prediction.store(0, Ordering::Relaxed);
    }
}

pub(crate) struct DemandSample<'a> {
    tracker: &'a DemandTracker,
    owner: u64,
    start_empty: bool,
    previous: u64,
    started: u64,
    initial: u16,
    cap: u16,
    #[cfg(any(test, feature = "predictor-debug"))]
    demand: usize,
}

impl DemandSample<'_> {
    #[cfg(any(test, feature = "predictor-debug"))]
    pub(crate) fn demand(&self) -> usize {
        self.demand
    }

    pub(crate) fn finish(mut self, now: u64, delivered: usize, reliable: bool) {
        let complete =
            if self.start_empty && reliable && (1..=u16::MAX as usize).contains(&delivered) {
                (now & TICK_MASK) | ((delivered as u64) << 46)
            } else {
                0
            };
        if complete != 0 && self.owner != 0 {
            let last = self.tracker.last_prediction.load(Ordering::Relaxed);
            if (last == 0 || now.wrapping_sub(last.wrapping_sub(1)) >= PREDICTION_INTERVAL_NS)
                && (self.previous >> 46) as u16 != 0
                && self.tracker.sample.load(Ordering::Relaxed) == self.owner
            {
                self.tracker.update_prediction(
                    self.previous,
                    self.started,
                    now,
                    self.initial as usize,
                    self.cap as usize,
                );
            }
        }
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
