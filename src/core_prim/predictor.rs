use crate::{internals::once::Once, utility::NUM_SIZE_CLASSES};

pub const DEFAULT_BATCH: usize = 128;
pub static mut PREDICTOR_INIT_BATCH: usize = DEFAULT_BATCH;
pub static mut BULK_FILL_PREDICTOR_INIT_BATCH: usize = 384;

pub struct Predictor {
    batch: usize,
    low_count: u8,
    once: Once,
    is_fill: bool,
    _class: usize,
}

impl Predictor {
    pub const fn new(fill: bool, class: usize) -> Self {
        Self {
            batch: 1,
            low_count: 0,
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
            self.low_count = 0;
        });
    }

    #[inline(always)]
    pub unsafe fn update_refill(&mut self, demand: usize, max: usize) {
        self.update_global_batch_value();

        let demand = demand.max(1);
        let batch = self.batch;

        if demand > batch {
            let grow = (batch + (batch >> 1)).max(demand);
            self.batch = grow.min(max);
            self.low_count = 0;
            return;
        }

        if demand * 4 < batch {
            self.low_count += 1;

            if self.low_count == 4 {
                self.batch = (batch >> 1).max(1);
                self.low_count = 0;
            }
        } else {
            self.low_count = 0;
        }
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

pub const EMA_ALPHA: f32 = 0.25;

pub struct EmaPredictor {
    ema: f32,
    time: usize,
}

impl EmaPredictor {
    pub const fn new() -> Self {
        Self {
            ema: 1000.0,
            time: 1000,
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

pub static mut TRIM_PREDICTOR: [EmaPredictor; NUM_SIZE_CLASSES] =
    [const { EmaPredictor::new() }; NUM_SIZE_CLASSES];
