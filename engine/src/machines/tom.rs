//! Tom: pitch-swept sine with a short stick transient, no drive.
//!
//! Distinct from BD Classic in three ways that make it read as a tom rather
//! than a kick: a higher fundamental range, a longer pitch-sweep time with a
//! sustained body after landing, and an optional initial "stick" click
//! layer that's noise-through-highpass rather than saturation. No drive —
//! the body is clean, so the strip filter is where tone shaping lives.
//!
//! # Macros
//!
//! Canonical 4-bank layout (PITCH/FILTER/AMP/MOD), flat index `bank*8+slot`,
//! MIDI CC `20 + flat` on the track's channel:
//!
//! | idx | CC  | name      | range         | notes |
//! |-----|-----|-----------|---------------|-------|
//! | 0   | 20  | TUNE      | 45..220 Hz    | rack-to-floor tom fundamental |
//! | 1   | 21  | SWEEP     | 1×..5×        | start-to-end pitch ratio |
//! | 2   | 22  | SWP_T     | 20..200 ms    | pitch-sweep decay (slower than kick) |
//! | 5   | 25  | MACH      | 0..1          | machine selector (quantised over MachineId::ALL) |
//! | 16  | 36  | LEVEL     | 0..1          | per-machine output level |
//! | 18  | 38  | DEC       | 100..900 ms   | amp decay (rings longer than a kick) |
//! | 20  | 40  | STICK     | 0..1          | initial noise-click amount |
//! | 22  | 42  | SEND.DLY  | 0..1          | delay send (track-routed) |
//! | 23  | 43  | SEND.RVB  | 0..1          | reverb send (track-routed) |
//!
//! All other slots are RESV (default 0.0) and ignored.

use crate::dsp::filter::cutoff_coeff;
use crate::dsp::{decay_coeff, DecayEnv, Noise, OnePoleHp, SineOsc};
use crate::machines::{
    NUM_MACROS, SLOT_MACH_5, SLOT_LEVEL, SLOT_MACH_7, SLOT_MACH_1, SLOT_MACH_2, SLOT_MACH_0,
};
use crate::SAMPLE_RATE;

/// Tom machine.
pub struct Tom {
    osc: SineOsc,
    amp_env: DecayEnv,
    pitch_env: DecayEnv,
    noise: Noise,
    stick_env: DecayEnv,
    hp: OnePoleHp,
    start_hz: f32,
    end_hz: f32,
    sweep_range: f32,
    /// Semitone multiplier applied to every frequency. Set by [`retune`],
    /// re-applied by `set_macros` so a later macro recompute keeps the note.
    freq_scale: f32,
    stick_amount: f32,
    level: f32,
}

impl Tom {
    /// Build the machine with the given macro values applied.
    pub fn new(macros: &[f32; NUM_MACROS]) -> Self {
        let mut m = Self {
            osc: SineOsc::new(),
            amp_env: DecayEnv::new(0.0),
            pitch_env: DecayEnv::new(0.0),
            noise: Noise::new(0x6D2B_79F2),
            stick_env: DecayEnv::new(0.0),
            hp: OnePoleHp::new(0.0),
            start_hz: 0.0,
            end_hz: 0.0,
            sweep_range: 0.0,
            freq_scale: 1.0,
            stick_amount: 0.0,
            level: 0.0,
        };
        m.set_macros(macros);
        m
    }

    /// Recompute coefficients from macros. Setup rate.
    pub fn set_macros(&mut self, macros: &[f32; NUM_MACROS]) {
        let end_hz = 45.0 + 175.0 * macros[SLOT_MACH_0]; // TUNE 45..220 Hz
        let pitch_ratio = 1.0 + 4.0 * macros[SLOT_MACH_1]; // SWEEP 1×..5×
        let pitch_decay_s = 0.02 + 0.18 * macros[SLOT_MACH_2]; // SWP_T 20..200 ms
        let amp_decay_s = 0.1 + 0.8 * macros[SLOT_MACH_5]; // DEC 100..900 ms
        let stick_amount = macros[SLOT_MACH_7]; // STICK 0..1
        let level = macros[SLOT_LEVEL]; // LEVEL 0..1

        self.start_hz = end_hz * pitch_ratio * self.freq_scale;
        self.end_hz = end_hz * self.freq_scale;
        self.sweep_range = self.start_hz - self.end_hz;
        self.stick_amount = stick_amount;
        self.level = level;

        self.amp_env
            .set_coeff(decay_coeff(amp_decay_s, SAMPLE_RATE));
        self.pitch_env
            .set_coeff(decay_coeff(pitch_decay_s, SAMPLE_RATE));
        // The stick click is short — 5..50 ms — and highpassed so it
        // reads as a stick slap rather than a cymbal tick. The decay tracks
        // the STICK amount, so a higher knob both raises the click and
        // lengthens it.
        self.stick_env
            .set_coeff(decay_coeff(0.005 + 0.045 * stick_amount, SAMPLE_RATE));
        self.hp.set_coeff(cutoff_coeff(3000.0, SAMPLE_RATE));
    }

    /// Transpose by `semis` semitones relative to the macro pitch.
    ///
    /// Scales both sweep endpoints so the whole pitch drop moves with the
    /// note. The stick click is pitchless noise and is untouched. Absolute,
    /// not incremental.
    pub fn retune(&mut self, semis: f32) {
        let new_scale = crate::dsp::fast::semitone_ratio(semis);
        let ratio = new_scale / self.freq_scale;
        self.freq_scale = new_scale;
        self.start_hz *= ratio;
        self.end_hz *= ratio;
        self.sweep_range = self.start_hz - self.end_hz;
    }

    /// Begin a hit at `velocity` (0.0..=1.0).
    pub fn trigger(&mut self, velocity: f32) {
        self.amp_env.trigger(velocity);
        self.pitch_env.trigger(1.0);
        self.stick_env.trigger(velocity);
        self.osc.reset_phase();
    }

    /// Silence.
    pub fn reset(&mut self) {
        self.amp_env.reset();
        self.pitch_env.reset();
        self.stick_env.reset();
        self.hp.reset();
        self.osc.reset_phase();
    }

    /// Still sounding?
    pub fn is_active(&self) -> bool {
        self.amp_env.is_active()
    }

    /// One sample.
    #[inline(always)]
    pub fn tick(&mut self) -> f32 {
        let amp = self.amp_env.tick();
        if amp == 0.0 {
            // Stick env is shorter; if amp is gone, nothing left.
            return 0.0;
        }

        let sweep = self.pitch_env.tick();
        self.osc.set_freq(self.end_hz + self.sweep_range * sweep);

        let body = self.osc.tick() * amp;

        // Stick click: highpassed noise with its own short env. Gates off the
        // STICK macro implicitly via the env decay time (more stick = longer
        // click) and explicitly via the level gain below.
        let stick_amp = self.stick_env.tick();
        let stick = if stick_amp != 0.0 {
            self.hp.tick(self.noise.tick()) * stick_amp
        } else {
            0.0
        };

        // Crossfade: stick_amount controls how present the click is; body is
        // always the dominant element. No drive — the body stays clean.
        let mixed = body + stick * self.stick_amount * 1.25;
        mixed * self.level
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::machines::MachineId;

    #[test]
    fn silent_until_struck() {
        let macros = MachineId::ALL
            .iter()
            .find(|m| **m == MachineId::Tom)
            .unwrap()
            .default_macros();
        let mut t = Tom::new(&macros);
        for _ in 0..1000 {
            assert_eq!(t.tick(), 0.0);
        }
    }

    #[test]
    fn velocity_scales_output() {
        let id = MachineId::Tom;
        let macros = id.default_macros();
        let peak_at = |vel: f32| {
            let mut t = Tom::new(&macros);
            t.trigger(vel);
            let mut peak = 0.0f32;
            for _ in 0..4800 {
                peak = peak.max(libm::fabsf(t.tick()));
            }
            peak
        };
        assert!(peak_at(1.0) > peak_at(0.25), "velocity had no effect");
    }

    #[test]
    fn pitch_falls_over_time() {
        let id = MachineId::Tom;
        let macros = id.default_macros();
        let mut t = Tom::new(&macros);
        t.trigger(1.0);
        let window = (0.01 * SAMPLE_RATE) as usize;
        let count_crossings = |t: &mut Tom, n: usize| {
            let mut prev = t.tick();
            let mut c = 0;
            for _ in 1..n {
                let s = t.tick();
                if (prev < 0.0) != (s < 0.0) {
                    c += 1;
                }
                prev = s;
            }
            c
        };
        let early = count_crossings(&mut t, window);
        for _ in 0..window * 4 {
            t.tick();
        }
        let late = count_crossings(&mut t, window);
        assert!(early > late, "pitch did not fall: {early} then {late}");
    }

    #[test]
    fn decays_to_silence() {
        let id = MachineId::Tom;
        let macros = id.default_macros();
        let mut t = Tom::new(&macros);
        t.trigger(1.0);
        for _ in 0..(5.0 * SAMPLE_RATE) as usize {
            t.tick();
        }
        assert!(!t.is_active());
    }
}
