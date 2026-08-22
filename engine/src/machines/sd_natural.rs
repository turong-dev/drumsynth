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
//! Canonical 4-bank layout (PITCH/FILTER/AMP/MOD), flat index `bank*8+slot`,
//! MIDI CC `20 + flat` on the track's channel:
//!
//! | idx | CC  | name      | range         | notes |
//! |-----|-----|-----------|---------------|-------|
//! | 0   | 20  | TUNE      | 100..400 Hz   | shell fundamental |
//! | 1   | 21  | RATIO     | 1.0..2.0      | second tone relative to first |
//! | 5   | 25  | MACH      | 0..1          | machine selector (quantised over MachineId::ALL) |
//! | 8   | 28  | HPF       | 400..4000 Hz  | noise highpass |
//! | 16  | 36  | LEVEL     | 0..1          | per-machine output level |
//! | 18  | 38  | BDEC      | 40..640 ms    | body decay |
//! | 19  | 39  | NDEC      | 30..830 ms    | noise decay |
//! | 21  | 41  | NMIX      | 0..1          | body↔rattle crossfade |
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
    /// Semitone multiplier applied to every frequency. Set by [`retune`],
    /// re-applied by `set_macros` so a later macro recompute keeps the note.
    freq_scale: f32,
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
            freq_scale: 1.0,
            level: 0.0,
        };
        s.set_macros(macros);
        s
    }

    /// Recompute coefficients from macros. Setup rate.
    pub fn set_macros(&mut self, macros: &[f32; NUM_MACROS]) {
        let body_hz = 100.0 + 300.0 * macros[SLOT_MACH_0]; // TUNE 100..400 Hz
        let body_ratio = 1.0 + macros[SLOT_MACH_1]; // RATIO 1.0..2.0
        let body_decay_s = 0.04 + 0.6 * macros[SLOT_MACH_5]; // BDEC 40..640 ms
        let noise_decay_s = 0.03 + 0.8 * macros[SLOT_MACH_6]; // NDEC 30..830 ms
        let noise_hp_hz = 400.0 + 3600.0 * macros[SLOT_FILT_0]; // HPF 400..4000 Hz
        let noise_mix = macros[SLOT_MACH_7]; // NMIX 0..1
        let level = macros[SLOT_LEVEL]; // LEVEL 0..1

        self.body_a.set_freq(body_hz * self.freq_scale);
        self.body_b.set_freq(body_hz * body_ratio * self.freq_scale);
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

    /// Transpose by `semis` semitones relative to the macro pitch.
    ///
    /// Scales both shell tones, preserving the fixed ratio between them.
    /// The noise rattle is pitchless and untouched. Absolute, not
    /// incremental.
    pub fn retune(&mut self, semis: f32) {
        let new_scale = fast::semitone_ratio(semis);
        let ratio = new_scale / self.freq_scale;
        self.freq_scale = new_scale;
        self.body_a.set_freq(self.body_a.freq() * ratio);
        self.body_b.set_freq(self.body_b.freq() * ratio);
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
        macros[SLOT_MACH_7] = 1.0; // NMIX
        let mut s = SdNatural::new(&macros);
        s.trigger(1.0);
        assert!(peak_over(&mut s, 4800) > 0.05);
    }

    #[test]
    fn zero_noise_mix_still_makes_sound() {
        let mut macros = MachineId::SdNatural.default_macros();
        macros[SLOT_MACH_7] = 0.0; // NMIX
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
