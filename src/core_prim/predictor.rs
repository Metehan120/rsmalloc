use std::sync::atomic::{AtomicUsize, Ordering};

use crate::utility::NUM_SIZE_CLASSES;

pub const DEFAULT_BATCH: usize = 128;
pub static mut PREDICTOR_INIT_BATCH: usize = DEFAULT_BATCH;
pub static mut BULK_FILL_PREDICTOR_INIT_BATCH: usize = 384;

pub struct AdaptiveBatching {
    state: AtomicUsize,
}

impl AdaptiveBatching {
    #[inline(always)]
    const fn encode(batch: usize, low_count: u8) -> usize {
        (batch << 8) | low_count as usize
    }

    #[inline(always)]
    fn decode(state: usize, init_batch: usize) -> (usize, u8) {
        if state == 0 {
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

    #[inline(never)]
    pub fn update_refill_noninline(&self, init_batch: usize, demand: usize, max: usize) {
        self.update_refill(init_batch, demand, max);
    }

    #[inline(always)]
    pub fn batch(&self, init_batch: usize, fallback: usize) -> usize {
        let state = self.state.load(Ordering::Relaxed);
        let (batch, _) = Self::decode(state, init_batch);
        batch.min(fallback)
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
