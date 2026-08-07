//! SD FM: FM snare plus filtered noise.
//!
//! The FM sibling of SD Natural. The body is a 2-operator FM tone — punchier
//! and more "electronic" than the dual-sine shell — paired with the same
//! highpassed-noise rattle. Costs two `sin_turns` lookups plus one noise
//! tick; cheaper than the Syntakt analog SD FM in absolute terms and
//! indistinguishable in character for one-shot drum use.
//!
//! # Macros
//!
//! Canonical 4-bank layout (PITCH/FILTER/AMP/MOD), flat index `bank*8+slot`,
//! MIDI CC `20 + flat` on the track's channel:
//!
//! | idx | CC  | name      | range         | notes |
//! |-----|-----|-----------|---------------|-------|
//! | 0   | 20  | TUNE      | 100..400 Hz   | carrier pitch |
//! | 1   | 21  | RAT       | 1.0..4.0      | modulator-to-carrier ratio |
//! | 5   | 25  | MACH      | 0..1          | machine selector (quantised over MachineId::ALL) |
//! | 16  | 36  | BDEC      | 40..480 ms    | body decay |
//! | 17  | 37  | NDEC      | 30..830 ms    | noise decay |
//! | 18  | 38  | LEVEL     | 0..1          | per-machine output level |
//! | 20  | 40  | NMIX      | 0..1          | body↔rattle crossfade |
//! | 21  | 41  | SEND.DLY  | 0..1          | delay send (track-routed) |
//! | 22  | 42  | SEND.RVB  | 0..1          | reverb send (track-routed) |
//! | 24  | 44  | AMT       | 0..3          | FM amount in carrier cycles |
//! | 25  | 45  | MENV      | 5..105 ms     | mod-envelope decay (FM thins over time) |
//!
//! All other slots are RESV (default 0.0) and ignored.

use crate::dsp::filter::cutoff_coeff;
use crate::dsp::{decay_coeff, fast, DecayEnv, Noise, OnePoleHp, SineOsc};
use crate::machines::{
    NUM_MACROS, SLOT_DECAY, SLOT_DECAY_2, SLOT_LEVEL, SLOT_MIX, SLOT_MOD_AMOUNT, SLOT_MOD_ENV,
    SLOT_SWEEP, SLOT_TUNE,
};
use crate::SAMPLE_RATE;

/// SD FM machine.
pub struct SdFm {
    carrier: SineOsc,
    modulator: SineOsc,
    body_env: DecayEnv,
    mod_env: DecayEnv,
    noise: Noise,
    noise_env: DecayEnv,
    hp: OnePoleHp,
    mod_hz: f32,
    mod_amount: f32,
    body_gain: f32,
    noise_gain: f32,
    /// Semitone multiplier applied to every frequency. Set by [`retune`],
    /// re-applied by `set_macros` so a later macro recompute keeps the note.
    freq_scale: f32,
    level: f32,
}

impl SdFm {
    /// Build the machine with the given macro values applied.
    pub fn new(macros: &[f32; NUM_MACROS]) -> Self {
        let mut s = Self {
            carrier: SineOsc::new(),
            modulator: SineOsc::new(),
            body_env: DecayEnv::new(0.0),
            mod_env: DecayEnv::new(0.0),
            noise: Noise::new(0x7C3E_5A18),
            noise_env: DecayEnv::new(0.0),
            hp: OnePoleHp::new(0.0),
            mod_hz: 0.0,
            mod_amount: 0.0,
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
        let carrier_hz = 100.0 + 300.0 * macros[SLOT_TUNE]; // TUNE 100..400 Hz
        let mod_ratio = 1.0 + 3.0 * macros[SLOT_SWEEP]; // RAT 1..4
        let body_decay_s = 0.04 + 0.44 * macros[SLOT_DECAY]; // BDEC 40..480 ms
        let noise_decay_s = 0.03 + 0.8 * macros[SLOT_DECAY_2]; // NDEC 30..830 ms
        let mod_decay_s = 0.005 + 0.1 * macros[SLOT_MOD_ENV]; // MENV 5..105 ms
        let mod_amount = macros[SLOT_MOD_AMOUNT] * 3.0; // AMT 0..3
        let noise_mix = macros[SLOT_MIX].clamp(0.0, 1.0); // NMIX 0..1
        let level = macros[SLOT_LEVEL]; // LEVEL 0..1

        self.carrier.set_freq(carrier_hz * self.freq_scale);
        self.mod_hz = carrier_hz * mod_ratio * self.freq_scale;
        self.modulator.set_freq(self.mod_hz);
        self.mod_amount = mod_amount;

        self.body_env
            .set_coeff(decay_coeff(body_decay_s, SAMPLE_RATE));
        self.mod_env
            .set_coeff(decay_coeff(mod_decay_s, SAMPLE_RATE));
        self.noise_env
            .set_coeff(decay_coeff(noise_decay_s, SAMPLE_RATE));
        self.hp.set_coeff(cutoff_coeff(1500.0, SAMPLE_RATE));

        self.body_gain = 1.0 - noise_mix;
        self.noise_gain = noise_mix;
        self.level = level;
    }

    /// Transpose by `semis` semitones relative to the macro pitch.
    ///
    /// Scales carrier and modulator together so the FM ratio — and with it
    /// the snare's timbre — survives the transpose. The noise rattle is
    /// pitchless and untouched. Absolute, not incremental.
    pub fn retune(&mut self, semis: f32) {
        let new_scale = fast::semitone_ratio(semis);
        let ratio = new_scale / self.freq_scale;
        self.freq_scale = new_scale;
        self.carrier.set_freq(self.carrier.freq() * ratio);
        self.mod_hz *= ratio;
        self.modulator.set_freq(self.mod_hz);
    }

    /// Hit it.
    pub fn trigger(&mut self, velocity: f32) {
        self.body_env.trigger(velocity);
        self.mod_env.trigger(1.0);
        self.noise_env.trigger(velocity);
        self.carrier.reset_phase();
        self.modulator.reset_phase();
    }

    /// Silence.
    pub fn reset(&mut self) {
        self.body_env.reset();
        self.mod_env.reset();
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

        let body = if body_amp != 0.0 {
            let mod_env = self.mod_env.tick();
            let mod_signal = self.modulator.tick() * self.mod_amount * mod_env;
            self.carrier.tick_with_phase_bias(mod_signal) * body_amp
        } else {
            0.0
        };

        let rattle = if noise_amp != 0.0 {
            self.hp.tick(self.noise.tick()) * noise_amp
        } else {
            0.0
        };

        let mixed = body * self.body_gain + rattle * self.noise_gain;
        fast::soft_clip(mixed) * self.level
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::machines::MachineId;

    fn peak_over(s: &mut SdFm, n: usize) -> f32 {
        let mut peak = 0.0f32;
        for _ in 0..n {
            peak = peak.max(libm::fabsf(s.tick()));
        }
        peak
    }

    #[test]
    fn silent_until_struck() {
        let id = MachineId::SdFm;
        let macros = id.default_macros();
        let mut s = SdFm::new(&macros);
        assert_eq!(peak_over(&mut s, 1000), 0.0);
    }

    #[test]
    fn full_noise_mix_still_makes_sound() {
        let id = MachineId::SdFm;
        let mut m = id.default_macros();
        m[SLOT_MIX] = 1.0;
        let mut s = SdFm::new(&m);
        s.trigger(1.0);
        assert!(peak_over(&mut s, 4800) > 0.05);
    }

    #[test]
    fn zero_noise_mix_still_makes_sound() {
        let id = MachineId::SdFm;
        let mut m = id.default_macros();
        m[SLOT_MIX] = 0.0;
        let mut s = SdFm::new(&m);
        s.trigger(1.0);
        assert!(peak_over(&mut s, 4800) > 0.05);
    }

    #[test]
    fn fm_amount_changes_tone() {
        let id = MachineId::SdFm;
        let mut clean = id.default_macros();
        clean[SLOT_MOD_AMOUNT] = 0.0;
        clean[SLOT_MIX] = 0.0;
        let mut fm = id.default_macros();
        fm[SLOT_MOD_AMOUNT] = 1.0;
        fm[SLOT_MIX] = 0.0;
        let crossings = |macros: &[f32; NUM_MACROS]| {
            let mut s = SdFm::new(macros);
            s.trigger(1.0);
            let n = (0.02 * SAMPLE_RATE) as usize;
            let mut prev = s.tick();
            let mut c = 0;
            for _ in 1..n {
                let v = s.tick();
                if (prev < 0.0) != (v < 0.0) {
                    c += 1;
                }
                prev = v;
            }
            c
        };
        let clean_c = crossings(&clean);
        let fm_c = crossings(&fm);
        assert!(
            fm_c > clean_c,
            "FM should add spectral content: clean={clean_c}, fm={fm_c}"
        );
    }

    #[test]
    fn decays_to_silence() {
        let id = MachineId::SdFm;
        let macros = id.default_macros();
        let mut s = SdFm::new(&macros);
        s.trigger(1.0);
        for _ in 0..(5.0 * SAMPLE_RATE) as usize {
            s.tick();
        }
        assert!(!s.is_active());
    }
}
