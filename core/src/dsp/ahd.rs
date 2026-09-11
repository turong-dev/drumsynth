//! Attack-Hold-Decay envelope.
//!
//! Complements [`crate::dsp::DecayEnv`] (one-shot exponential). This one
//! has a linear attack stage and an explicit hold, which is what the
//! per-track *amp* envelope needs — a snare that wants a 2ms attack lip, a
//! tonal voice that wants to hold a long-decaying tail, neither of which a
//! bare decay can describe.
//!
//! The decay stage shares [`DecayEnv`]'s `-60dB-inside-N-seconds`
//! semantics. The attack stage is linear rather than exponential: percussive
//! attacks run a handful of samples, where a curve you cannot see is a
//! curve you cannot hear, and linear is one multiply per sample.
//!
//! Used by [`crate::Track`] for its amp envelope. The same struct serves
//! as the filter envelope, with the caller scaling its output by a bipolar
//! depth and adding it to the cutoff.

use crate::dsp::decay_coeff;
use crate::DENORMAL_FLOOR;
use crate::SAMPLE_RATE;

/// Where the envelope is in its segments.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Stage {
    /// Not sounding; output is zero.
    Idle,
    /// Linear rise from zero to `peak` over `attack_samples`.
    Attack,
    /// Held at `peak` for `hold_samples`.
    Hold,
    /// Exponential fall from `peak` toward zero.
    Decay,
}

/// Attack-Hold-Decay envelope, velocity-scaled.
///
/// Decay toward roughly -60dB after `decay_s`. The decay coefficient is
/// derived from `decay_s` exactly as in [`DecayEnv`]; the per-sample
/// multiply therefore behaves the same way and flushes to exact zero
/// below [`DENORMAL_FLOOR`].
#[derive(Clone, Copy)]
pub struct AhdEnv {
    stage: Stage,
    value: f32,
    peak: f32,
    attack_inc: f32,
    hold_samples: usize,
    hold_left: usize,
    decay_coeff: f32,
}

impl AhdEnv {
    /// Idle envelope with zero-length segments; configure via
    /// [`set_params`](Self::set_params).
    pub const fn new() -> Self {
        Self {
            stage: Stage::Idle,
            value: 0.0,
            peak: 1.0,
            attack_inc: 0.0,
            hold_samples: 0,
            hold_left: 0,
            decay_coeff: 0.0,
        }
    }

    /// Configure timings in seconds (setup rate). `attack_s` and
    /// `hold_s` are linear; `decay_s` is the time-to-(-60dB) exponential
    /// half-life. Any non-positive time collapses to a skipped stage.
    pub fn set_params(&mut self, attack_s: f32, hold_s: f32, decay_s: f32) {
        self.attack_inc = if attack_s > 0.0 {
            1.0 / (attack_s * SAMPLE_RATE)
        } else {
            // Zero attack: jump straight to peak — segment degenerates to a
            // single sample step.
            0.0
        };
        self.hold_samples = if hold_s > 0.0 {
            (hold_s * SAMPLE_RATE) as usize
        } else {
            0
        };
        self.decay_coeff = decay_coeff(decay_s, SAMPLE_RATE);
    }

    /// Direct decay-coefficient override, for callers that already have one
    /// (e.g. a machine reusing the same decay time as an internal envelope).
    pub fn set_decay_coeff(&mut self, coeff: f32) {
        self.decay_coeff = coeff;
    }

    /// Begin a hit at `velocity` (0.0..=1.0). Scales the *envelope's peak*,
    /// so a softer hit is shorter and quieter rather than only quieter.
    pub fn trigger(&mut self, velocity: f32) {
        let v = velocity.clamp(0.0, 1.0);
        self.peak = v;
        if self.attack_inc > 0.0 {
            self.value = 0.0;
            self.stage = Stage::Attack;
        } else {
            // Degenerate attack: start holding at full velocity.
            self.value = v;
            self.stage = if self.hold_samples > 0 {
                self.hold_left = self.hold_samples;
                Stage::Hold
            } else {
                Stage::Decay
            };
        }
    }

    /// Force to silence immediately.
    #[inline]
    pub fn reset(&mut self) {
        self.stage = Stage::Idle;
        self.value = 0.0;
    }

    /// True while the envelope is producing non-zero output.
    #[inline]
    pub fn is_active(&self) -> bool {
        self.stage != Stage::Idle
    }

    /// Current level without advancing.
    #[inline(always)]
    pub fn peek(&self) -> f32 {
        self.value
    }

    /// Advance one sample and return the level *before* the step, so a
    /// freshly triggered envelope yields its full peak on the first sample
    /// of hold/decay rather than vanishing instantly.
    #[inline(always)]
    pub fn tick(&mut self) -> f32 {
        let out = self.value;
        match self.stage {
            Stage::Idle => {}
            Stage::Attack => {
                self.value += self.attack_inc * self.peak;
                if self.value >= self.peak {
                    self.value = self.peak;
                    self.stage = if self.hold_samples > 0 {
                        self.hold_left = self.hold_samples;
                        Stage::Hold
                    } else {
                        Stage::Decay
                    };
                }
            }
            Stage::Hold => {
                self.hold_left = self.hold_left.saturating_sub(1);
                if self.hold_left == 0 {
                    self.stage = Stage::Decay;
                }
            }
            Stage::Decay => {
                self.value *= self.decay_coeff;
                if self.value < DENORMAL_FLOOR {
                    self.value = 0.0;
                    self.stage = Stage::Idle;
                }
            }
        }
        out
    }
}

impl Default for AhdEnv {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn silent_until_triggered() {
        let mut e = AhdEnv::new();
        e.set_params(0.0, 0.0, 0.1);
        for _ in 0..1000 {
            assert_eq!(e.tick(), 0.0);
        }
        assert!(!e.is_active());
    }

    #[test]
    fn zero_attack_jumps_to_peak() {
        let mut e = AhdEnv::new();
        e.set_params(0.0, 0.0, 0.1);
        e.trigger(1.0);
        assert_eq!(e.tick(), 1.0, "first sample should be the peak");
        // Already in decay; should be slightly less next sample.
        assert!(e.tick() < 1.0);
    }

    #[test]
    fn attack_ramps_linearly() {
        let mut e = AhdEnv::new();
        e.set_params(0.01, 0.0, 0.1);
        e.trigger(1.0);
        let n = (0.01 * SAMPLE_RATE) as usize;
        // The tick contract is "return the level *before* the step", so the
        // very first sample is 0 and the segment completes on transition;
        // the (n+1)th sample is the held peak. Loop n samples to see the
        // ramp, then confirm a held peak sample follows.
        let mut last = -1.0;
        for _ in 0..n {
            let v = e.tick();
            assert!(v > last, "attack should rise: {v} <= {last}");
            last = v;
        }
        // End of attack window: we should have just reached the peak.
        assert!((last - 1.0).abs() < 0.01, "did not reach peak: {last}");
        // One more tick returns the held/decayed peak.
        let peak_sample = e.tick();
        approx::assert_abs_diff_eq!(peak_sample, 1.0, epsilon = 1e-6);
    }

    #[test]
    fn hold_sits_at_peak() {
        let mut e = AhdEnv::new();
        e.set_params(0.0, 0.02, 0.1);
        e.trigger(1.0);
        let n = (0.02 * SAMPLE_RATE) as usize;
        // First tick is the held peak; n total hold ticks each yield peak.
        let first = e.tick();
        approx::assert_abs_diff_eq!(first, 1.0, epsilon = 1e-6);
        for _ in 0..(n - 1) {
            approx::assert_abs_diff_eq!(e.tick(), 1.0, epsilon = 1e-6);
        }
        // The nth hold tick transitions stage->Decay and still returns 1.0.
        approx::assert_abs_diff_eq!(e.tick(), 1.0, epsilon = 1e-6);
        assert!(matches!(e.stage, Stage::Decay), "should be in decay");
        // Next tick should be < 1.0 (the level before the next decay step).
        let decayed = e.tick();
        assert!(decayed < 1.0, "did not decay: {decayed}");
    }

    #[test]
    fn decays_to_silence() {
        let mut e = AhdEnv::new();
        e.set_params(0.0, 0.0, 0.05);
        e.trigger(1.0);
        for _ in 0..(5.0 * SAMPLE_RATE) as usize {
            e.tick();
        }
        assert!(!e.is_active(), "should have flushed to zero");
        assert_eq!(e.peek(), 0.0);
    }

    #[test]
    fn velocity_scales_peak() {
        let mut a = AhdEnv::new();
        a.set_params(0.0, 0.0, 0.1);
        a.trigger(1.0);
        let loud = a.tick();

        let mut b = AhdEnv::new();
        b.set_params(0.0, 0.0, 0.1);
        b.trigger(0.25);
        let quiet = b.tick();
        assert!(loud > quiet, "velocity had no effect: {loud} vs {quiet}");
        approx::assert_abs_diff_eq!(quiet, 0.25, epsilon = 1e-6);
    }

    #[test]
    fn degenerate_params_are_safe() {
        let mut e = AhdEnv::new();
        e.set_params(0.0, 0.0, 0.0);
        e.trigger(1.0);
        for _ in 0..1024 {
            let s = e.tick();
            assert!(s.is_finite(), "non-finite: {s}");
        }
    }
}
