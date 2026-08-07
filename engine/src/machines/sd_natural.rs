//! SD Natural: two detuned body tones plus a filtered noise burst.
//!
//! Two voices glued together. The shell is a pair of sines a fifth-ish apart,
//! to avoid sounding like a tom. The wires are highpassed noise with their own
//! — usually longer — decay.
//!
//! The `NMIX` macro is the single most useful control here: sweep it and you
//! travel from tom, through snare, to something closer to a clap.
//!
//! # Macros
//!
//! | idx | name   | range         | notes |
//! |-----|--------|---------------|-------|
//! | 0   | TUNE   | 100..400 Hz   | shell fundamental |
//! | 1   | RATIO  | 1.0..2.0      | second tone relative to first |
//! | 2   | BDEC   | 40..640 ms    | body decay |
//! | 3   | NDEC   | 30..830 ms    | noise decay |
//! | 4   | HPF    | 400..4000 Hz  | noise highpass |
//! | 5   | NMIX   | 0..1          | body↔rattle crossfade |
//! | 6   | LEVEL  | 0..1          | per-machine output level |
//! | 7   | RESV   | (reserved)    | noise LP resonance (phase 2) |

use crate::dsp::filter::cutoff_coeff;
use crate::dsp::{decay_coeff, fast, DecayEnv, Noise, OnePoleHp, SineOsc};
use crate::machines::NUM_MACROS;
use crate::SAMPLE_RATE;

/// SD Natural machine.
pub struct SdNatural {
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

impl SdNatural {
    /// Build the machine with the given macro values applied.
    pub fn new(macros: &[f32; NUM_MACROS]) -> Self {
        let mut s = Self {
            body_a: SineOsc::new(),
            body_b: SineOsc::new(),
            body_env: DecayEnv::new(0.0),
            noise: Noise::new(0x9E37_79B9),
            noise_env: DecayEnv::new(0.0),
            hp: OnePoleHp::new(0.0),
            body_gain: 0.0,
            noise_gain: 0.0,
            level: 0.0,
        };
        s.set_macros(macros);
        s
    }

    /// Recompute coefficients from macros. Setup rate.
    pub fn set_macros(&mut self, macros: &[f32; NUM_MACROS]) {
        let body_hz = 100.0 + 300.0 * macros[0]; // TUNE 100..400 Hz
        let body_ratio = 1.0 + macros[1]; // RATIO 1.0..2.0
        let body_decay_s = 0.04 + 0.6 * macros[2]; // BDEC 40..640 ms
        let noise_decay_s = 0.03 + 0.8 * macros[3]; // NDEC 30..830 ms
        let noise_hp_hz = 400.0 + 3600.0 * macros[4]; // HPF 400..4000 Hz
        let noise_mix = macros[5]; // NMIX 0..1
        let level = macros[6]; // LEVEL 0..1

        self.body_a.set_freq(body_hz);
        self.body_b.set_freq(body_hz * body_ratio);
        self.body_env
            .set_coeff(decay_coeff(body_decay_s, SAMPLE_RATE));
        self.noise_env
            .set_coeff(decay_coeff(noise_decay_s, SAMPLE_RATE));
        self.hp.set_coeff(cutoff_coeff(noise_hp_hz, SAMPLE_RATE));

        // Equal-ish power crossfade would be nicer, but this is one multiply
        // and the difference is inaudible over a 200ms burst.
        self.body_gain = 1.0 - noise_mix.clamp(0.0, 1.0);
        self.noise_gain = noise_mix.clamp(0.0, 1.0);
        self.level = level;
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

        // Two body tones at equal weight; halving keeps the sum in range.
        let body = (self.body_a.tick() + self.body_b.tick()) * 0.5 * body_amp;
        let rattle = self.hp.tick(self.noise.tick()) * noise_amp;

        let mixed = body * self.body_gain + rattle * self.noise_gain;
        fast::soft_clip(mixed) * self.level
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::machines::MachineId;

    fn peak_over(s: &mut SdNatural, n: usize) -> f32 {
        let mut peak = 0.0f32;
        for _ in 0..n {
            peak = peak.max(libm::fabsf(s.tick()));
        }
        peak
    }

    #[test]
    fn silent_until_struck() {
        let macros = MachineId::SdNatural.default_macros();
        let mut s = SdNatural::new(&macros);
        assert_eq!(peak_over(&mut s, 1000), 0.0);
    }

    #[test]
    fn full_noise_mix_still_makes_sound() {
        let mut macros = MachineId::SdNatural.default_macros();
        macros[5] = 1.0; // NMIX
        let mut s = SdNatural::new(&macros);
        s.trigger(1.0);
        assert!(peak_over(&mut s, 4800) > 0.05);
    }

    #[test]
    fn zero_noise_mix_still_makes_sound() {
        let mut macros = MachineId::SdNatural.default_macros();
        macros[5] = 0.0; // NMIX
        let mut s = SdNatural::new(&macros);
        s.trigger(1.0);
        assert!(peak_over(&mut s, 4800) > 0.05);
    }

    #[test]
    fn decays_to_silence() {
        let macros = MachineId::SdNatural.default_macros();
        let mut s = SdNatural::new(&macros);
        s.trigger(1.0);
        for _ in 0..(5.0 * SAMPLE_RATE) as usize {
            s.tick();
        }
        assert!(!s.is_active());
    }
}
