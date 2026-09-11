//! Envelopes.

use crate::DENORMAL_FLOOR;

/// A one-shot exponential decay.
///
/// Triggered to a level, decays toward zero, flushes to exactly zero when it
/// gets small enough. No attack stage — drums generally want the transient
/// intact, and if you need to soften one, a short lowpass on the output does
/// a better job than an attack ramp.
///
/// The flush-to-zero is not cosmetic. Without it the value asymptotes into
/// denormal range and stays there forever, and on cores that handle denormals
/// in microcode every subsequent multiply is dramatically more expensive.
/// It also means [`is_active`](Self::is_active) can be an exact comparison
/// rather than a threshold test.
#[derive(Clone, Copy)]
pub struct DecayEnv {
    value: f32,
    coeff: f32,
}

impl DecayEnv {
    /// Create an idle envelope with the given per-sample decay coefficient.
    ///
    /// Use [`crate::dsp::decay_coeff`] to derive the coefficient from a time
    /// in seconds.
    #[inline]
    pub const fn new(coeff: f32) -> Self {
        Self { value: 0.0, coeff }
    }

    /// Update the decay rate. Does not disturb a sounding envelope.
    #[inline]
    pub fn set_coeff(&mut self, coeff: f32) {
        self.coeff = coeff;
    }

    /// Restart at `level`.
    #[inline]
    pub fn trigger(&mut self, level: f32) {
        self.value = level;
    }

    /// Force to silence.
    #[inline]
    pub fn reset(&mut self) {
        self.value = 0.0;
    }

    /// True while the envelope is producing non-zero output.
    #[inline]
    pub fn is_active(&self) -> bool {
        self.value != 0.0
    }

    /// Current level without advancing.
    #[inline(always)]
    pub fn peek(&self) -> f32 {
        self.value
    }

    /// Advance one sample and return the level *before* the step.
    ///
    /// Returning the pre-step value means a freshly triggered envelope yields
    /// its full trigger level on the first sample, so transients are not
    /// silently attenuated.
    #[inline(always)]
    pub fn tick(&mut self) -> f32 {
        let out = self.value;
        self.value *= self.coeff;
        if self.value < DENORMAL_FLOOR {
            self.value = 0.0;
        }
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::dsp::decay_coeff;
    use crate::SAMPLE_RATE;

    #[test]
    fn first_sample_is_full_level() {
        let mut e = DecayEnv::new(decay_coeff(0.1, SAMPLE_RATE));
        e.trigger(1.0);
        assert_eq!(e.tick(), 1.0);
    }

    #[test]
    fn reaches_exact_zero() {
        let mut e = DecayEnv::new(decay_coeff(0.01, SAMPLE_RATE));
        e.trigger(1.0);
        for _ in 0..(SAMPLE_RATE as usize) {
            e.tick();
        }
        assert!(!e.is_active(), "should have flushed to exactly zero");
        assert_eq!(e.peek(), 0.0);
    }

    #[test]
    fn hits_minus_60db_at_the_stated_time() {
        let decay_s = 0.25;
        let mut e = DecayEnv::new(decay_coeff(decay_s, SAMPLE_RATE));
        e.trigger(1.0);
        for _ in 0..((decay_s * SAMPLE_RATE) as usize) {
            e.tick();
        }
        // 0.001 is -60dB. Allow a little slack for the flush threshold.
        approx::assert_relative_eq!(e.peek(), 0.001, epsilon = 0.0002);
    }

    #[test]
    fn zero_decay_time_is_instant_not_nan() {
        let mut e = DecayEnv::new(decay_coeff(0.0, SAMPLE_RATE));
        e.trigger(1.0);
        assert_eq!(e.tick(), 1.0);
        assert!(e.peek().is_finite());
        assert!(!e.is_active());
    }
}
