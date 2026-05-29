use crate::{internals::once::Once, utility::NUM_SIZE_CLASSES};

pub static mut PREDICTOR_INIT_BATCH: usize = 8;

pub struct Predictor {
    ema: f32,
    batch: usize,
    once: Once,
}

impl Predictor {
    pub const fn new() -> Self {
        Self {
            ema: 1.0,
            batch: 1,
            once: Once::new(),
        }
    }

    pub fn update_global_batch_value(&mut self) {
        self.once.call_once(|| {
            self.batch = unsafe { PREDICTOR_INIT_BATCH };
        });
    }

    #[inline(always)]
    pub fn update_refill(&mut self, demand: usize, min: usize, max: usize) {
        self.update_global_batch_value();

        const ALPHA: f32 = 0.15;

        let demand = demand.max(1);

        self.ema = ALPHA * demand as f32 + (1.0 - ALPHA) * self.ema;
        self.batch = (self.ema.ceil() as usize).clamp(min, max);
    }

    #[inline(always)]
    pub fn batch(&self, fallback: usize) -> usize {
        self.batch.max(1).min(fallback)
    }
}

#[thread_local]
pub static mut PREDICTOR: [Predictor; NUM_SIZE_CLASSES] =
    [const { Predictor::new() }; NUM_SIZE_CLASSES];
