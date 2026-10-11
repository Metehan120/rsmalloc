use crate::utility::{NUM_SIZE_CLASSES, unlikely};
#[cfg(feature = "predictive-demand")]
use std::sync::atomic::AtomicU32;
use std::sync::atomic::{AtomicUsize, Ordering};

pub const DEFAULT_BATCH: usize = 128;
pub static mut PREDICTOR_INIT_BATCH: usize = DEFAULT_BATCH;
pub static mut BULK_FILL_PREDICTOR_INIT_BATCH: usize = 384;

pub struct AdaptiveBatching {
    state: AtomicUsize,
    #[cfg(feature = "predictive-demand")]
    demand: AtomicUsize,
    #[cfg(feature = "predictive-demand")]
    unused: AtomicU32,
}

impl AdaptiveBatching {
    #[inline(always)]
    const fn encode(batch: usize, low_count: u8) -> usize {
        (batch << 8) | low_count as usize
    }

    #[inline(always)]
    fn decode(state: usize, init_batch: usize) -> (usize, u8) {
        if unlikely(state == 0) {
            (init_batch.max(1), 0)
        } else {
            (state >> 8, state as u8)
        }
    }

    #[inline(always)]
    pub fn update_refill(&self, init_batch: usize, demand: usize, max: usize) {
        let old = self.state.load(Ordering::Relaxed);
        let (batch, low_count) = Self::decode(old, init_batch);
        let demand = demand.max(1);

        let (next_batch, next_low_count) = if demand > batch {
            ((batch + (batch >> 1)).max(demand).min(max), 0)
        } else if demand.saturating_mul(4) < batch {
            if low_count >= 3 {
                ((batch >> 1).max(1), 0)
            } else {
                (batch, low_count + 1)
            }
        } else {
            (batch, 0)
        };

        let new = Self::encode(next_batch, next_low_count);
        if new != old {
            let _ =
                self.state
                    .compare_exchange_weak(old, new, Ordering::Relaxed, Ordering::Relaxed);
        }
    }

    #[cfg(not(feature = "confidence-predictor"))]
    #[inline(always)]
    pub fn update_transfer(&self, init_batch: usize, available: usize, max: usize) {
        let old = self.state.load(Ordering::Relaxed);
        let (batch, _) = Self::decode(old, init_batch);
        let batch = batch.clamp(1, max);
        #[cfg(feature = "predictive-demand")]
        let target = (((available >> 2) + self.expected_demand(init_batch)) >> 1).clamp(1, max);
        #[cfg(not(feature = "predictive-demand"))]
        let target = (available >> 2).clamp(1, max);
        let next = if target > batch {
            batch + ((target - batch) >> 2).max(1)
        } else if target < batch {
            batch - ((batch - target) >> 2).max(1)
        } else {
            batch
        };

        let new = Self::encode(next, 0);
        if new != old {
            let _ =
                self.state
                    .compare_exchange_weak(old, new, Ordering::Relaxed, Ordering::Relaxed);
        }
    }

    #[cfg(feature = "predictive-demand")]
    #[inline(always)]
    pub fn expected_demand(&self, init: usize) -> usize {
        let demand = self.demand.load(Ordering::Relaxed);
        if unlikely(demand == 0) { init } else { demand }
    }

    #[cfg(feature = "predictive-demand")]
    #[inline(always)]
    pub fn update_demand(&self, cached: usize, init: usize, max: usize) {
        let old = self.demand.load(Ordering::Relaxed);
        let demand = if unlikely(old == 0) {
            init.clamp(1, max)
        } else {
            old
        };
        let unused = self.unused.load(Ordering::Relaxed) as usize;
        let next = if cached < unused {
            (demand * 2).min(max)
        } else {
            (demand / 2).max(1)
        };
        if next != old {
            self.demand.store(next, Ordering::Relaxed);
        }
        self.unused
            .store(demand.saturating_sub(cached) as u32, Ordering::Relaxed);
    }

    #[cfg(feature = "confidence-predictor")]
    #[inline(always)]
    fn decode_transfer(state: usize, init_batch: usize) -> (usize, u8, u8) {
        if unlikely(state == 0) {
            (init_batch.max(1), 5, 0)
        } else {
            let (batch, stats) = Self::decode(state, init_batch);
            let (confidence, low) = confidence_policy::unpack(stats);
            (batch, confidence, low)
        }
    }

    #[cfg(feature = "confidence-predictor")]
    #[inline(always)]
    pub fn transfer_batch(&self, init_batch: usize, fallback: usize) -> (usize, bool) {
        let state = self.state.load(Ordering::Relaxed);
        let (batch, confidence, low) = Self::decode_transfer(state, init_batch);
        let is_low = confidence_policy::is_low(confidence, low);
        let candidate = batch.min(fallback);
        (confidence_policy::request(candidate, is_low), is_low)
    }

    #[cfg(feature = "confidence-predictor")]
    #[inline(always)]
    pub fn update_transfer_feedback(
        &self,
        init_batch: usize,
        available: usize,
        requested: usize,
        obtained: usize,
        was_low: bool,
        max: usize,
    ) {
        let old = self.state.load(Ordering::Relaxed);
        let (batch, confidence, low) = Self::decode_transfer(old, init_batch);
        let (next, confidence, low) = confidence_policy::next(
            batch,
            confidence,
            low,
            available,
            requested,
            obtained,
            was_low,
            max,
            #[cfg(feature = "predictive-demand")]
            self.expected_demand(init_batch),
        );
        let new = Self::encode(next, confidence_policy::pack(confidence, low));
        if new != old {
            let _ =
                self.state
                    .compare_exchange_weak(old, new, Ordering::Relaxed, Ordering::Relaxed);
        }
    }

    #[inline(always)]
    pub fn batch(&self, init_batch: usize, fallback: usize) -> usize {
        let state = self.state.load(Ordering::Relaxed);
        let (batch, _) = Self::decode(state, init_batch);
        batch.min(fallback)
    }
}

#[cfg(feature = "confidence-predictor")]
mod confidence_policy {
    #[inline(always)]
    pub(super) fn request(candidate: usize, is_low: bool) -> usize {
        if is_low {
            (candidate - candidate.div_ceil(3)).max(1).min(candidate)
        } else {
            candidate
        }
    }

    #[inline(always)]
    pub(super) fn pack(confidence: u8, low: u8) -> u8 {
        confidence | (low << 4)
    }

    #[inline(always)]
    pub(super) fn unpack(stats: u8) -> (u8, u8) {
        // Initialization and capped score transitions preserve 0..=10.
        (stats & 0x0f, stats >> 4)
    }

    #[inline(always)]
    pub(super) fn is_low(confidence: u8, low: u8) -> bool {
        low != 0 && (confidence / 2) <= low
    }

    #[inline(always)]
    pub(super) fn next(
        batch: usize,
        confidence: u8,
        low: u8,
        available: usize,
        requested: usize,
        obtained: usize,
        was_low: bool,
        max: usize,
        #[cfg(feature = "predictive-demand")] expected_demand: usize,
    ) -> (usize, u8, u8) {
        let batch = batch.clamp(1, max);
        let supply = available.max(obtained).min(max);

        let (confidence, low) = if obtained < requested {
            (confidence.saturating_sub(1), (low + 2).min(10))
        } else if was_low {
            (confidence, low.saturating_sub(1))
        } else {
            ((confidence + 1).min(10), low.saturating_sub(1))
        };

        let next = if obtained < requested {
            batch.min(obtained)
        } else if was_low {
            batch
        } else {
            #[cfg(feature = "predictive-demand")]
            if expected_demand < batch {
                return (
                    (batch - ((batch - expected_demand) >> 3).max(1)).min(supply),
                    confidence,
                    low,
                );
            }
            if confidence >= 8 {
                #[cfg(feature = "predictive-demand")]
                let growth = (expected_demand >> 3).max(1);
                #[cfg(not(feature = "predictive-demand"))]
                let growth = (batch >> 3).max(1);
                // Class limits bound the candidate and the growth increment.
                (batch + growth).min(supply)
            } else {
                batch.min(supply)
            }
        };
        (next, confidence, low)
    }
}

pub const EMA_ALPHA: f32 = 0.25;

pub struct EmaSmoothing {
    ema: f32,
    time: usize,
}

impl EmaSmoothing {
    pub const fn new() -> Self {
        Self {
            ema: 10.0,
            time: 10,
        }
    }

    #[inline(always)]
    pub unsafe fn update_refill(&mut self, demand: usize, min: usize, max: usize) {
        let demand = demand.max(1);

        self.ema = EMA_ALPHA * demand as f32 + (1.0 - EMA_ALPHA) * self.ema;
        self.time = (self.ema.ceil() as usize).clamp(min, max);
    }

    #[inline(always)]
    pub unsafe fn time(&mut self, fallback: usize) -> usize {
        let out = self.time.max(1).min(fallback);

        out
    }
}

pub static mut TRIM_SMOOTHING: [EmaSmoothing; NUM_SIZE_CLASSES] =
    [const { EmaSmoothing::new() }; NUM_SIZE_CLASSES];
