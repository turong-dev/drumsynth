//! BD FM: 2-operator FM kick.
//!
//! A different kick character from BD Classic — instead of a pitch-swept
//! sine through a saturator, an FM modulator (osc B) drives the carrier
//! (osc A) with its own decay, giving a punchier, more "electronic" attack
//! without needing a soft-clipper. The Syntakt BD FM has separate mod-osc
//! pitch, decay, and amount controls; we surface the most musically
//! important three of those as macros.
//!
//! # Topology
//!
//! ```text
//!   pitch env ──► carrier freq
//!   mod env    ──► mod amount  (decays to zero, so FM thins over time)
//!   mod osc    ──► phase_mod(carrier) → sine lookup
//! ```
//!
//! Per-sample cost: two `sin_turns` lookups (carrier + modulator) — still
//! cheap thanks to the table swap.

use crate::dsp::{decay_coeff, fast, DecayEnv, SineOsc};
use crate::machines::NUM_MACROS;
use crate::SAMPLE_RATE;

/// BD FM machine.
pub struct BdFm {
    carrier: SineOsc,
    modulator: SineOsc,
    amp_env: DecayEnv,
    pitch_env: DecayEnv,
    mod_env: DecayEnv,
    // Cached from macros so `tick` never reads the macro array.
    start_hz: f32,
    end_hz: f32,
    sweep_range: f32,
    mod_hz: f32,
    mod_amount: f32,
    level: f32,
}

impl BdFm {
    /// Build the machine with the given macro values applied.
    pub fn new(macros: &[f32; NUM_MACROS]) -> Self {
        let mut m = Self {
            carrier: SineOsc::new(),
            modulator: SineOsc::new(),
            amp_env: DecayEnv::new(0.0),
            pitch_env: DecayEnv::new(0.0),
            mod_env: DecayEnv::new(0.0),
            start_hz: 0.0,
            end_hz: 0.0,
            sweep_range: 0.0,
            mod_hz: 0.0,
            mod_amount: 0.0,
            level: 0.0,
        };
        m.set_macros(macros);
        m
    }

    /// Recompute coefficients from macros. Setup rate.
    pub fn set_macros(&mut self, macros: &[f32; NUM_MACROS]) {
        let end_hz = 30.0 + 90.0 * macros[0]; // TUNE 30..120 Hz
        let pitch_ratio = 1.0 + 8.0 * macros[1]; // SWEEP 1×..9×
        let pitch_decay_s = 0.005 + 0.1 * macros[2]; // SWP_T 5..105 ms
        let amp_decay_s = 0.05 + 1.45 * macros[3]; // DEC 50..1500 ms
        let mod_ratio = 1.0 + 7.0 * macros[4]; // MOD.HZ 1×..8× relative
        let mod_decay_s = 0.005 + 0.1 * macros[5]; // MOD.DEC 5..105 ms
        let mod_amount = macros[6] * 4.0; // MOD.AMT 0..4 (in carrier cycles)
        let level = macros[7]; // LEVEL 0..1

        self.start_hz = end_hz * pitch_ratio;
        self.end_hz = end_hz;
        self.sweep_range = self.start_hz - self.end_hz;
        self.mod_hz = end_hz * mod_ratio;
        self.mod_amount = mod_amount;
        self.level = level;

        self.amp_env
            .set_coeff(decay_coeff(amp_decay_s, SAMPLE_RATE));
        self.pitch_env
            .set_coeff(decay_coeff(pitch_decay_s, SAMPLE_RATE));
        self.mod_env
            .set_coeff(decay_coeff(mod_decay_s, SAMPLE_RATE));
    }

    /// Begin a hit at `velocity` (0.0..=1.0).
    pub fn trigger(&mut self, velocity: f32) {
        self.amp_env.trigger(velocity);
        self.pitch_env.trigger(1.0);
        self.mod_env.trigger(1.0);
        self.carrier.reset_phase();
        self.modulator.reset_phase();
    }

    /// Silence.
    pub fn reset(&mut self) {
        self.amp_env.reset();
        self.pitch_env.reset();
        self.mod_env.reset();
        self.carrier.reset_phase();
        self.modulator.reset_phase();
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
            return 0.0;
        }

        let sweep = self.pitch_env.tick();
        let carrier_hz = self.end_hz + self.sweep_range * sweep;
        self.carrier.set_freq(carrier_hz);
        self.modulator.set_freq(self.mod_hz);

        // FM: phase of the carrier is perturbed by modulator × mod_amount ×
        // mod_env. Done by *biasing* the carrier phase directly rather than
        // recomputing the sine, since `sin_turns` takes a normalised phase.
        // We reach into the SineOsc's phase via `tick_with_phase_bias`.
        let mod_env = self.mod_env.tick();
        let mod_signal = self.modulator.tick() * self.mod_amount * mod_env;

        let raw = self.carrier.tick_with_phase_bias(mod_signal) * amp;
        fast::soft_clip(raw) * self.level
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::machines::MachineId;

    #[test]
    fn silent_until_struck() {
        let id = MachineId::BdFm;
        let macros = id.default_macros();
        let mut k = BdFm::new(&macros);
        for _ in 0..1000 {
            assert_eq!(k.tick(), 0.0);
        }
    }

    #[test]
    fn velocity_scales_output() {
        let id = MachineId::BdFm;
        let macros = id.default_macros();
        let peak_at = |vel: f32| {
            let mut k = BdFm::new(&macros);
            k.trigger(vel);
            let mut peak = 0.0f32;
            for _ in 0..4800 {
                peak = peak.max(libm::fabsf(k.tick()));
            }
            peak
        };
        assert!(peak_at(1.0) > peak_at(0.25), "velocity had no effect");
    }

    #[test]
    fn mod_amount_changes_tone() {
        let id = MachineId::BdFm;
        let mut quiet = id.default_macros();
        quiet[6] = 0.0; // MOD.AMT = 0 → plain sine (no FM)
        let mut loud = id.default_macros();
        loud[6] = 1.0; // MOD.AMT = 1 → max FM

        // FM adds sidebands at higher frequencies, so the heavier-FM case
        // should have more zero crossings per unit time than the clean sine.
        let crossings = |macros: &[f32; NUM_MACROS]| {
            let mut k = BdFm::new(macros);
            k.trigger(1.0);
            let n = (0.02 * SAMPLE_RATE) as usize; // 20ms window
            let mut prev = k.tick();
            let mut c = 0;
            for _ in 1..n {
                let s = k.tick();
                if (prev < 0.0) != (s < 0.0) {
                    c += 1;
                }
                prev = s;
            }
            c
        };
        let clean = crossings(&quiet);
        let fm = crossings(&loud);
        assert!(
            fm > clean,
            "FM should add spectral content (more crossings): clean={clean}, fm={fm}"
        );
    }

    #[test]
    fn decays_to_silence() {
        let id = MachineId::BdFm;
        let macros = id.default_macros();
        let mut k = BdFm::new(&macros);
        k.trigger(1.0);
        for _ in 0..(5.0 * SAMPLE_RATE) as usize {
            k.tick();
        }
        assert!(!k.is_active());
    }
}
