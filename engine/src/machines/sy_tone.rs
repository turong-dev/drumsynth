//! SY Tone: 2-operator FM tonal synth.
//!
//! The simplest tonal machine — a carrier/modulator FM pair with feedback
//! on the modulator, through the track strip's filter and amp envelope.
//! Unlike the drum machines, SY Tone is *tonal*: it tracks pitch via the
//! TUNE macro (which here maps to a full note range, not just a drum-pitch
//! rasp), and the strip's amp envelope defaults to ADSR (Phase 2 will
//! add proper note-off support; for now it's AHD like the drums).
//!
//! # Macros
//!
//! Canonical 4-bank layout (PITCH/FILTER/AMP/MOD), flat index `bank*8+slot`,
//! MIDI CC `20 + flat` on the track's channel:
//!
//! | idx | CC  | name      | range         | notes |
//! |-----|-----|-----------|---------------|-------|
//! | 0   | 20  | TUNE      | 55..1760 Hz   | carrier pitch (A1..A6 range) |
//! | 1   | 21  | RATIO     | 1.0..4.0      | modulator/carrier ratio |
//! | 2   | 22  | FDBK      | 0..1          | modulator feedback (adds complexity) |
//! | 5   | 25  | MACH      | 0..1          | machine selector (quantised over MachineId::ALL) |
//! | 16  | 36  | LEVEL     | 0..1          | per-machine output level |
//! | 18  | 38  | DEC       | 50..2000 ms   | amp decay |
//! | 22  | 42  | SEND.DLY  | 0..1          | delay send (track-routed) |
//! | 23  | 43  | SEND.RVB  | 0..1          | reverb send (track-routed) |
//! | 24  | 44  | MOD.AMT   | 0..3          | FM depth in carrier cycles |
//! | 25  | 45  | MENV      | 5..200 ms     | modulation envelope decay |
//!
//! All other slots are RESV (default 0.0) and ignored.

use crate::dsp::{decay_coeff, fast, DecayEnv, SineOsc};
use crate::machines::{
    NUM_MACROS, SLOT_DECAY, SLOT_LEVEL, SLOT_MOD_AMOUNT, SLOT_MOD_ENV, SLOT_SWEEP, SLOT_SWEEP_TIME,
    SLOT_TUNE,
};
use crate::SAMPLE_RATE;

/// SY Tone machine.
pub struct SyTone {
    carrier: SineOsc,
    modulator: SineOsc,
    amp_env: DecayEnv,
    mod_env: DecayEnv,
    mod_hz: f32,
    mod_amount: f32,
    feedback: f32,
    /// Last modulator output, used for feedback.
    mod_last: f32,
    /// Semitone multiplier applied to every frequency. Set by [`retune`],
    /// re-applied by `set_macros` so a later macro recompute keeps the note.
    freq_scale: f32,
    level: f32,
}

impl SyTone {
    /// Build the machine with the given macro values applied.
    pub fn new(macros: &[f32; NUM_MACROS]) -> Self {
        let mut m = Self {
            carrier: SineOsc::new(),
            modulator: SineOsc::new(),
            amp_env: DecayEnv::new(0.0),
            mod_env: DecayEnv::new(0.0),
            mod_hz: 0.0,
            mod_amount: 0.0,
            feedback: 0.0,
            mod_last: 0.0,
            freq_scale: 1.0,
            level: 0.0,
        };
        m.set_macros(macros);
        m
    }

    /// Recompute coefficients from macros. Setup rate.
    pub fn set_macros(&mut self, macros: &[f32; NUM_MACROS]) {
        // Exponential pitch mapping: 55 Hz (A1) to 1760 Hz (A6).
        // macro 0 = 0 → 55 Hz, 1 → 1760 Hz. Use exp2 over 5 octaves.
        let carrier_hz = 55.0 * crate::dsp::fast::exp2_approx(macros[SLOT_TUNE] * 5.0);
        let mod_ratio = 1.0 + 3.0 * macros[SLOT_SWEEP]; // RATIO 1..4
        let feedback = macros[SLOT_SWEEP_TIME]; // FDBK 0..1
        let mod_decay_s = 0.005 + 0.195 * macros[SLOT_MOD_ENV]; // MENV 5..200 ms
        let mod_amount = macros[SLOT_MOD_AMOUNT] * 3.0; // MOD.AMT 0..3
        let amp_decay_s = 0.05 + 1.95 * macros[SLOT_DECAY]; // DEC 50..2000 ms
        let level = macros[SLOT_LEVEL]; // LEVEL 0..1

        self.carrier.set_freq(carrier_hz * self.freq_scale);
        self.mod_hz = carrier_hz * mod_ratio * self.freq_scale;
        self.modulator.set_freq(self.mod_hz);
        self.mod_amount = mod_amount;
        self.feedback = feedback;
        self.amp_env
            .set_coeff(decay_coeff(amp_decay_s, SAMPLE_RATE));
        self.mod_env
            .set_coeff(decay_coeff(mod_decay_s, SAMPLE_RATE));
        self.level = level;
    }

    /// Transpose by `semis` semitones relative to the macro pitch.
    ///
    /// Scales carrier and modulator together so the FM ratio — and with it
    /// the timbre — survives the transpose. This is the machine that makes
    /// a channel-mapped track genuinely melodic. Absolute, not incremental.
    pub fn retune(&mut self, semis: f32) {
        let new_scale = fast::semitone_ratio(semis);
        let ratio = new_scale / self.freq_scale;
        self.freq_scale = new_scale;
        self.carrier.set_freq(self.carrier.freq() * ratio);
        self.mod_hz *= ratio;
        self.modulator.set_freq(self.mod_hz);
    }

    /// Begin a hit at `velocity` (0.0..=1.0).
    pub fn trigger(&mut self, velocity: f32) {
        self.amp_env.trigger(velocity);
        self.mod_env.trigger(1.0);
        self.carrier.reset_phase();
        self.modulator.reset_phase();
        self.mod_last = 0.0;
    }

    /// Silence.
    pub fn reset(&mut self) {
        self.amp_env.reset();
        self.mod_env.reset();
        self.carrier.reset_phase();
        self.modulator.reset_phase();
        self.mod_last = 0.0;
    }

    /// Still sounding?
    pub fn is_active(&self) -> bool {
        self.amp_env.is_active()
    }

    /// One sample.
    ///
    /// FM with modulator feedback: the modulator feeds its own previous
    /// output back into its phase, adding spectral complexity that
    /// plain 2-op FM lacks. This is the DX7-style trick that makes
    /// 2-operator FM sound richer than it has any right to.
    #[inline(always)]
    pub fn tick(&mut self) -> f32 {
        let amp = self.amp_env.tick();
        if amp == 0.0 {
            return 0.0;
        }

        let mod_env = self.mod_env.tick();

        // Modulator with feedback: phase bias = mod_signal + feedback × mod_last
        let mod_feedback = self.mod_last * self.feedback;
        let mod_signal = self.modulator.tick_with_phase_bias(mod_feedback);
        self.mod_last = mod_signal;

        // FM the carrier.
        let fm_amount = mod_signal * self.mod_amount * mod_env;
        let raw = self.carrier.tick_with_phase_bias(fm_amount) * amp;

        fast::soft_clip(raw) * self.level
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::machines::MachineId;

    #[test]
    fn silent_until_struck() {
        let id = MachineId::SyTone;
        let macros = id.default_macros();
        let mut s = SyTone::new(&macros);
        for _ in 0..1000 {
            assert_eq!(s.tick(), 0.0);
        }
    }

    #[test]
    fn produces_sound() {
        let id = MachineId::SyTone;
        let macros = id.default_macros();
        let mut s = SyTone::new(&macros);
        s.trigger(1.0);
        let mut peak = 0.0f32;
        for _ in 0..4800 {
            peak = peak.max(libm::fabsf(s.tick()));
        }
        assert!(peak > 0.05, "SY Tone too quiet");
    }

    #[test]
    fn velocity_scales_output() {
        let id = MachineId::SyTone;
        let macros = id.default_macros();
        let peak_at = |vel: f32| {
            let mut s = SyTone::new(&macros);
            s.trigger(vel);
            let mut peak = 0.0f32;
            for _ in 0..2400 {
                peak = peak.max(libm::fabsf(s.tick()));
            }
            peak
        };
        assert!(peak_at(1.0) > peak_at(0.25), "velocity had no effect");
    }

    #[test]
    fn decays_to_silence() {
        let id = MachineId::SyTone;
        let macros = id.default_macros();
        let mut s = SyTone::new(&macros);
        s.trigger(1.0);
        for _ in 0..(5.0 * SAMPLE_RATE) as usize {
            s.tick();
        }
        assert!(!s.is_active());
    }

    #[test]
    fn pitch_tracks_macro() {
        let id = MachineId::SyTone;
        let mut high = id.default_macros();
        high[0] = 1.0; // → ~1760 Hz
        let mut low = id.default_macros();
        low[0] = 0.0; // → 55 Hz

        // Sample the first 256 output values at each pitch. If pitch tracking
        // works, the two sequences will differ (different carrier frequency →
        // different waveform shape even through FM sidebands).
        let render = |macros: &[f32; NUM_MACROS]| -> [f32; 256] {
            let mut s = SyTone::new(macros);
            s.trigger(1.0);
            let mut buf = [0.0f32; 256];
            for i in 0..256 {
                buf[i] = s.tick();
            }
            buf
        };
        let low_buf = render(&low);
        let high_buf = render(&high);

        // Both should produce non-trivial output.
        let low_peak = low_buf.iter().fold(0.0f32, |a, &b| a.max(libm::fabsf(b)));
        let high_peak = high_buf.iter().fold(0.0f32, |a, &b| a.max(libm::fabsf(b)));
        assert!(low_peak > 0.01, "low pitch silent: {low_peak}");
        assert!(high_peak > 0.01, "high pitch silent: {high_peak}");

        // The waveforms should differ — same synthesis, different carrier
        // pitch → different sample sequence.
        let mut max_diff = 0.0f32;
        for i in 0..256 {
            max_diff = max_diff.max(libm::fabsf(low_buf[i] - high_buf[i]));
        }
        assert!(
            max_diff > 0.05,
            "pitch had no effect on output: max_diff={max_diff}"
        );
    }

    #[test]
    fn no_nans_from_feedback() {
        let id = MachineId::SyTone;
        let mut macros = id.default_macros();
        macros[SLOT_SWEEP_TIME] = 1.0; // max feedback
        macros[SLOT_MOD_AMOUNT] = 1.0; // max FM amount
        let mut s = SyTone::new(&macros);
        s.trigger(1.0);
        for _ in 0..(2.0 * SAMPLE_RATE) as usize {
            let v = s.tick();
            assert!(v.is_finite(), "feedback produced NaN: {v}");
        }
    }

    #[test]
    fn retune_transposes_an_octave() {
        let id = MachineId::SyTone;
        let macros = id.default_macros();
        let window = (0.03 * SAMPLE_RATE) as usize;
        let crossings = |semis: f32| {
            let mut s = SyTone::new(&macros);
            s.retune(semis);
            s.trigger(1.0);
            let mut prev = s.tick();
            let mut c = 0u32;
            for _ in 1..window {
                let v = s.tick();
                if (prev < 0.0) != (v < 0.0) {
                    c += 1;
                }
                prev = v;
            }
            c
        };
        let base = crossings(0.0);
        let octave = crossings(12.0);
        // +12 semitones = 2x frequency ≈ 2x zero crossings. FM sidebands make
        // it approximate; demand a clear gap, not exact doubling.
        assert!(
            octave > base * 3 / 2,
            "octave-up should cross clearly more: {base} vs {octave}"
        );
        assert!(
            octave < base * 4,
            "crossing counts implausible: {base} vs {octave}"
        );
    }
}
