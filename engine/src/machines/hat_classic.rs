//! Hat Classic: bandpassed noise with a very short decay.
//!
//! The cheapest machine here by a wide margin — no oscillator, two one-pole
//! filters, an envelope, a PRNG. Worth knowing when budgeting: if you end up
//! over on cycles, hats are not where the problem is.
//!
//! A more convincing metallic hat uses six detuned square waves through a
//! bandpass (the 808 approach) rather than noise — that arrives in a later
//! phase as a separate machine (HH Basic / HH Lab) rather than growing this
//! one.
//!
//! # Macros
//!
//! Canonical 4-bank layout (PITCH/FILTER/AMP/MOD), flat index `bank*8+slot`,
//! MIDI CC `20 + flat` on the track's channel:
//!
//! | idx | CC  | name      | range          | notes |
//! |-----|-----|-----------|----------------|-------|
//! | 5   | 25  | MACH      | 0..1           | machine selector (quantised over MachineId::ALL) |
//! | 8   | 28  | HPF       | 2000..11000 Hz | highpass colour |
//! | 9   | 29  | LPF       | 4000..16000 Hz | lowpass top; takes the fizz off |
//! | 16  | 36  | LEVEL     | 0..1           | per-machine output level |
//! | 18  | 38  | DEC       | 10..510 ms     | decay time; <100 ms reads as closed |
//! | 22  | 42  | SEND.DLY  | 0..1           | delay send (track-routed) |
//! | 23  | 43  | SEND.RVB  | 0..1           | reverb send (track-routed) |
//!
//! Noise-only, so the PITCH bank is RESV (default 0.0) and ignored apart
//! from the track-routed machine selector at slot 5.

use crate::dsp::filter::cutoff_coeff;
use crate::dsp::{decay_coeff, DecayEnv, Noise, OnePoleHp, OnePoleLp};
use crate::machines::{NUM_MACROS, SLOT_CUT, SLOT_DECAY, SLOT_LEVEL, SLOT_LPF};
use crate::SAMPLE_RATE;

/// Hat Classic machine.
pub struct HatClassic {
    noise: Noise,
    env: DecayEnv,
    hp: OnePoleHp,
    lp: OnePoleLp,
    level: f32,
}

impl HatClassic {
    /// Build the machine with the given macro values applied.
    pub fn new(macros: &[f32; NUM_MACROS]) -> Self {
        let mut h = Self {
            // A different seed from the snare, so the two do not correlate
            // when they land on the same step.
            noise: Noise::new(0x5851_F42D),
            env: DecayEnv::new(0.0),
            hp: OnePoleHp::new(0.0),
            lp: OnePoleLp::new(0.0),
            level: 0.0,
        };
        h.set_macros(macros);
        h
    }

    /// Recompute coefficients from macros. Setup rate.
    pub fn set_macros(&mut self, macros: &[f32; NUM_MACROS]) {
        let decay_s = 0.01 + 0.5 * macros[SLOT_DECAY]; // DEC 10..510 ms
        let hp_hz = 2000.0 + 9000.0 * macros[SLOT_CUT]; // HPF 2..11 kHz
        let lp_hz = 4000.0 + 12000.0 * macros[SLOT_LPF]; // LPF 4..16 kHz
        let level = macros[SLOT_LEVEL]; // LEVEL 0..1

        self.env.set_coeff(decay_coeff(decay_s, SAMPLE_RATE));
        self.hp.set_coeff(cutoff_coeff(hp_hz, SAMPLE_RATE));
        self.lp.set_coeff(cutoff_coeff(lp_hz, SAMPLE_RATE));
        self.level = level;
    }

    /// Hit it.
    pub fn trigger(&mut self, velocity: f32) {
        self.env.trigger(velocity);
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

    /// Transpose is a no-op — bandpassed noise has no pitch to shift.
    /// Present so [`MachineSlot::retune`](crate::machines::MachineSlot::retune)
    /// can dispatch uniformly across the catalogue.
    pub fn retune(&mut self, _semis: f32) {}

    /// One sample.
    #[inline(always)]
    pub fn tick(&mut self) -> f32 {
        let amp = self.env.tick();
        if amp == 0.0 {
            return 0.0;
        }
        let n = self.noise.tick();
        let band = self.lp.tick(self.hp.tick(n));
        band * amp * self.level
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::machines::MachineId;

    #[test]
    fn silent_until_struck() {
        let macros = MachineId::HatClassic.default_macros();
        let mut h = HatClassic::new(&macros);
        for _ in 0..1000 {
            assert_eq!(h.tick(), 0.0);
        }
    }

    #[test]
    fn decays_quickly() {
        let macros = MachineId::HatClassic.default_macros();
        let mut h = HatClassic::new(&macros);
        h.trigger(1.0);
        for _ in 0..(0.5 * SAMPLE_RATE) as usize {
            h.tick();
        }
        assert!(!h.is_active(), "closed hat rang for too long");
    }

    #[test]
    fn output_is_bounded() {
        let macros = MachineId::HatClassic.default_macros();
        let mut h = HatClassic::new(&macros);
        for _ in 0..200 {
            h.trigger(1.0);
            for _ in 0..512 {
                let s = h.tick();
                assert!(s.abs() <= 1.0, "hat escaped: {s}");
            }
        }
    }
}
