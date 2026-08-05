//! Snare: two detuned body tones plus a filtered noise burst.
//!
//! A snare is really two instruments glued together. The shell gives you a
//! pitched thud, usually modelled as a pair of sines a fifth-ish apart to
//! avoid sounding like a tom. The wires underneath give you the rattle, which
//! is highpassed noise with its own, usually longer, decay.
//!
//! The `noise_mix` parameter is the single most useful control here: sweep it
//! and you travel from tom, through snare, to something closer to a clap.

use crate::dsp::filter::cutoff_coeff;
use crate::dsp::{decay_coeff, fast, DecayEnv, Noise, OnePoleHp, SineOsc};
use crate::SAMPLE_RATE;

/// Snare parameters.
#[derive(Clone, Copy)]
#[cfg_attr(feature = "debug-params", derive(Debug))]
pub struct SnareParams {
    /// Fundamental of the shell, Hz.
    pub body_hz: f32,
    /// Ratio of the second body tone to the first. Around 1.5 avoids a tom.
    pub body_ratio: f32,
    /// Body decay, seconds.
    pub body_decay_s: f32,
    /// Noise decay, seconds. Longer than the body gives a wetter snare.
    pub decay_s: f32,
    /// Highpass cutoff applied to the noise, Hz.
    pub noise_hp_hz: f32,
    /// Balance between body and noise. 0.0 is pure body, 1.0 pure rattle.
    pub noise_mix: f32,
    /// Output level, linear.
    pub level: f32,
}

impl Default for SnareParams {
    fn default() -> Self {
        Self {
            body_hz: 185.0,
            body_ratio: 1.48,
            body_decay_s: 0.12,
            decay_s: 0.19,
            noise_hp_hz: 1200.0,
            noise_mix: 0.62,
            level: 0.7,
        }
    }
}

/// Snare voice.
pub struct Snare {
    body_a: SineOsc,
    body_b: SineOsc,
    body_env: DecayEnv,
    noise: Noise,
    noise_env: DecayEnv,
    hp: OnePoleHp,
    body_gain: f32,
    noise_gain: f32,
    level: f32,
}

impl Snare {
    /// Build a snare from parameters.
    pub fn new(p: &SnareParams) -> Self {
        let mut s = Self {
            body_a: SineOsc::new(),
            body_b: SineOsc::new(),
            body_env: DecayEnv::new(0.0),
            noise: Noise::new(0x9E37_79B9),
            noise_env: DecayEnv::new(0.0),
            hp: OnePoleHp::new(0.0),
            body_gain: 0.0,
            noise_gain: 0.0,
            level: p.level,
        };
        s.set_params(p);
        s
    }

    /// Recompute coefficients. Setup-time, not real-time.
    pub fn set_params(&mut self, p: &SnareParams) {
        self.body_a.set_freq(p.body_hz);
        self.body_b.set_freq(p.body_hz * p.body_ratio);
        self.body_env
            .set_coeff(decay_coeff(p.body_decay_s, SAMPLE_RATE));
        self.noise_env
            .set_coeff(decay_coeff(p.decay_s, SAMPLE_RATE));
        self.hp.set_coeff(cutoff_coeff(p.noise_hp_hz, SAMPLE_RATE));

        // Equal-ish power crossfade would be nicer, but this is one multiply
        // and the difference is inaudible over a 200ms burst.
        let mix = p.noise_mix.clamp(0.0, 1.0);
        self.body_gain = 1.0 - mix;
        self.noise_gain = mix;
        self.level = p.level;
    }

    /// Hit it.
    pub fn trigger(&mut self, velocity: f32) {
        self.body_env.trigger(velocity);
        self.noise_env.trigger(velocity);
        self.body_a.reset_phase();
        self.body_b.reset_phase();
    }

    /// Silence.
    pub fn reset(&mut self) {
        self.body_env.reset();
        self.noise_env.reset();
        self.hp.reset();
    }

    /// Still sounding?
    pub fn is_active(&self) -> bool {
        self.body_env.is_active() || self.noise_env.is_active()
    }

    /// One sample.
    #[inline(always)]
    pub fn tick(&mut self) -> f32 {
        let body_amp = self.body_env.tick();
        let noise_amp = self.noise_env.tick();

        if body_amp == 0.0 && noise_amp == 0.0 {
            return 0.0;
        }

        // The two body tones at equal weight; halving keeps the sum in range.
        let body = (self.body_a.tick() + self.body_b.tick()) * 0.5 * body_amp;
        let rattle = self.hp.tick(self.noise.tick()) * noise_amp;

        let mixed = body * self.body_gain + rattle * self.noise_gain;
        fast::soft_clip(mixed) * self.level
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn peak_over(s: &mut Snare, n: usize) -> f32 {
        let mut peak = 0.0f32;
        for _ in 0..n {
            peak = peak.max(libm::fabsf(s.tick()));
        }
        peak
    }

    #[test]
    fn silent_until_struck() {
        let mut s = Snare::new(&SnareParams::default());
        assert_eq!(peak_over(&mut s, 1000), 0.0);
    }

    #[test]
    fn full_noise_mix_still_makes_sound() {
        let mut p = SnareParams::default();
        p.noise_mix = 1.0;
        let mut s = Snare::new(&p);
        s.trigger(1.0);
        assert!(peak_over(&mut s, 4800) > 0.05);
    }

    #[test]
    fn zero_noise_mix_still_makes_sound() {
        let mut p = SnareParams::default();
        p.noise_mix = 0.0;
        let mut s = Snare::new(&p);
        s.trigger(1.0);
        assert!(peak_over(&mut s, 4800) > 0.05);
    }

    #[test]
    fn decays_to_silence() {
        let mut s = Snare::new(&SnareParams::default());
        s.trigger(1.0);
        for _ in 0..(5.0 * SAMPLE_RATE) as usize {
            s.tick();
        }
        assert!(!s.is_active());
    }
}
