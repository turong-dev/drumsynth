//! CB Classic: two-oscillator cowbell.
//!
//! The 808 cowbell: two square-ish oscillators at a fixed ratio (the
//! classic 540/800 Hz pair), summed and bandpassed. Short decay, bright
//! and boxy. The Syntakt CB Classic adds a detune macro; we do the same.
//!
//! # Macros
//!
//! | idx | name   | range         | notes |
//! |-----|--------|---------------|-------|
//! | 0   | TUNE   | 300..1000 Hz  | osc A frequency |
//! | 1   | DEC    | 30..400 ms    | decay |
//! | 2   | DET    | 1.0..1.5      | osc B/A ratio |
//! | 3   | BPF    | 300..4000 Hz  | bandpass center |
//! | 4   | LEVEL  | 0..1          | per-machine output level |
//! | 5-7 | RESV*  | (reserved)    | Q, waveform, etc. |

use crate::dsp::filter::cutoff_coeff;
use crate::dsp::{decay_coeff, DecayEnv, OnePoleHp, OnePoleLp};
use crate::machines::NUM_MACROS;
use crate::SAMPLE_RATE;

/// CB Classic machine.
pub struct CbClassic {
    phase_a: f32,
    phase_b: f32,
    freq_a: f32,
    freq_b: f32,
    env: DecayEnv,
    hp: OnePoleHp,
    lp: OnePoleLp,
    level: f32,
}

impl CbClassic {
    /// Build the machine with the given macro values applied.
    pub fn new(macros: &[f32; NUM_MACROS]) -> Self {
        let mut m = Self {
            phase_a: 0.0,
            phase_b: 0.0,
            freq_a: 0.0,
            freq_b: 0.0,
            env: DecayEnv::new(0.0),
            hp: OnePoleHp::new(0.0),
            lp: OnePoleLp::new(0.0),
            level: 0.0,
        };
        m.set_macros(macros);
        m
    }

    /// Recompute coefficients from macros. Setup rate.
    pub fn set_macros(&mut self, macros: &[f32; NUM_MACROS]) {
        let base_hz = 300.0 + 700.0 * macros[0]; // TUNE 300..1000 Hz
        let decay_s = 0.03 + 0.37 * macros[1]; // DEC 30..400 ms
        let detune = 1.0 + 0.5 * macros[2]; // DET 1.0..1.5
        let bpf_hz = 300.0 + 3700.0 * macros[3]; // BPF 300..4000 Hz
        let level = macros[4]; // LEVEL 0..1

        self.freq_a = base_hz;
        self.freq_b = base_hz * detune;
        self.env.set_coeff(decay_coeff(decay_s, SAMPLE_RATE));
        self.hp.set_coeff(cutoff_coeff(bpf_hz * 0.5, SAMPLE_RATE));
        self.lp.set_coeff(cutoff_coeff(bpf_hz * 2.0, SAMPLE_RATE));
        self.level = level;
    }

    /// Hit it.
    pub fn trigger(&mut self, velocity: f32) {
        self.env.trigger(velocity);
        self.phase_a = 0.0;
        self.phase_b = 0.0;
    }

    /// Silence.
    pub fn reset(&mut self) {
        self.env.reset();
        self.hp.reset();
        self.lp.reset();
    }

    /// Still sounding?
    pub fn is_active(&self) -> bool {
        self.env.is_active()
    }

    /// One sample. Two square-wave oscillators → bandpass.
    #[inline(always)]
    pub fn tick(&mut self) -> f32 {
        let amp = self.env.tick();
        if amp == 0.0 {
            return 0.0;
        }

        // Square via sign of sin_turns — shared table, no extra code.
        let sq_a = if crate::dsp::fast::sin_turns(self.phase_a) >= 0.0 {
            1.0
        } else {
            -1.0
        };
        let sq_b = if crate::dsp::fast::sin_turns(self.phase_b) >= 0.0 {
            1.0
        } else {
            -1.0
        };

        self.phase_a += self.freq_a * crate::INV_SAMPLE_RATE;
        if self.phase_a >= 1.0 {
            self.phase_a -= 1.0;
        }
        self.phase_b += self.freq_b * crate::INV_SAMPLE_RATE;
        if self.phase_b >= 1.0 {
            self.phase_b -= 1.0;
        }

        // Two squares at different pitches, summed and bandpassed.
        let sum = (sq_a + sq_b) * 0.5;
        let bandpassed = self.lp.tick(self.hp.tick(sum));
        bandpassed * amp * self.level
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::machines::MachineId;

    fn peak_over(s: &mut CbClassic, n: usize) -> f32 {
        let mut peak = 0.0f32;
        for _ in 0..n {
            peak = peak.max(libm::fabsf(s.tick()));
        }
        peak
    }

    #[test]
    fn silent_until_struck() {
        let id = MachineId::CbClassic;
        let macros = id.default_macros();
        let mut s = CbClassic::new(&macros);
        assert_eq!(peak_over(&mut s, 1000), 0.0);
    }

    #[test]
    fn produces_sound() {
        let id = MachineId::CbClassic;
        let macros = id.default_macros();
        let mut s = CbClassic::new(&macros);
        s.trigger(1.0);
        assert!(peak_over(&mut s, 1200) > 0.05, "cowbell too quiet");
    }

    #[test]
    fn decays_to_silence() {
        let id = MachineId::CbClassic;
        let macros = id.default_macros();
        let mut s = CbClassic::new(&macros);
        s.trigger(1.0);
        for _ in 0..(2.0 * SAMPLE_RATE) as usize {
            s.tick();
        }
        assert!(!s.is_active());
    }

    #[test]
    fn output_is_bounded() {
        let id = MachineId::CbClassic;
        let macros = id.default_macros();
        let mut s = CbClassic::new(&macros);
        for _ in 0..100 {
            s.trigger(1.0);
            for _ in 0..512 {
                let v = s.tick();
                assert!(v.abs() <= 1.0, "cowbell escaped: {v}");
            }
        }
    }
}
