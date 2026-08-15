//! BD VA: virtual-analogue kick via bridged-T resonator.
//!
//! Mirrors the TR-808 kick voice — a pulse excitates a tuned resonant
//! network rather than a swept sine. Distinct from [`BdClassic`](super::bd_classic::BdClassic),
//! an actual pitch-swept sine through a saturator, and from [`BdFm`](super::bd_fm::BdFm),
//! a 2-operator FM kick. The VA character is sharper and more "drum-head"-
//! like: the resonator rings at its tuned frequency, with a pitch
//! transient produced by sliding the resonator's target frequency over
//! the first few milliseconds of the hit.
//!
//! # Topology
//!
//! ```text
//!   amp env  ──► pulse excitation ──► bridged-T resonator ──► out
//!   pitch env──► resonator target_hz (per-sample retune)
//! ```
//!
//! The amp env *is* the pulse excitation: a fast-decaying envelope
//! injected into the resonator as the input. The resonator rings out per
//! its Q, dying on its own time scale. The pitch env slides the
//! resonator's `target_hz` from `base_hz + sweep_hz` down to `base_hz`,
//! giving the kick a downward "thump" transient.
//!
//! Per-sample cost: one [`BridgedT::set_hz`] (one `fast::sin_turns`
//! lookup, no divide — the Phase 11 Option B split) plus one
//! [`BridgedT::process`] (4 mul / 3 add). The Q-dependent divide and the
//! second lookup run at control rate in `set_macros` via
//! [`BridgedT::set_q`], called once per macro change (or per block under
//! Q modulation).
//!
//! # Macros
//!
//! Canonical 4-bank layout (PITCH/FILTER/AMP/MOD), flat index `bank*8+slot`,
//! MIDI CC `20 + flat` on the track's channel:
//!
//! | idx | CC  | name    | range        | notes |
//! |-----|-----|---------|--------------|-------|
//! | 0   | 20  | TUNE    | 30..120 Hz   | settled resonator frequency |
//! | 1   | 21  | SWEEP   | 0..120 Hz    | pitch-sweep depth above TUNE |
//! | 2   | 22  | SWP_T   | 5..55 ms     | pitch-sweep decay time |
//! | 5   | 25  | MACH    | 0..1         | machine selector (quantised over MachineId::ALL) |
//! | 9   | 29  | Q       | 0.5..10      | resonator Q (FILTER bank: the "resonance" slot) |
//! | 16  | 36  | LEVEL   | 0..1         | per-machine output level |
//! | 17  | 37  | PAN     | 0..1         | (track-routed; ignored here) |
//! | 18  | 38  | DEC     | 50..1500 ms  | amp-decay (pulse excitation) time |
//! | 22  | 42  | SEND.DLY| 0..1         | delay send (track-routed) |
//! | 23  | 43  | SEND.RVB| 0..1         | reverb send (track-routed) |
//!
//! All other slots are RESV (default 0.0) and ignored — the resonator
//! *is* the filter, so there is no FILTER-bank cutoff macro, only Q.

use crate::dsp::{decay_coeff, fast, BridgedT, DecayEnv};
use crate::machines::{
    NUM_MACROS, SLOT_DECAY, SLOT_LEVEL, SLOT_LPF, SLOT_SWEEP, SLOT_SWEEP_TIME, SLOT_TUNE,
};
use crate::SAMPLE_RATE;

/// BD VA machine.
pub struct BdVa {
    resonator: BridgedT,
    amp_env: DecayEnv,
    pitch_env: DecayEnv,
    /// Resonator target frequency at rest (post pitch-sweep).
    base_hz: f32,
    /// Pitch-sweep depth in Hz, added to `base_hz` at trigger moment and
    /// decaying to zero via `pitch_env`.
    sweep_hz: f32,
    /// Resonator Q (FILTER bank). Cached so `tick` never reads the macro
    /// array, and so `retune` doesn't have to track it.
    q: f32,
    /// Per-machine output level, 0..1.
    level: f32,
    /// Semitone multiplier applied to every frequency. Set by [`retune`],
    /// re-applied by `set_macros` so a later macro recompute keeps the note.
    freq_scale: f32,
}

impl BdVa {
    /// Build the machine with the given macro values applied.
    pub fn new(macros: &[f32; NUM_MACROS]) -> Self {
        let mut m = Self {
            resonator: BridgedT::new(),
            amp_env: DecayEnv::new(0.0),
            pitch_env: DecayEnv::new(0.0),
            base_hz: 0.0,
            sweep_hz: 0.0,
            q: 0.0,
            level: 0.0,
            freq_scale: 1.0,
        };
        m.set_macros(macros);
        m
    }

    /// Recompute coefficients from macros. Setup rate.
    pub fn set_macros(&mut self, macros: &[f32; NUM_MACROS]) {
        let base_hz = 30.0 + 90.0 * macros[SLOT_TUNE]; // TUNE 30..120 Hz
        let sweep_depth = 120.0 * macros[SLOT_SWEEP]; // SWEEP 0..120 Hz above base
        let pitch_decay_s = 0.005 + 0.05 * macros[SLOT_SWEEP_TIME]; // SWP_T 5..55 ms
        let amp_decay_s = 0.05 + 1.45 * macros[SLOT_DECAY]; // DEC  50..1500 ms
        let q = 0.5 + 9.5 * macros[SLOT_LPF]; // Q 0.5..10 (FILTER resonance slot)
        let level = macros[SLOT_LEVEL]; // LEVEL 0..1

        self.base_hz = base_hz * self.freq_scale;
        self.sweep_hz = sweep_depth * self.freq_scale;
        self.q = q;
        self.level = level;

        self.amp_env
            .set_coeff(decay_coeff(amp_decay_s, SAMPLE_RATE));
        self.pitch_env
            .set_coeff(decay_coeff(pitch_decay_s, SAMPLE_RATE));
        // Resonator control-rate setup: anchor the Q-dependent bandwidth
        // family to `base_hz` (the settled pitch — where the hit spends
        // most of its time), and seed `a1` alongside. `tick` per-sample
        // calls only `set_hz(current_hz)` — the cheap path, one lookup,
        // no divide. Under Q modulation the mod control pass re-runs this
        // every block, refreshing the cached `b0`/`a2`/`a0_inv`.
        self.resonator.set_q(self.q, self.base_hz);
    }

    /// Transpose by `semis` semitones relative to the macro pitch.
    ///
    /// Scales `base_hz` and `sweep_hz` together, so the resonance frequency
    /// and the pitch-sweep depth both move with the note. Absolute, not
    /// incremental — calling it twice with the same value is a no-op.
    pub fn retune(&mut self, semis: f32) {
        let new_scale = fast::semitone_ratio(semis);
        let ratio = new_scale / self.freq_scale;
        self.freq_scale = new_scale;
        self.base_hz *= ratio;
        self.sweep_hz *= ratio;
    }

    /// Begin a hit at `velocity` (0.0..=1.0).
    pub fn trigger(&mut self, velocity: f32) {
        self.amp_env.trigger(velocity);
        self.pitch_env.trigger(1.0);
        // Reset the resonator so the new transient is not contaminated by
        // the tail of the previous hit.
        self.resonator.reset();
    }

    /// Silence.
    pub fn reset(&mut self) {
        self.amp_env.reset();
        self.pitch_env.reset();
        self.resonator.reset();
    }

    /// Still sounding?
    pub fn is_active(&self) -> bool {
        self.amp_env.is_active()
    }

    /// One sample.
    ///
    /// The amp env feeds the resonator as a pulse excitation; the
    /// resonator's own decay (set by `Q`) shapes the body of the hit.
    /// The pitch env slides the resonator's `target_hz` downward.
    #[inline(always)]
    pub fn tick(&mut self) -> f32 {
        let amp = self.amp_env.tick();
        if amp == 0.0 {
            return 0.0;
        }

        let sweep = self.pitch_env.tick();
        let current_hz = self.base_hz + self.sweep_hz * sweep;
        // Per-sample: just the moving-frequency coefficient. The Q-dependent
        // family (alpha, a0_inv, b0, a2) is cached from `set_macros` and
        // stays cached through a hit unless Q is being modulated (in which
        // case the block-rate control pass re-runs `set_macros` and
        // refreshes it). One `sin_turns` lookup, no divide.
        self.resonator.set_hz(current_hz);
        self.resonator.process(amp) * self.level
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::machines::MachineId;

    #[test]
    fn silent_until_struck() {
        let id = MachineId::BdVa;
        let macros = id.default_macros();
        let mut k = BdVa::new(&macros);
        for _ in 0..1000 {
            assert_eq!(k.tick(), 0.0);
        }
    }

    #[test]
    fn velocity_scales_output() {
        let id = MachineId::BdVa;
        let macros = id.default_macros();
        let peak_at = |vel: f32| {
            let mut k = BdVa::new(&macros);
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
    fn tune_macro_changes_resonator_pitch() {
        // With no pitch sweep, the resonator sits at one tone for the whole
        // hit — high TUNE produces a clearly higher-frequency ring than low
        // TUNE over the same window. This is the direct macro→target_hz
        // check; the "pitch falls over time" path is exercised only via
        // `sweep_macro_shortens_first_window_pitch`.
        let id = MachineId::BdVa;
        let mut low = id.default_macros();
        low[SLOT_TUNE] = 0.0; // 30 Hz base
        low[SLOT_SWEEP] = 0.0; // no pitch sweep — pure base-hz ring
        low[SLOT_LPF] = 0.7; // higher Q for cleaner ring

        let mut high = id.default_macros();
        high[SLOT_TUNE] = 1.0; // 120 Hz base
        high[SLOT_SWEEP] = 0.0;
        high[SLOT_LPF] = 0.7;

        let crossings_in_50ms = |macros: &[f32; NUM_MACROS]| {
            let mut k = BdVa::new(macros);
            k.trigger(1.0);
            let n = (0.05 * SAMPLE_RATE) as usize;
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

        let lo = crossings_in_50ms(&low);
        let hi = crossings_in_50ms(&high);
        // 30 Hz × 50 ms ≈ 1.5 cyc → 3 crossings; 120 Hz × 50 ms ≈ 6 cyc → 12.
        assert!(
            hi > lo * 2,
            "TUNE macro should set resonator pitch: low={lo}, high={hi}"
        );
    }

    #[test]
    fn decays_to_silence() {
        let id = MachineId::BdVa;
        let macros = id.default_macros();
        let mut k = BdVa::new(&macros);
        k.trigger(1.0);
        for _ in 0..(5.0 * SAMPLE_RATE) as usize {
            k.tick();
        }
        assert!(!k.is_active());
    }

    #[test]
    fn retune_transposes_and_survives_recompute() {
        let id = MachineId::BdVa;
        let macros = id.default_macros();
        let window = (0.02 * SAMPLE_RATE) as usize;
        let crossings = |semis: f32, recompute: bool| {
            let mut k = BdVa::new(&macros);
            k.retune(semis);
            if recompute {
                k.set_macros(&macros);
            }
            k.trigger(1.0);
            let mut prev = k.tick();
            let mut c = 0;
            for _ in 1..window {
                let s = k.tick();
                if (prev < 0.0) != (s < 0.0) {
                    c += 1;
                }
                prev = s;
            }
            c
        };
        let base = crossings(0.0, false);
        let up = crossings(12.0, false);
        let up_recomputed = crossings(12.0, true);
        assert!(up > base, "octave-up should cross more: {base} vs {up}");
        assert!(
            i32::abs(up_recomputed - up) <= 1,
            "set_macros dropped the retune: {up} vs {up_recomputed}"
        );
    }

    #[test]
    fn extreme_macros_do_not_produce_nans() {
        let id = MachineId::BdVa;
        let base = id.default_macros();
        for &v in &[0.0f32, 1.0f32] {
            let mut macros = base;
            for m in macros.iter_mut() {
                *m = v;
            }
            let mut k = BdVa::new(&macros);
            k.trigger(1.0);
            for _ in 0..(2.0 * SAMPLE_RATE) as usize {
                let s = k.tick();
                assert!(s.is_finite(), "non-finite output at macro={v}: {s}");
            }
        }
    }
}
