//! Oscillators.

use crate::INV_SAMPLE_RATE;

/// A sine oscillator driven by a normalised phase accumulator.
///
/// Phase is kept in `0.0..1.0` rather than radians so that wrapping is a
/// subtract rather than a modulo, and so retuning is a single multiply.
///
/// # The optimisation you will eventually want
///
/// [`tick`](Self::tick) calls [`crate::dsp::fast::sin_turns`], which currently
/// forwards to `libm::sinf`. That is accurate and portable and will be one of
/// the more expensive things in your per-sample budget.
///
/// When `firmware/src/bin/bench.rs` tells you it matters, swap the body of
/// `sin_turns` for a table lookup with linear interpolation, or a polynomial
/// approximation. Both live behind that one function precisely so that the
/// swap is a single edit and the host renderer immediately tells you whether
/// you can hear the difference. Do not do it before then.
#[derive(Clone, Copy)]
pub struct SineOsc {
    /// Normalised phase, `0.0..1.0`.
    phase: f32,
    /// Phase increment per sample, i.e. `freq / sample_rate`.
    inc: f32,
}

impl SineOsc {
    /// A silent oscillator at zero phase.
    #[inline]
    pub const fn new() -> Self {
        Self {
            phase: 0.0,
            inc: 0.0,
        }
    }

    /// Set frequency in Hz.
    ///
    /// Multiplies by the precomputed reciprocal sample rate rather than
    /// dividing. This is called per sample by the pitch-swept voices, so the
    /// divide would be a real cost.
    #[inline(always)]
    pub fn set_freq(&mut self, hz: f32) {
        self.inc = hz * INV_SAMPLE_RATE;
    }

    /// Current frequency in Hz.
    ///
    /// Read back when a machine needs to transpose an oscillator that does
    /// not keep its own base-frequency field — scale the current value and
    /// hand it back to [`set_freq`](Self::set_freq).
    #[inline(always)]
    pub fn freq(&self) -> f32 {
        self.inc * crate::SAMPLE_RATE
    }

    /// Reset phase to zero.
    ///
    /// Worth doing on trigger for percussion: a kick that starts at a
    /// consistent point in the cycle has a consistent transient, and an
    /// inconsistent one sounds like a fault.
    #[inline]
    pub fn reset_phase(&mut self) {
        self.phase = 0.0;
    }

    /// Advance one sample and return the output.
    #[inline(always)]
    pub fn tick(&mut self) -> f32 {
        let out = super::fast::sin_turns(self.phase);
        self.phase += self.inc;
        // Branch rather than fract() — the increment is always well under 1.0
        // at audio frequencies, so this wraps at most once and predicts nearly
        // perfectly.
        if self.phase >= 1.0 {
            self.phase -= 1.0;
        }
        out
    }

    /// Advance one sample with an added phase bias, returning the output.
    ///
    /// Used for FM: the modulator contributes a normalised-turns offset to the
    /// carrier's phase *before* lookup, without disturbing the carrier's own
    /// phase accumulator. The bias is in turns (1.0 = one cycle of phase
    /// deviation) and `sin_turns` wraps for free, so no extra modulo here.
    /// The accumulator advances as usual after the lookup.
    #[inline(always)]
    pub fn tick_with_phase_bias(&mut self, bias: f32) -> f32 {
        let out = super::fast::sin_turns(self.phase + bias);
        self.phase += self.inc;
        if self.phase >= 1.0 {
            self.phase -= 1.0;
        }
        out
    }
}

impl Default for SineOsc {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::SAMPLE_RATE;

    #[test]
    fn produces_expected_period() {
        let freq = 100.0;
        let mut osc = SineOsc::new();
        osc.set_freq(freq);

        // Count zero crossings over one second; expect 2 per cycle.
        let mut prev = osc.tick();
        let mut crossings = 0;
        for _ in 1..(SAMPLE_RATE as usize) {
            let s = osc.tick();
            if (prev < 0.0) != (s < 0.0) {
                crossings += 1;
            }
            prev = s;
        }
        assert!(
            (crossings as f32 - 2.0 * freq).abs() <= 2.0,
            "expected ~{} crossings, got {crossings}",
            2.0 * freq
        );
    }

    #[test]
    fn stays_bounded() {
        let mut osc = SineOsc::new();
        osc.set_freq(440.0);
        for _ in 0..100_000 {
            let s = osc.tick();
            assert!(s.abs() <= 1.001, "sine escaped: {s}");
        }
    }
}
