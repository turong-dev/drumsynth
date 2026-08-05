//! Closed hi-hat: bandpassed noise, very short decay.
//!
//! The cheapest voice here by a wide margin — no oscillator, two one-pole
//! filters and an envelope. Worth knowing when you are budgeting: if you end
//! up over on cycles, hats are not where the problem is.
//!
//! A more convincing hat uses six detuned square waves through a bandpass
//! (the 808 approach) rather than noise. That is a good second iteration, and
//! costs roughly six times as much. Start here.

use crate::dsp::filter::cutoff_coeff;
use crate::dsp::{decay_coeff, DecayEnv, Noise, OnePoleHp, OnePoleLp};
use crate::SAMPLE_RATE;

/// Hi-hat parameters.
#[derive(Clone, Copy)]
#[cfg_attr(feature = "debug-params", derive(Debug))]
pub struct HatParams {
    /// Decay, seconds. Under 100ms reads as closed.
    pub decay_s: f32,
    /// Highpass cutoff, Hz. Sets how thin the hat sounds.
    pub hp_hz: f32,
    /// Lowpass cutoff, Hz. Takes the fizz off the top.
    pub lp_hz: f32,
    /// Output level, linear.
    pub level: f32,
}

impl Default for HatParams {
    fn default() -> Self {
        Self {
            decay_s: 0.055,
            hp_hz: 6500.0,
            lp_hz: 12_000.0,
            level: 0.4,
        }
    }
}

/// Closed hi-hat voice.
pub struct Hat {
    noise: Noise,
    env: DecayEnv,
    hp: OnePoleHp,
    lp: OnePoleLp,
    level: f32,
}

impl Hat {
    /// Build a hat from parameters.
    pub fn new(p: &HatParams) -> Self {
        let mut h = Self {
            // A different seed from the snare, so the two do not correlate
            // when they land on the same step.
            noise: Noise::new(0x5851_F42D),
            env: DecayEnv::new(0.0),
            hp: OnePoleHp::new(0.0),
            lp: OnePoleLp::new(0.0),
            level: p.level,
        };
        h.set_params(p);
        h
    }

    /// Recompute coefficients. Setup-time, not real-time.
    pub fn set_params(&mut self, p: &HatParams) {
        self.env.set_coeff(decay_coeff(p.decay_s, SAMPLE_RATE));
        self.hp.set_coeff(cutoff_coeff(p.hp_hz, SAMPLE_RATE));
        self.lp.set_coeff(cutoff_coeff(p.lp_hz, SAMPLE_RATE));
        self.level = p.level;
    }

    /// Hit it.
    pub fn trigger(&mut self, velocity: f32) {
        self.env.trigger(velocity);
    }

    /// Silence.
    pub fn reset(&mut self) {
        self.env.reset();
        self.hp.reset();
        self.lp.reset();
    }

    /// Still sounding?
    pub fn is_active(&self) -> bool {
        self.env.is_active()
    }

    /// One sample.
    #[inline(always)]
    pub fn tick(&mut self) -> f32 {
        let amp = self.env.tick();
        if amp == 0.0 {
            return 0.0;
        }
        let n = self.noise.tick();
        let band = self.lp.tick(self.hp.tick(n));
        band * amp * self.level
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn silent_until_struck() {
        let mut h = Hat::new(&HatParams::default());
        for _ in 0..1000 {
            assert_eq!(h.tick(), 0.0);
        }
    }

    #[test]
    fn decays_quickly() {
        let mut h = Hat::new(&HatParams::default());
        h.trigger(1.0);
        // Half a second is a long time for a closed hat.
        for _ in 0..(0.5 * SAMPLE_RATE) as usize {
            h.tick();
        }
        assert!(!h.is_active(), "closed hat rang for too long");
    }

    #[test]
    fn output_is_bounded() {
        let mut h = Hat::new(&HatParams::default());
        for _ in 0..200 {
            h.trigger(1.0);
            for _ in 0..512 {
                let s = h.tick();
                assert!(s.abs() <= 1.0, "hat escaped: {s}");
            }
        }
    }
}
