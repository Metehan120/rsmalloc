use crate::{internals::once::Once, utility::NUM_SIZE_CLASSES};

pub const DEFAULT_BATCH: usize = 128;
pub static mut PREDICTOR_INIT_BATCH: usize = DEFAULT_BATCH;
pub static mut BULK_FILL_PREDICTOR_INIT_BATCH: usize = 384;
pub static mut EMA_ALPHA: f32 = 0.15;

pub struct Predictor {
    ema: f32,
    batch: usize,
    once: Once,
    is_fill: bool,
    _class: usize,
}

impl Predictor {
    pub const fn new(fill: bool, class: usize) -> Self {
        Self {
            ema: 1.0,
            batch: 1,
            once: Once::new(),
            is_fill: fill,
            _class: class,
        }
    }

    pub unsafe fn update_global_batch_value(&mut self) {
        self.once.call_once(|| {
            let init_batch = if self.is_fill {
                unsafe { BULK_FILL_PREDICTOR_INIT_BATCH }.max(1)
            } else {
                unsafe { PREDICTOR_INIT_BATCH }.max(1)
            };

            self.batch = init_batch;
            self.ema = init_batch as f32;
        });
    }

    #[inline(always)]
    pub unsafe fn update_refill(&mut self, demand: usize, min: usize, max: usize) {
        self.update_global_batch_value();
        let demand = demand.max(1);

        self.ema = EMA_ALPHA * demand as f32 + (1.0 - EMA_ALPHA) * self.ema;
        self.batch = (self.ema.ceil() as usize).clamp(min, max);
    }

    #[inline(always)]
    pub unsafe fn batch(&mut self, fallback: usize) -> usize {
        self.update_global_batch_value();
        let out = self.batch.max(1).min(fallback);

        #[cfg(feature = "predictor-debug")]
        if self.is_fill {
            eprintln!("Predictor Bulk Fill: {} (class {})", out, self._class);
        } else {
            eprintln!("Predictor Block Batching: {} (class {})", out, self._class);
        }

        out
    }
}

#[thread_local]
pub static mut PREDICTOR: [Predictor; NUM_SIZE_CLASSES] = {
    let mut i = 0;
    let mut result = [const { Predictor::new(false, 0) }; NUM_SIZE_CLASSES];
    while i < NUM_SIZE_CLASSES {
        result[i] = Predictor::new(false, i);
        i += 1;
    }
    result
};

#[thread_local]
pub static mut BULK_FILL_PREDICTOR: [Predictor; NUM_SIZE_CLASSES] = {
    let mut i = 0;
    let mut result = [const { Predictor::new(true, 0) }; NUM_SIZE_CLASSES];
    while i < NUM_SIZE_CLASSES {
        result[i] = Predictor::new(true, i);
        i += 1;
    }
    result
};
