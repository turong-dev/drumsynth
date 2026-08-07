//! BD Classic: pitch-swept sine with drive — the classic analogue-style body.
//!
//! A sine whose frequency drops rapidly from a click-like starting pitch down
//! to a steady fundamental, multiplied by an amplitude envelope, then driven
//! into saturation for weight.
//!
//! Almost all of the character lives in the relationship between the two
//! envelope times: a pitch decay much shorter than the amp decay gives a
//! tight, clicky attack over a sustained body; bring them closer together
//! and it turns into a tom.
//!
//! # Macros
//!
//! | idx | name   | range        | notes |
//! |-----|--------|--------------|-------|
//! | 0   | TUNE   | 30..120 Hz   | settled fundamental |
//! | 1   | SWEEP  | 1×..11×      | start-to-end pitch ratio |
//! | 2   | SWP_T  | 5..155 ms    | pitch-sweep decay time |
//! | 3   | DEC    | 50..1500 ms  | amp-decay time |
//! | 4   | DRIVE  | 1.0..6.0     | saturation amount |
//! | 5   | LEVEL  | 0..1         | per-machine output level |
//! | 6   | WAVE   | (reserved)   | sin/asym/triangle toggle (phase 2) |
//! | 7   | TRN    | (reserved)   | transient layer (phase 2) |

use crate::dsp::{decay_coeff, fast, DecayEnv, SineOsc};
use crate::machines::NUM_MACROS;
use crate::SAMPLE_RATE;

/// BD Classic machine.
pub struct BdClassic {
    osc: SineOsc,
    amp_env: DecayEnv,
    pitch_env: DecayEnv,
    // Cached from params so `tick` never reads the macro array.
    start_hz: f32,
    end_hz: f32,
    sweep_range: f32,
    drive: f32,
    level: f32,
}

impl BdClassic {
    /// Build the machine with the given macro values applied.
    pub fn new(macros: &[f32; NUM_MACROS]) -> Self {
        let mut m = Self {
            osc: SineOsc::new(),
            amp_env: DecayEnv::new(0.0),
            pitch_env: DecayEnv::new(0.0),
            start_hz: 0.0,
            end_hz: 0.0,
            sweep_range: 0.0,
            drive: 0.0,
            level: 0.0,
        };
        m.set_macros(macros);
        m
    }

    /// Recompute coefficients from macros. Setup rate.
    pub fn set_macros(&mut self, macros: &[f32; NUM_MACROS]) {
        let end_hz = 30.0 + 90.0 * macros[0]; // TUNE 30..120 Hz
        let pitch_ratio = 1.0 + 10.0 * macros[1]; // SWEEP 1×..11×
        let pitch_decay_s = 0.005 + 0.15 * macros[2]; // SWP_T 5..155 ms
        let amp_decay_s = 0.05 + 1.45 * macros[3]; // DEC  50..1500 ms
        let drive = 1.0 + 5.0 * macros[4]; // DRIVE 1..6
        let level = macros[5]; // LEVEL 0..1

        self.start_hz = end_hz * pitch_ratio;
        self.end_hz = end_hz;
        self.sweep_range = self.start_hz - self.end_hz;
        self.drive = drive;
        self.level = level;

        self.amp_env
            .set_coeff(decay_coeff(amp_decay_s, SAMPLE_RATE));
        self.pitch_env
            .set_coeff(decay_coeff(pitch_decay_s, SAMPLE_RATE));
        // Macros 6 (WAVE) and 7 (TRN) are reserved for Phase 2 expansion
        // (oscillator shape, transient layer). Macros 6/7 default to 0,
        // which is sine-only / no transient — the previous voice's sound.
    }

    /// Begin a hit at `velocity` (0.0..=1.0).
    pub fn trigger(&mut self, velocity: f32) {
        self.amp_env.trigger(velocity);
        self.pitch_env.trigger(1.0);
        // Consistent phase on every hit so every transient is identical.
        self.osc.reset_phase();
    }

    /// Silence.
    pub fn reset(&mut self) {
        self.amp_env.reset();
        self.pitch_env.reset();
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
            return 0.0;
        }

        // Linear sweep on a decaying envelope gives an exponential-feeling
        // glide without a per-sample powf.
        let sweep = self.pitch_env.tick();
        self.osc.set_freq(self.end_hz + self.sweep_range * sweep);

        let raw = self.osc.tick() * amp;
        fast::soft_clip(raw * self.drive) * self.level
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn silent_until_struck() {
        let macros = crate::machines::MachineId::BdClassic.default_macros();
        let mut k = BdClassic::new(&macros);
        for _ in 0..1000 {
            assert_eq!(k.tick(), 0.0);
        }
    }

    #[test]
    fn velocity_scales_output() {
        let macros = crate::machines::MachineId::BdClassic.default_macros();
        let peak_at = |vel: f32| {
            let mut k = BdClassic::new(&macros);
            k.trigger(vel);
            let mut peak = 0.0f32;
            for _ in 0..4800 {
                peak = peak.max(libm::fabsf(k.tick()));
            }
            peak
        };
        let quiet = peak_at(0.25);
        let loud = peak_at(1.0);
        assert!(loud > quiet, "velocity had no effect: {quiet} vs {loud}");
    }

    #[test]
    fn pitch_falls_over_time() {
        // Zero crossings early > late, since the sweep is downward.
        let macros = crate::machines::MachineId::BdClassic.default_macros();
        let mut k = BdClassic::new(&macros);
        k.trigger(1.0);

        let window = (0.01 * SAMPLE_RATE) as usize;
        let count_crossings = |k: &mut BdClassic, n: usize| {
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

        let early = count_crossings(&mut k, window);
        for _ in 0..window * 4 {
            k.tick();
        }
        let late = count_crossings(&mut k, window);
        assert!(early > late, "pitch did not fall: {early} then {late}");
    }
}
