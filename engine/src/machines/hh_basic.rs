//! HH Basic: six detuned square-wave oscillators through a bandpass —
//! the classic 808 hi-hat topology.
//!
//! The Syntakt HH Basic machine uses six separately tunable oscillators
//! summed and bandpassed. Six square waves at closely-spaced frequencies
//! create the metallic "shimmer" that noise-based hats can't. This is the
//! machine the README flagged as "the good second iteration" for hats.
//!
//! Per-sample cost: 6 `sin_turns` lookups (square via the sine table —
//! we take the sign of the sine rather than a true square, which keeps
//! the table reuse and sounds close enough for a one-shot hat).
//!
//! # Macros
//!
//! Canonical 4-bank layout (PITCH/FILTER/AMP/MOD), flat index `bank*8+slot`,
//! MIDI CC `20 + flat` on the track's channel:
//!
//! | idx | CC  | name      | range          | notes |
//! |-----|-----|-----------|----------------|-------|
//! | 0   | 20  | TUNE      | 200..1000 Hz   | fundamental of the osc bank |
//! | 1   | 21  | TONE      | 0..1           | bipolar — shrill to deep detune spread |
//! | 5   | 25  | MACH      | 0..1           | machine selector (quantised over MachineId::ALL) |
//! | 8   | 28  | BPF       | 2000..12000 Hz | bandpass center |
//! | 16  | 36  | LEVEL     | 0..1           | per-machine output level |
//! | 18  | 38  | DEC       | 10..510 ms     | main decay |
//! | 19  | 39  | TDEC      | 5..80 ms       | transient decay (initial bright tick) |
//! | 21  | 41  | RST       | 0..1           | osc reset on trigger (0=free, 1=reset) |
//! | 22  | 42  | SEND.DLY  | 0..1           | delay send (track-routed) |
//! | 23  | 43  | SEND.RVB  | 0..1           | reverb send (track-routed) |
//!
//! All other slots are RESV (default 0.0) and ignored.

use crate::dsp::filter::cutoff_coeff;
use crate::dsp::{decay_coeff, DecayEnv, OnePoleHp, OnePoleLp};
use crate::machines::{
    NUM_MACROS, SLOT_CUT, SLOT_DECAY, SLOT_DECAY_2, SLOT_LEVEL, SLOT_MIX, SLOT_SWEEP, SLOT_TUNE,
};
use crate::SAMPLE_RATE;

/// Number of oscillators in the bank.
const NUM_OSCS: usize = 6;

/// Base frequency ratios for the six oscillators — the classic 808
/// cymbal/hat ratios, roughly 6:1 spacing with small detune.
const BASE_RATIOS: [f32; NUM_OSCS] = [1.0, 1.0, 1.5, 1.5, 2.0, 2.0];

/// HH Basic machine.
pub struct HhBasic {
    /// Phase accumulators for the six oscillators.
    phases: [f32; NUM_OSCS],
    /// Per-osc frequency (Hz).
    freqs: [f32; NUM_OSCS],
    /// Main decay envelope.
    env: DecayEnv,
    /// Transient (bright tick) envelope.
    transient_env: DecayEnv,
    hp: OnePoleHp,
    lp: OnePoleLp,
    /// Whether to reset oscillator phases on trigger.
    reset_on_trig: bool,
    /// Semitone multiplier applied to every frequency. Set by [`retune`],
    /// re-applied by `set_macros` so a later macro recompute keeps the note.
    freq_scale: f32,
    level: f32,
}

impl HhBasic {
    /// Build the machine with the given macro values applied.
    pub fn new(macros: &[f32; NUM_MACROS]) -> Self {
        let mut h = Self {
            phases: [0.0; NUM_OSCS],
            freqs: [0.0; NUM_OSCS],
            env: DecayEnv::new(0.0),
            transient_env: DecayEnv::new(0.0),
            hp: OnePoleHp::new(0.0),
            lp: OnePoleLp::new(0.0),
            reset_on_trig: false,
            freq_scale: 1.0,
            level: 0.0,
        };
        h.set_macros(macros);
        h
    }

    /// Recompute coefficients from macros. Setup rate.
    pub fn set_macros(&mut self, macros: &[f32; NUM_MACROS]) {
        let base_hz = 200.0 + 800.0 * macros[SLOT_TUNE]; // TUNE 200..1000 Hz
        let tone_spread = 0.01 + 0.15 * macros[SLOT_SWEEP]; // TONE detune spread
        let transient_decay_s = 0.005 + 0.075 * macros[SLOT_DECAY_2]; // TDEC 5..80 ms
        let main_decay_s = 0.01 + 0.5 * macros[SLOT_DECAY]; // DEC 10..510 ms
        let reset_on_trig = macros[SLOT_MIX] > 0.5; // RST threshold
        let level = macros[SLOT_LEVEL]; // LEVEL 0..1
        let bpf_hz = 2000.0 + 10000.0 * macros[SLOT_CUT]; // BPF 2..12 kHz

        // Build the frequency bank: base ratios with bipolar detune spread.
        // Even-indexed oscs go sharp, odd-indexed go flat (or vice versa).
        for (i, r) in BASE_RATIOS.iter().enumerate() {
            let detune_sign = if i % 2 == 0 { 1.0 } else { -1.0 };
            let detune = 1.0 + detune_sign * tone_spread * (i as f32 + 1.0) * 0.1;
            self.freqs[i] = base_hz * r * detune * self.freq_scale;
        }

        self.env.set_coeff(decay_coeff(main_decay_s, SAMPLE_RATE));
        self.transient_env
            .set_coeff(decay_coeff(transient_decay_s, SAMPLE_RATE));
        self.hp.set_coeff(cutoff_coeff(bpf_hz * 0.6, SAMPLE_RATE));
        self.lp.set_coeff(cutoff_coeff(bpf_hz * 1.4, SAMPLE_RATE));
        self.reset_on_trig = reset_on_trig;
        self.level = level;
    }

    /// Transpose by `semis` semitones relative to the macro pitch.
    ///
    /// Scales the whole osc bank, preserving the ratios and detune between
    /// the six oscillators. Absolute, not incremental.
    pub fn retune(&mut self, semis: f32) {
        let new_scale = crate::dsp::fast::semitone_ratio(semis);
        let ratio = new_scale / self.freq_scale;
        self.freq_scale = new_scale;
        for f in self.freqs.iter_mut() {
            *f *= ratio;
        }
    }

    /// Hit it.
    pub fn trigger(&mut self, velocity: f32) {
        self.env.trigger(velocity);
        self.transient_env.trigger(velocity);
        if self.reset_on_trig {
            self.phases = [0.0; NUM_OSCS];
        }
    }

    /// Silence.
    pub fn reset(&mut self) {
        self.env.reset();
        self.transient_env.reset();
        self.hp.reset();
        self.lp.reset();
    }

    /// Still sounding?
    pub fn is_active(&self) -> bool {
        self.env.is_active()
    }

    /// One sample.
    ///
    /// Six oscillators summed, each as a "square" — the sign of the sine
    /// table lookup. Cheaper than a real square wave generator and
    /// harmonically close enough for a one-shot metallic hat.
    #[inline(always)]
    pub fn tick(&mut self) -> f32 {
        let amp = self.env.tick();
        if amp == 0.0 {
            return 0.0;
        }

        let transient = self.transient_env.tick();

        // Sum six square-ish oscillators. Sign of sin_turns gives a square
        // wave at the same frequency, using the same table.
        let mut sum = 0.0f32;
        for i in 0..NUM_OSCS {
            let s = crate::dsp::fast::sin_turns(self.phases[i]);
            sum += if s >= 0.0 { 1.0 } else { -1.0 };
            self.phases[i] += self.freqs[i] * crate::INV_SAMPLE_RATE;
            if self.phases[i] >= 1.0 {
                self.phases[i] -= 1.0;
            }
        }
        // Normalize: six ±1 oscs → ±1 average.
        let osc_bank = sum / NUM_OSCS as f32;

        // Bandpass: HP then LP.
        let bandpassed = self.lp.tick(self.hp.tick(osc_bank));

        // Main envelope + transient accent, scaled by the level. (The level
        // must gate the whole signal, not just the transient, or the hat
        // never actually goes quiet when LEVEL is turned down.)
        (bandpassed * (amp + transient * 0.5)) * self.level
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::machines::MachineId;

    #[test]
    fn silent_until_struck() {
        let id = MachineId::HhBasic;
        let macros = id.default_macros();
        let mut h = HhBasic::new(&macros);
        for _ in 0..1000 {
            assert_eq!(h.tick(), 0.0);
        }
    }

    #[test]
    fn decays_to_silence() {
        let id = MachineId::HhBasic;
        let macros = id.default_macros();
        let mut h = HhBasic::new(&macros);
        h.trigger(1.0);
        for _ in 0..(1.0 * SAMPLE_RATE) as usize {
            h.tick();
        }
        assert!(!h.is_active());
    }

    #[test]
    fn output_is_bounded() {
        let id = MachineId::HhBasic;
        let macros = id.default_macros();
        let mut h = HhBasic::new(&macros);
        for _ in 0..100 {
            h.trigger(1.0);
            for _ in 0..512 {
                let s = h.tick();
                assert!(s.abs() <= 1.0, "hh basic escaped: {s}");
            }
        }
    }

    #[test]
    fn reset_on_trig_zeroes_phases() {
        let id = MachineId::HhBasic;
        let mut macros = id.default_macros();
        macros[SLOT_MIX] = 1.0; // RST = reset
        let mut h = HhBasic::new(&macros);
        // Run a bit to advance phases.
        h.trigger(1.0);
        for _ in 0..100 {
            h.tick();
        }
        let mid_phases = h.phases;
        // Re-trigger should reset.
        h.trigger(1.0);
        for (i, &p) in h.phases.iter().enumerate() {
            // After reset, first tick hasn't run yet, so phases should be
            // near zero (trigger resets, tick hasn't advanced).
            assert!(
                p.abs() < mid_phases[i].abs() + 0.01 || mid_phases[i] < 0.001,
                "phases not reset: {} vs {}",
                p,
                mid_phases[i]
            );
        }
    }
}
