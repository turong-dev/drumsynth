//! CY Metallic: ring-modulated metallic cymbal.
//!
//! Two oscillators whose product (ring modulation) produces the
//! inharmonic metallic partials that read as "cymbal" rather than "hat."
//! Highpassed noise adds the stick hit. Longer decay than a hat, with a
//! wash of sustained metallic shimmer.
//!
//! The Syntakt CY Metallic uses ring modulation between two oscillators;
//! we do the same. Two `sin_turns` lookups × one multiply = the metallic
//! core. Noise + HPF = the transient.
//!
//! # Macros
//!
//! | idx | name   | range         | notes |
//! |-----|--------|---------------|-------|
//! | 0   | TUNE   | 200..1200 Hz  | osc A frequency |
//! | 1   | TONE   | 0..1          | osc B/A ratio (1.3..3.0) |
//! | 2   | TDEC   | 5..100 ms     | transient (noise) decay |
//! | 3   | DEC    | 100..2000 ms  | main decay (long — it's a cymbal) |
//! | 4   | NCOL   | 1000..8000 Hz | noise HP colour |
//! | 5   | LEVEL  | 0..1          | per-machine output level |
//! | 6   | RESV   | (reserved)    | hit/wash balance (phase 2) |
//! | 7   | RESV2  | (reserved)    | osc reset (phase 2) |

use crate::dsp::filter::cutoff_coeff;
use crate::dsp::{decay_coeff, fast, DecayEnv, Noise, OnePoleHp, SineOsc};
use crate::machines::NUM_MACROS;
use crate::SAMPLE_RATE;

/// CY Metallic machine.
pub struct CyMetallic {
    osc_a: SineOsc,
    osc_b: SineOsc,
    env: DecayEnv,
    transient_env: DecayEnv,
    noise: Noise,
    noise_hp: OnePoleHp,
    /// Semitone multiplier applied to every frequency. Set by [`retune`],
    /// re-applied by `set_macros` so a later macro recompute keeps the note.
    freq_scale: f32,
    level: f32,
}

impl CyMetallic {
    /// Build the machine with the given macro values applied.
    pub fn new(macros: &[f32; NUM_MACROS]) -> Self {
        let mut m = Self {
            osc_a: SineOsc::new(),
            osc_b: SineOsc::new(),
            env: DecayEnv::new(0.0),
            transient_env: DecayEnv::new(0.0),
            noise: Noise::new(0xB19E_4D72),
            noise_hp: OnePoleHp::new(0.0),
            freq_scale: 1.0,
            level: 0.0,
        };
        m.set_macros(macros);
        m
    }

    /// Recompute coefficients from macros. Setup rate.
    pub fn set_macros(&mut self, macros: &[f32; NUM_MACROS]) {
        let osc_a_hz = 200.0 + 1000.0 * macros[0]; // TUNE 200..1200 Hz
        let ratio = 1.3 + 1.7 * macros[1]; // TONE 1.3..3.0
        let transient_decay_s = 0.005 + 0.095 * macros[2]; // TDEC 5..100 ms
        let main_decay_s = 0.1 + 1.9 * macros[3]; // DEC 100..2000 ms
        let noise_hp_hz = 1000.0 + 7000.0 * macros[4]; // NCOL 1..8 kHz
        let level = macros[5]; // LEVEL 0..1

        self.osc_a.set_freq(osc_a_hz * self.freq_scale);
        self.osc_b.set_freq(osc_a_hz * ratio * self.freq_scale);
        self.env.set_coeff(decay_coeff(main_decay_s, SAMPLE_RATE));
        self.transient_env
            .set_coeff(decay_coeff(transient_decay_s, SAMPLE_RATE));
        self.noise_hp
            .set_coeff(cutoff_coeff(noise_hp_hz, SAMPLE_RATE));
        self.level = level;
    }

    /// Transpose by `semis` semitones relative to the macro pitch.
    ///
    /// Scales both oscillators, preserving the ring-modulation ratio (the
    /// metallic character). The noise transient is pitchless and untouched.
    /// Absolute, not incremental.
    pub fn retune(&mut self, semis: f32) {
        let new_scale = fast::semitone_ratio(semis);
        let ratio = new_scale / self.freq_scale;
        self.freq_scale = new_scale;
        self.osc_a.set_freq(self.osc_a.freq() * ratio);
        self.osc_b.set_freq(self.osc_b.freq() * ratio);
    }

    /// Hit it.
    pub fn trigger(&mut self, velocity: f32) {
        self.env.trigger(velocity);
        self.transient_env.trigger(velocity);
        self.osc_a.reset_phase();
        self.osc_b.reset_phase();
    }

    /// Silence.
    pub fn reset(&mut self) {
        self.env.reset();
        self.transient_env.reset();
        self.noise_hp.reset();
    }

    /// Still sounding?
    pub fn is_active(&self) -> bool {
        self.env.is_active()
    }

    /// One sample.
    ///
    /// Ring modulation: `A × B` produces sum and difference frequencies —
    /// the inharmonic metallic shimmer. Plus HP noise for the stick transient.
    #[inline(always)]
    pub fn tick(&mut self) -> f32 {
        let amp = self.env.tick();
        if amp == 0.0 {
            return 0.0;
        }

        let transient = self.transient_env.tick();

        // Ring mod: the product of two sines.
        let metallic = self.osc_a.tick() * self.osc_b.tick();

        // Noise transient for the stick hit.
        let stick = if transient != 0.0 {
            self.noise_hp.tick(self.noise.tick()) * transient
        } else {
            0.0
        };

        let mixed = metallic * amp + stick * 0.5;
        fast::soft_clip(mixed) * self.level
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::machines::MachineId;

    fn peak_over(s: &mut CyMetallic, n: usize) -> f32 {
        let mut peak = 0.0f32;
        for _ in 0..n {
            peak = peak.max(libm::fabsf(s.tick()));
        }
        peak
    }

    #[test]
    fn silent_until_struck() {
        let id = MachineId::CyMetallic;
        let macros = id.default_macros();
        let mut s = CyMetallic::new(&macros);
        assert_eq!(peak_over(&mut s, 1000), 0.0);
    }

    #[test]
    fn produces_sound() {
        let id = MachineId::CyMetallic;
        let macros = id.default_macros();
        let mut s = CyMetallic::new(&macros);
        s.trigger(1.0);
        assert!(peak_over(&mut s, 2400) > 0.05, "cymbal too quiet");
    }

    #[test]
    fn decays_to_silence() {
        let id = MachineId::CyMetallic;
        let macros = id.default_macros();
        let mut s = CyMetallic::new(&macros);
        s.trigger(1.0);
        for _ in 0..(5.0 * SAMPLE_RATE) as usize {
            s.tick();
        }
        assert!(!s.is_active());
    }

    #[test]
    fn output_is_bounded() {
        let id = MachineId::CyMetallic;
        let macros = id.default_macros();
        let mut s = CyMetallic::new(&macros);
        for _ in 0..100 {
            s.trigger(1.0);
            for _ in 0..1024 {
                let v = s.tick();
                assert!(v.abs() <= 1.0, "cymbal escaped: {v}");
            }
        }
    }
}
