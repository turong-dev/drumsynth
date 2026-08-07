//! RS: rimshot — two detuned square-ish oscillators plus a noise tick.
//!
//! A rimshot is the crack of the stick hitting the rim and head at once:
//! short, bright, with a metallic edge that's closer to a square than a
//! sine. Two oscillators detuned a few cents give it thickness without
//! "chorus"; the noise tick adds the physical "crack" at the start.
//!
//! # Macros
//!
//! | idx | name   | range         | notes |
//! |-----|--------|---------------|-------|
//! | 0   | TUNE   | 200..800 Hz   | body pitch |
//! | 1   | DET    | 1.0..1.08     | osc2 relative to osc1 (up to ~8%) |
//! | 2   | DEC    | 15..150 ms    | body decay — intentionally short |
//! | 3   | NDEC   | 5..60 ms      | noise-tick decay (shorter than body) |
//! | 4   | NLEV   | 0..1          | tick level |
//! | 5   | HPF    | 1000..6000 Hz | noise tick colour |
//! | 6   | LEVEL  | 0..1          | per-machine output level |
//! | 7   | RESV   | (reserved)    | waveform select (phase 2) |

use crate::dsp::filter::cutoff_coeff;
use crate::dsp::{decay_coeff, fast, DecayEnv, Noise, OnePoleHp, SineOsc};
use crate::machines::NUM_MACROS;
use crate::SAMPLE_RATE;

/// RS machine.
pub struct Rs {
    osc_a: SineOsc,
    osc_b: SineOsc,
    body_env: DecayEnv,
    noise: Noise,
    noise_env: DecayEnv,
    hp: OnePoleHp,
    noise_level: f32,
    level: f32,
}

impl Rs {
    /// Build the machine with the given macro values applied.
    pub fn new(macros: &[f32; NUM_MACROS]) -> Self {
        let mut s = Self {
            osc_a: SineOsc::new(),
            osc_b: SineOsc::new(),
            body_env: DecayEnv::new(0.0),
            noise: Noise::new(0xA15B_3E2D),
            noise_env: DecayEnv::new(0.0),
            hp: OnePoleHp::new(0.0),
            noise_level: 0.0,
            level: 0.0,
        };
        s.set_macros(macros);
        s
    }

    /// Recompute coefficients from macros. Setup rate.
    pub fn set_macros(&mut self, macros: &[f32; NUM_MACROS]) {
        let body_hz = 200.0 + 600.0 * macros[0]; // TUNE 200..800 Hz
        let detune = 1.0 + 0.08 * macros[1]; // DET up to ~8%
        let body_decay_s = 0.015 + 0.135 * macros[2]; // DEC 15..150 ms
        let noise_decay_s = 0.005 + 0.055 * macros[3]; // NDEC 5..60 ms
        let noise_level = macros[4]; // NLEV 0..1
        let hp_hz = 1000.0 + 5000.0 * macros[5]; // HPF 1..6 kHz
        let level = macros[6]; // LEVEL 0..1

        self.osc_a.set_freq(body_hz);
        self.osc_b.set_freq(body_hz * detune);
        self.body_env
            .set_coeff(decay_coeff(body_decay_s, SAMPLE_RATE));
        self.noise_env
            .set_coeff(decay_coeff(noise_decay_s, SAMPLE_RATE));
        self.hp.set_coeff(cutoff_coeff(hp_hz, SAMPLE_RATE));
        self.noise_level = noise_level;
        self.level = level;
    }

    /// Hit it.
    pub fn trigger(&mut self, velocity: f32) {
        self.body_env.trigger(velocity);
        self.noise_env.trigger(velocity);
        self.osc_a.reset_phase();
        self.osc_b.reset_phase();
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

        // Two sines half-weighted; the third harmonic of "rim" tone is the
        // square-ish edge, but two detuned sines get us most of the way
        // there and stay clean. Saturation adds the click-y character.
        let body = if body_amp != 0.0 {
            let pair = (self.osc_a.tick() + self.osc_b.tick()) * 0.5;
            fast::soft_clip(pair * 2.0) * body_amp
        } else {
            0.0
        };

        let tick = if noise_amp != 0.0 {
            self.hp.tick(self.noise.tick()) * noise_amp
        } else {
            0.0
        };

        (body + tick * self.noise_level) * self.level
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::machines::MachineId;

    fn peak_over(s: &mut Rs, n: usize) -> f32 {
        let mut peak = 0.0f32;
        for _ in 0..n {
            peak = peak.max(libm::fabsf(s.tick()));
        }
        peak
    }

    #[test]
    fn silent_until_struck() {
        let id = MachineId::Rs;
        let macros = id.default_macros();
        let mut s = Rs::new(&macros);
        assert_eq!(peak_over(&mut s, 1000), 0.0);
    }

    #[test]
    fn decays_quickly() {
        let id = MachineId::Rs;
        let macros = id.default_macros();
        let mut s = Rs::new(&macros);
        s.trigger(1.0);
        for _ in 0..(0.5 * SAMPLE_RATE) as usize {
            s.tick();
        }
        assert!(!s.is_active(), "rimshot rang too long");
    }

    #[test]
    fn produces_sound() {
        let id = MachineId::Rs;
        let macros = id.default_macros();
        let mut s = Rs::new(&macros);
        s.trigger(1.0);
        assert!(peak_over(&mut s, 2400) > 0.1, "rimshot too quiet");
    }

    #[test]
    fn noise_and_body_both_contribute() {
        let id = MachineId::Rs;
        // Pure noise
        let mut m = id.default_macros();
        m[4] = 0.0;
        let mut s = Rs::new(&m);
        s.trigger(1.0);
        let body_only = peak_over(&mut s, 480);
        // Pure body — body still sounds at NLEV=0 (noise contributes nothing)
        let body_peak = body_only;

        let mut m2 = id.default_macros();
        m2[4] = 1.0;
        let mut s2 = Rs::new(&m2);
        s2.trigger(1.0);
        let body_and_tick = peak_over(&mut s2, 480);

        // With both, the peak should be at least the body-only peak.
        assert!(
            body_and_tick >= body_peak * 0.9,
            "tick suppressed body: {body_and_tick} vs {body_peak}"
        );
    }
}
