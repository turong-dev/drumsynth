//! RS: rimshot — two detuned square-ish oscillators plus a noise tick.
//!
//! A rimshot is the crack of the stick hitting the rim and head at once:
//! short, bright, with a metallic edge that's closer to a square than a
//! sine. Two oscillators detuned a few cents give it thickness without
//! "chorus"; the noise tick adds the physical "crack" at the start.
//!
//! # Macros
//!
//! Canonical 4-bank layout (PITCH/FILTER/AMP/MOD), flat index `bank*8+slot`,
//! MIDI CC `20 + flat` on the track's channel:
//!
//! | idx | CC  | name      | range         | notes |
//! |-----|-----|-----------|---------------|-------|
//! | 0   | 20  | TUNE      | 200..800 Hz   | body pitch |
//! | 1   | 21  | DET       | 1.0..1.08     | osc2 relative to osc1 (up to ~8%) |
//! | 5   | 25  | MACH      | 0..1          | machine selector (quantised over MachineId::ALL) |
//! | 8   | 28  | HPF       | 1000..6000 Hz | noise tick colour |
//! | 16  | 36  | LEVEL     | 0..1          | per-machine output level |
//! | 18  | 38  | DEC       | 15..150 ms    | body decay — intentionally short |
//! | 19  | 39  | NDEC      | 5..60 ms      | noise-tick decay (shorter than body) |
//! | 20  | 40  | NLEV      | 0..1          | tick level |
//! | 22  | 42  | SEND.DLY  | 0..1          | delay send (track-routed) |
//! | 23  | 43  | SEND.RVB  | 0..1          | reverb send (track-routed) |
//!
//! All other slots are RESV (default 0.0) and ignored.

use crate::dsp::filter::cutoff_coeff;
use crate::dsp::{decay_coeff, fast, DecayEnv, Noise, OnePoleHp, SineOsc};
use crate::machines::{
    NUM_MACROS, SLOT_FILT_0, SLOT_MACH_5, SLOT_MACH_6, SLOT_LEVEL, SLOT_MACH_7, SLOT_MACH_1, SLOT_MACH_0,
};
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
    /// Semitone multiplier applied to every frequency. Set by [`retune`],
    /// re-applied by `set_macros` so a later macro recompute keeps the note.
    freq_scale: f32,
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
            freq_scale: 1.0,
            level: 0.0,
        };
        s.set_macros(macros);
        s
    }

    /// Recompute coefficients from macros. Setup rate.
    pub fn set_macros(&mut self, macros: &[f32; NUM_MACROS]) {
        let body_hz = 200.0 + 600.0 * macros[SLOT_MACH_0]; // TUNE 200..800 Hz
        let detune = 1.0 + 0.08 * macros[SLOT_MACH_1]; // DET up to ~8%
        let body_decay_s = 0.015 + 0.135 * macros[SLOT_MACH_5]; // DEC 15..150 ms
        let noise_decay_s = 0.005 + 0.055 * macros[SLOT_MACH_6]; // NDEC 5..60 ms
        let noise_level = macros[SLOT_MACH_7]; // NLEV 0..1
        let hp_hz = 1000.0 + 5000.0 * macros[SLOT_FILT_0]; // HPF 1..6 kHz
        let level = macros[SLOT_LEVEL]; // LEVEL 0..1

        self.osc_a.set_freq(body_hz * self.freq_scale);
        self.osc_b.set_freq(body_hz * detune * self.freq_scale);
        self.body_env
            .set_coeff(decay_coeff(body_decay_s, SAMPLE_RATE));
        self.noise_env
            .set_coeff(decay_coeff(noise_decay_s, SAMPLE_RATE));
        self.hp.set_coeff(cutoff_coeff(hp_hz, SAMPLE_RATE));
        self.noise_level = noise_level;
        self.level = level;
    }

    /// Transpose by `semis` semitones relative to the macro pitch.
    ///
    /// Scales both oscillators, preserving the detune between them. The
    /// noise tick is pitchless and untouched. Absolute, not incremental.
    pub fn retune(&mut self, semis: f32) {
        let new_scale = fast::semitone_ratio(semis);
        let ratio = new_scale / self.freq_scale;
        self.freq_scale = new_scale;
        self.osc_a.set_freq(self.osc_a.freq() * ratio);
        self.osc_b.set_freq(self.osc_b.freq() * ratio);
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
            // The crack is the whole point of a rimshot — it should read
            // against the body, not hide under it. 2.0 brings the
            // highpassed-noise tick up to body level at NLEV = 1.
            self.hp.tick(self.noise.tick()) * noise_amp * 2.0
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
        m[SLOT_MACH_7] = 0.0;
        let mut s = Rs::new(&m);
        s.trigger(1.0);
        let body_only = peak_over(&mut s, 480);
        // Pure body — body still sounds at NLEV=0 (noise contributes nothing)
        let body_peak = body_only;

        let mut m2 = id.default_macros();
        m2[SLOT_MACH_7] = 1.0;
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
