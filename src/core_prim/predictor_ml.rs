//! Small, allocation-free Q12 AR(2) demand model. Packing is independent of
//! synchronization; the caller owns storage and publication of the word.

const ONE: i64 = 4096;
const SLOPE_LIMIT: i64 = 4 * ONE;

#[derive(Clone, Copy)]
pub(super) struct LinearDemand {
    bias: i16,
    w_latest: i16,
    w_previous: i16,
    latest: u16,
    older: u16,
    next: u16,
    have_history: bool,
    cap: u16,
}

impl LinearDemand {
    // Valid callers supply 1..=65535. Saturating here also makes malformed
    // external inputs safe, without narrowing an unchecked usize.
    fn bounded_cap(cap: usize) -> u16 {
        cap.clamp(1, u16::MAX as usize) as u16
    }

    pub(super) fn cold(init: usize, cap: usize) -> Self {
        let cap = Self::bounded_cap(cap);
        Self {
            bias: 0,
            w_latest: (2 * ONE) as i16,
            w_previous: -ONE as i16,
            latest: 0,
            older: 0,
            next: init.clamp(1, cap as usize) as u16,
            have_history: false,
            cap,
        }
    }

    pub(super) fn decode(word: u128, init: usize, cap: usize) -> Self {
        let cap = Self::bounded_cap(cap);
        if word == 0 || (word >> 97) as u16 != cap {
            return Self::cold(init, cap as usize);
        }
        Self {
            bias: (word as i16).clamp(-ONE as i16, ONE as i16),
            w_latest: ((word >> 16) as i16).clamp(-SLOPE_LIMIT as i16, SLOPE_LIMIT as i16),
            w_previous: ((word >> 32) as i16).clamp(-SLOPE_LIMIT as i16, SLOPE_LIMIT as i16),
            latest: ((word >> 48) as u16).min(ONE as u16),
            older: ((word >> 64) as u16).min(ONE as u16),
            next: ((word >> 80) as u16).clamp(1, cap),
            have_history: (word >> 96) & 1 != 0,
            cap,
        }
    }

    pub(super) fn encode(self) -> u128 {
        self.bias as u16 as u128
            | ((self.w_latest as u16 as u128) << 16)
            | ((self.w_previous as u16 as u128) << 32)
            | ((self.latest as u128) << 48)
            | ((self.older as u128) << 64)
            | ((self.next as u128) << 80)
            | ((self.have_history as u128) << 96)
            | ((self.cap as u128) << 97)
    }

    fn raw_prediction(self) -> i64 {
        // Q24 -> Q12, truncating toward zero (also used for learning).
        (self.bias as i64 * ONE
            + self.w_latest as i64 * self.latest as i64
            + self.w_previous as i64 * self.older as i64)
            / ONE
    }

    pub(super) fn observe(&mut self, observed: usize, cap: usize) {
        let cap = Self::bounded_cap(cap);
        if self.cap != cap {
            *self = Self::cold(observed, cap as usize);
        }
        let observed = observed.clamp(1, cap as usize) as i64;
        let target = (observed * ONE + cap as i64 / 2) / cap as i64;
        if self.have_history {
            // Train on the OLD features that predicted this observation, not
            // on features containing the observation itself. Do not clip raw.
            let error = target - self.raw_prediction();
            let latest = self.latest as i64;
            let older = self.older as i64;
            let denominator = (ONE * ONE + latest * latest + older * older) * 8;
            let update = |weight: i16, feature: i64, limit: i64| {
                (weight as i64 + error * feature * ONE / denominator).clamp(-limit, limit) as i16
            };
            self.bias = update(self.bias, ONE, ONE);
            self.w_latest = update(self.w_latest, latest, SLOPE_LIMIT);
            self.w_previous = update(self.w_previous, older, SLOPE_LIMIT);
            self.older = self.latest;
        } else {
            self.older = target as u16;
            self.have_history = true;
        }
        self.latest = target as u16;
        let normalized = self.raw_prediction().clamp(0, ONE);
        self.next = ((normalized * cap as i64 + ONE / 2) / ONE).clamp(1, cap as i64) as u16;
    }

    #[cfg(any(test, feature = "predictor-debug"))]
    pub(super) fn forecast(self) -> usize {
        self.next as usize
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::eprintln;

    fn params(model: LinearDemand) -> [i16; 3] {
        [model.bias, model.w_latest, model.w_previous]
    }

    fn assert_bounded(model: LinearDemand) {
        assert!((1..=model.cap as usize).contains(&model.forecast()));
        assert!((-(ONE as i16)..=ONE as i16).contains(&model.bias));
        for weight in [model.w_latest, model.w_previous] {
            assert!((-(SLOPE_LIMIT as i16)..=SLOPE_LIMIT as i16).contains(&weight));
        }
        assert!(model.latest <= ONE as u16 && model.older <= ONE as u16);
        assert_eq!(model.encode() >> 113, 0);
    }

    #[test]
    fn cold_and_first_sample_persist() {
        let mut model = LinearDemand::decode(0, 37, 256);
        assert_eq!(model.forecast(), 37);
        assert_eq!(params(model), [0, 8192, -4096]);
        model.observe(64, 256);
        assert_eq!(model.latest, 1024);
        assert_eq!(model.older, 1024);
        assert_eq!(model.forecast(), 64);
        assert_eq!(params(model), [0, 8192, -4096]);
        assert_eq!(LinearDemand::cold(0, 256).forecast(), 1);
        assert_eq!(LinearDemand::cold(usize::MAX, 256).forecast(), 256);
    }

    #[test]
    fn trains_old_features_then_predicts_next() {
        let mut model = LinearDemand::cold(1, 256);
        model.observe(64, 256);
        model.observe(128, 256);
        // error=1024, features=[4096,1024,1024], norm=18_874_368.
        assert_eq!(params(model), [113, 8220, -4068]);
        assert_eq!((model.latest, model.older), (2048, 1024));
        assert_eq!(model.forecast(), 200);
    }

    #[test]
    fn gradients_match_float_reference() {
        let mut seed = 0x1324_5678_u64;
        for _ in 0..2000 {
            let mut next = || {
                seed = seed.wrapping_mul(6364136223846793005).wrapping_add(1);
                (seed >> 32) as u32
            };
            let mut model = LinearDemand {
                bias: (next() % 8193) as i16 - 4096,
                w_latest: ((next() % 32769) as i32 - 16384) as i16,
                w_previous: ((next() % 32769) as i32 - 16384) as i16,
                latest: (next() % 4097) as u16,
                older: (next() % 4097) as u16,
                next: 1,
                have_history: true,
                cap: 4096,
            };

            let observed = (next() % 4096 + 1) as usize;
            let features = [4096.0, model.latest as f64, model.older as f64];
            let before = params(model);
            let raw = before
                .iter()
                .zip(features)
                .map(|(&w, x)| w as f64 * x / 4096.0)
                .sum::<f64>();
            let error = observed as f64 - raw;
            let norm = features.iter().map(|x| x * x).sum::<f64>();
            model.observe(observed, 4096);
            for i in 0..3 {
                let limit = if i == 0 { 4096.0 } else { 16384.0 };
                let expected = (before[i] as f64 + error * features[i] * 4096.0 / (norm * 8.0))
                    .clamp(-limit, limit);
                assert!((params(model)[i] as f64 - expected).abs() < 1.13);
            }
            assert_bounded(model);
        }
    }

    #[test]
    fn learning_uses_unclipped_prediction() {
        let mut model = LinearDemand::cold(1, 4096);
        model.have_history = true;
        model.latest = 4096;
        model.older = 0;
        model.observe(4096, 4096);
        assert_eq!(params(model), [-256, 7936, -4096]);
        assert_eq!(model.forecast(), 3584);
    }

    #[test]
    fn signed_packing_and_layout() {
        let model = LinearDemand {
            bias: -123,
            w_latest: -12345,
            w_previous: 16384,
            latest: 1023,
            older: 3001,
            next: 77,
            have_history: true,
            cap: 999,
        };
        let word = model.encode();
        assert_eq!(word as u16, (-123_i16) as u16);
        assert_eq!((word >> 16) as u16, (-12345_i16) as u16);
        assert_eq!((word >> 32) as u16, 16384);
        assert_eq!((word >> 48) as u16, 1023);
        assert_eq!((word >> 64) as u16, 3001);
        assert_eq!((word >> 80) as u16, 77);
        assert_eq!((word >> 96) & 1, 1);
        assert_eq!((word >> 97) as u16, 999);
        assert_eq!(LinearDemand::decode(word, 1, 999).encode(), word);
        assert_eq!(
            LinearDemand::decode(word | (!0_u128 << 113), 1, 999).encode(),
            word
        );
    }

    #[test]
    fn malformed_words_saturate() {
        let max = LinearDemand::decode(u128::MAX, 10, 65535);
        assert_eq!(params(max), [-1, -1, -1]);
        assert_eq!((max.latest, max.older, max.next), (4096, 4096, 65535));
        assert_bounded(max);
        let word = 0x7fff_u128
            | (0x7fff << 16)
            | (0x8000 << 32)
            | (0xffff << 48)
            | (0xffff << 64)
            | (1 << 96)
            | (256 << 97);
        let model = LinearDemand::decode(word, 99, 256);
        assert_eq!(params(model), [4096, 16384, -16384]);
        assert_eq!(model.forecast(), 1);
        assert_bounded(model);
    }

    #[test]
    fn switching_cap_resets_normalization_and_learning() {
        let mut model = LinearDemand::cold(10, 256);
        model.observe(64, 256);
        model.observe(128, 256);
        let reset = LinearDemand::decode(model.encode(), 17, 512);
        assert_eq!(reset.encode(), LinearDemand::cold(17, 512).encode());
        model.observe(128, 512);
        let mut fresh = LinearDemand::cold(128, 512);
        fresh.observe(128, 512);
        assert_eq!(model.encode(), fresh.encode());
    }

    #[test]
    fn bounded_extremes_do_not_overflow() {
        for cap in [0, 1, 2, 255, 4096, 65535, usize::MAX] {
            for bias in [-4096_i16, 4096] {
                for w_latest in [-16384_i16, 16384] {
                    for w_previous in [-16384_i16, 16384] {
                        for latest in [0, 4096] {
                            for older in [0, 4096] {
                                let mut model = LinearDemand::cold(usize::MAX, cap);
                                model.bias = bias;
                                model.w_latest = w_latest;
                                model.w_previous = w_previous;
                                model.latest = latest;
                                model.older = older;
                                model.have_history = true;
                                for observed in [0, 1, cap, usize::MAX] {
                                    model.observe(observed, cap);
                                    assert_bounded(model);
                                    model = LinearDemand::decode(model.encode(), 1, cap);
                                    assert_bounded(model);
                                }
                            }
                        }
                    }
                }
            }
        }
    }

    #[test]
    fn seeded_patterns_report_errors_and_learn() {
        for pattern in ["constant", "ramp", "alternating", "seeded"] {
            let mut model = LinearDemand::cold(256, 1024);
            let mut seed = 12345_u32;
            let mut learned_error = 0_u64;
            let mut initial_error = 0_u64;
            let mut latest = 256_i64;
            let mut older = 256_i64;
            let mut changed = false;
            for t in 0..1024 {
                seed = seed.wrapping_mul(1664525).wrapping_add(1013904223);
                let observed = match pattern {
                    "constant" => 384,
                    "ramp" => 128 + (t % 128) * 6,
                    "alternating" => {
                        if t % 2 == 0 {
                            192
                        } else {
                            832
                        }
                    }
                    _ => 128 + (seed as usize % 768),
                };
                if t > 0 {
                    learned_error += model.forecast().abs_diff(observed) as u64;
                    initial_error += (2 * latest - older)
                        .clamp(1, 1024)
                        .abs_diff(observed as i64);
                }
                model.observe(observed, 1024);
                changed |= params(model) != [0, 8192, -4096];
                older = if t == 0 { observed as i64 } else { latest };
                latest = observed as i64;
                assert_bounded(model);
            }
            if pattern == "constant" {
                assert!(!changed);
                assert_eq!(learned_error, 0);
            } else {
                assert!(changed, "weights never changed for {pattern}");
            }
            eprintln!(
                "{pattern}: MAE learned={:.3}, fixed AR(2)={:.3}",
                learned_error as f64 / 1023.0,
                initial_error as f64 / 1023.0
            );
        }
    }
}
