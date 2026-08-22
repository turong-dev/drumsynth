//! Dub Siren: sine carrier with an internal pitch-modulating LFO, gated
//! by a long self-timed AHD envelope.
//!
//! The classic dub-reggae siren: a slow warble that dives and rises,
//! played back through long delay throws. Distinct from every other
//! machine in the catalogue, which are one-shot percussive hits — the
//! siren is a *gesture*: it lasts seconds, and its [`is_active`]
//! window is owned by an internal [`AhdEnv`] rather than a [`DecayEnv`].
//!
//! # Why self-timed, not gated
//!
//! The engine has no NoteOff path ([`EngineEvent`](crate::EngineEvent)
//! carries [`NoteOn`](crate::midi::MidiEvent::NoteOn) only), and the
//! per-track `is_active` early-out at `lib.rs:1381`/`lib.rs:1397` is
//! the budget mechanism that keeps idle kits cheap. A gated sustained
//! voice would either (a) need new NoteOff plumbing or (b) defeat the
//! early-out forever once triggered. Neither is acceptable. A
//! self-timed AHD gives the right semantics — trigger launches a
//! fixed-duration gesture, the machine reports `is_active` until the
//! AHD idles, and the early-out is preserved for the gesture's
//! duration. This is the same disposition the per-track strip amp env
//! has used since Phase 1; Phase 12 is the first time a *machine*
//! uses [`AhdEnv`].
//!
//! # Topology
//!
//! ```text
//!   internal LFO (sine/tri/saw) ──► pitch multiplier (octaves, exp2)
//!   AhdEnv (gesture)             ──► amp
//!   SineOsc (carrier × pitch mult) ──► out
//! ```
//!
//! The LFO runs at sample rate (the block-rate [`Lfo`](crate::dsp::Lfo)
//! would step a 1 Hz sweep in 6 audible increments per cycle), so it
//! is inline rather than a reused primitive. [`fast::exp2_approx`]
//! turns the LFO's bipolar output × depth-in-octaves into a per-sample
//! pitch multiplier — same family as [`fast::semitone_ratio`].
//!
//! # Macros
//!
//! Canonical 4-bank layout (PITCH/FILTER/AMP/MOD), flat index `bank*8+slot`,
//! MIDI CC `20 + flat` on the track's channel:
//!
//! | idx | CC  | name    | range        | notes |
//! |-----|-----|---------|--------------|-------|
//! | 0   | 20  | TUNE    | 100..1000 Hz | carrier base frequency |
//! | 1   | 21  | DEPTH   | 0..3 oct     | LFO pitch-sweep width |
//! | 2   | 22  | RATE    | 0.1..8 Hz    | siren LFO speed |
//! | 5   | 25  | MACH    | 0..1         | machine selector (quantised over MachineId::ALL) |
//! | 16  | 36  | LEVEL   | 0..1         | per-machine output level |
//! | 17  | 37  | PAN     | 0..1         | (track-routed; ignored here) |
//! | 18  | 38  | DEC     | 0.5..6 s     | AHD gesture length (5% atk / 85% hold / 10% dec) |
//! | 20  | 40  | SHAPE   | 0..1         | LFO waveform: sine ↔ tri ↔ saw (quantised, default tri) |
//! | 22  | 42  | SEND.DLY| 0..1         | delay send (track-routed) |
//! | 23  | 43  | SEND.RVB| 0..1         | reverb send (track-routed) |
//! | 26  | 46  | OUT     | 0..1         | track routing (Master/Aux1/2/3) |
//!
//! All other slots are RESV (default 0.0) and ignored.

use crate::dsp::{fast, AhdEnv, SineOsc};
use crate::machines::{
    NUM_MACROS, SLOT_MACH_5, SLOT_LEVEL, SLOT_MACH_7, SLOT_MACH_1, SLOT_MACH_2, SLOT_MACH_0,
};

/// Internal LFO waveform, selected by the SHAPE macro.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum SirenShape {
    /// Smooth sinusoidal pitch bend — the round "whoop" shape.
    Sine,
    /// Hard triangle — the angular "woop-woop" classic siren.
    Triangle,
    /// Rising sawtooth — a one-direction pitch climb that resets.
    Saw,
}

impl SirenShape {
    /// Quantise a 0..1 macro value to one of three waveforms in equal
    /// thirds. The default macro (0.5) lands in the middle band →
    /// [`Triangle`], the most identifiable siren shape.
    const fn from_macro(v: f32) -> Self {
        if v < 1.0 / 3.0 {
            Self::Sine
        } else if v < 2.0 / 3.0 {
            Self::Triangle
        } else {
            Self::Saw
        }
    }
}

/// Dub Siren machine.
pub struct DubSiren {
    osc: SineOsc,
    env: AhdEnv,
    /// Carrier base frequency in Hz, post-retune. The per-sample pitch
    /// multiplier from the LFO is applied on top of this.
    base_hz: f32,
    /// LFO pitch-sweep depth in octaves. The LFO outputs -1..1; the
    /// per-sample pitch multiplier is `2^(lfo * depth)`.
    depth_oct: f32,
    /// LFO phase accumulator, in turns (`0.0..1.0` = one cycle).
    lfo_phase: f32,
    /// LFO phase increment per sample = `rate / SAMPLE_RATE`.
    lfo_inc: f32,
    /// LFO waveform, set by the SHAPE macro.
    shape: SirenShape,
    /// Per-machine output level, 0..1.
    level: f32,
    /// Semitone multiplier applied to `base_hz`. Set by [`retune`],
    /// re-applied by `set_macros` so a later macro recompute keeps the
    /// note.
    freq_scale: f32,
}

impl DubSiren {
    /// Build the machine with the given macro values applied.
    pub fn new(macros: &[f32; NUM_MACROS]) -> Self {
        let mut m = Self {
            osc: SineOsc::new(),
            env: AhdEnv::new(),
            base_hz: 0.0,
            depth_oct: 0.0,
            lfo_phase: 0.0,
            lfo_inc: 0.0,
            shape: SirenShape::Triangle,
            level: 0.0,
            freq_scale: 1.0,
        };
        m.set_macros(macros);
        m
    }

    /// Recompute coefficients from macros. Setup rate.
    pub fn set_macros(&mut self, macros: &[f32; NUM_MACROS]) {
        let base_hz = 100.0 + 900.0 * macros[SLOT_MACH_0]; // TUNE 100..1000 Hz
        let depth_oct = 3.0 * macros[SLOT_MACH_1]; // DEPTH 0..3 oct
        let rate_hz = 0.1 + 7.9 * macros[SLOT_MACH_2]; // RATE 0.1..8 Hz
        let total_s = 0.5 + 5.5 * macros[SLOT_MACH_5]; // DEC 0.5..6 s
        let shape = SirenShape::from_macro(macros[SLOT_MACH_7]);
        let level = macros[SLOT_LEVEL]; // LEVEL 0..1

        self.base_hz = base_hz * self.freq_scale;
        self.depth_oct = depth_oct;
        self.lfo_inc = rate_hz * crate::INV_SAMPLE_RATE;
        self.shape = shape;
        self.level = level;

        // AHD proportions: 5% attack, 85% hold, 10% decay. The hold
        // segment is what makes a siren *sustained* — a pure attack/
        // decay would gate it like a drum hit. The decay tail is short
        // so the gesture ends cleanly without a long fade-out eating
        // budget.
        let atk = 0.05 * total_s;
        let hold = 0.85 * total_s;
        let dec = 0.10 * total_s;
        self.env.set_params(atk, hold, dec);
    }

    /// Transpose by `semis` semitones relative to the macro pitch.
    ///
    /// Scales the carrier base frequency. The LFO depth is in octaves
    /// relative to `base_hz`, so it travels with the note. Absolute,
    /// not incremental — calling it twice with the same value is a
    /// no-op.
    pub fn retune(&mut self, semis: f32) {
        let new_scale = fast::semitone_ratio(semis);
        let ratio = new_scale / self.freq_scale;
        self.freq_scale = new_scale;
        self.base_hz *= ratio;
    }

    /// Begin a gesture at `velocity` (0.0..=1.0). The AHD peak scales
    /// with velocity, so a softer hit is a quieter siren, not a
    /// shorter one — the gesture length is fixed by [`set_macros`].
    pub fn trigger(&mut self, velocity: f32) {
        self.env.trigger(velocity);
        self.osc.reset_phase();
        self.lfo_phase = 0.0;
    }

    /// Silence.
    pub fn reset(&mut self) {
        self.env.reset();
        self.osc.reset_phase();
        self.lfo_phase = 0.0;
    }

    /// Still sounding?
    ///
    /// True while the gesture AHD is in any non-Idle stage. This is
    /// the per-track early-out hook — see `lib.rs:1381`.
    pub fn is_active(&self) -> bool {
        self.env.is_active()
    }

    /// Advance the internal LFO by one sample and return its bipolar
    /// output in `-1.0..1.0`.
    #[inline(always)]
    fn tick_lfo(&mut self) -> f32 {
        let p = self.lfo_phase;
        let out = match self.shape {
            // Sine via the shared fast table — one lookup, no libm.
            SirenShape::Sine => fast::sin_turns(p),
            // Triangle: 4 * |p - 0.5| - 1, range -1..1. Phase is kept
            // in [0, 1) by the wrap below, so no floor needed.
            SirenShape::Triangle => 1.0 - 4.0 * libm::fabsf(p - 0.5),
            // Sawtooth: 2*p - 1, rising from -1 to +1 over one cycle.
            SirenShape::Saw => 2.0 * p - 1.0,
        };
        self.lfo_phase += self.lfo_inc;
        if self.lfo_phase >= 1.0 {
            self.lfo_phase -= 1.0;
        }
        out
    }

    /// One sample.
    #[inline(always)]
    pub fn tick(&mut self) -> f32 {
        let amp = self.env.tick();
        if amp == 0.0 {
            return 0.0;
        }

        // Pitch multiplier from LFO × depth-in-octaves. exp2_approx is
        // the same family as semitone_ratio; the LFO runs at audio rate
        // so the pitch glides rather than stepping.
        let lfo = self.tick_lfo();
        let pitch_mult = fast::exp2_approx(lfo * self.depth_oct);
        self.osc.set_freq(self.base_hz * pitch_mult);

        self.osc.tick() * amp * self.level
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::machines::MachineId;
    use crate::SAMPLE_RATE;

    fn peak_over(s: &mut DubSiren, n: usize) -> f32 {
        let mut peak = 0.0f32;
        for _ in 0..n {
            peak = peak.max(libm::fabsf(s.tick()));
        }
        peak
    }

    #[test]
    fn silent_until_struck() {
        let id = MachineId::DubSiren;
        let macros = id.default_macros();
        let mut s = DubSiren::new(&macros);
        assert_eq!(peak_over(&mut s, 1000), 0.0);
        assert!(!s.is_active());
    }

    #[test]
    fn velocity_scales_output() {
        let id = MachineId::DubSiren;
        let macros = id.default_macros();
        let peak_at = |vel: f32| {
            let mut s = DubSiren::new(&macros);
            s.trigger(vel);
            peak_over(&mut s, 4800)
        };
        let quiet = peak_at(0.25);
        let loud = peak_at(1.0);
        assert!(loud > quiet, "velocity had no effect: {quiet} vs {loud}");
    }

    #[test]
    fn decays_to_silence() {
        let id = MachineId::DubSiren;
        let macros = id.default_macros();
        let mut s = DubSiren::new(&macros);
        s.trigger(1.0);
        // Default DEC = 0.5 + 5.5 * 0.255 ≈ 1.9 s; allow 7 s for the
        // longest possible macro (DEC=1 → 6 s) plus settle.
        for _ in 0..(7.0 * SAMPLE_RATE) as usize {
            s.tick();
        }
        assert!(!s.is_active(), "siren did not stop");
    }

    #[test]
    fn sustained_gesture_stays_active_past_one_second() {
        // The gesture contract: a sweep is *sustained*, not a hit. At
        // default DEC (~1.9 s) the machine must still be active past
        // 1 s. This is the test that pins the budget argument — see
        // Phase 12's "sustained-gesture test" line in PLAN.md.
        let id = MachineId::DubSiren;
        let macros = id.default_macros();
        let mut s = DubSiren::new(&macros);
        s.trigger(1.0);
        for _ in 0..(1.0 * SAMPLE_RATE) as usize {
            s.tick();
        }
        assert!(s.is_active(), "siren cut short of 1 s");
        // And it must reach silence before 7 s (same window as above).
        for _ in 0..(6.0 * SAMPLE_RATE) as usize {
            s.tick();
        }
        assert!(!s.is_active(), "siren ran past 7 s");
    }

    #[test]
    fn rate_macro_changes_pitch_speed() {
        // A fast LFO RATE produces clearly more zero-crossings per
        // window than a slow one — direct macro→lfo_inc check.
        let id = MachineId::DubSiren;
        let mut slow = id.default_macros();
        slow[SLOT_MACH_2] = 0.0; // 0.1 Hz
        slow[SLOT_MACH_1] = 0.5; // 1.5 oct depth — audible sweep
        let mut fast_macros = id.default_macros();
        fast_macros[SLOT_MACH_2] = 1.0; // 8 Hz
        fast_macros[SLOT_MACH_1] = 0.5;

        let crossings_in_500ms = |macros: &[f32; NUM_MACROS]| {
            let mut s = DubSiren::new(macros);
            s.trigger(1.0);
            let n = (0.5 * SAMPLE_RATE) as usize;
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

        let lo = crossings_in_500ms(&slow);
        let hi = crossings_in_500ms(&fast_macros);
        // 0.1 Hz LFO barely moves pitch over 500 ms; 8 Hz LFO sweeps
        // the carrier dramatically, crossing far more often.
        assert!(
            hi > lo * 2,
            "RATE macro should change pitch speed: low={lo}, high={hi}"
        );
    }

    #[test]
    fn retune_transposes_and_survives_recompute() {
        let id = MachineId::DubSiren;
        let macros = id.default_macros();
        // Use a low RATE so the carrier pitch is roughly static over
        // the measurement window — the retune check needs a stable
        // pitch to count crossings against.
        let mut base_macros = macros;
        base_macros[SLOT_MACH_2] = 0.0; // 0.1 Hz
        base_macros[SLOT_MACH_1] = 0.0; // no depth — pure carrier
        let window = (0.05 * SAMPLE_RATE) as usize;
        let crossings = |semis: f32, recompute: bool| {
            let mut s = DubSiren::new(&base_macros);
            s.retune(semis);
            if recompute {
                s.set_macros(&base_macros);
            }
            s.trigger(1.0);
            let mut prev = s.tick();
            let mut c = 0;
            for _ in 1..window {
                let v = s.tick();
                if (prev < 0.0) != (v < 0.0) {
                    c += 1;
                }
                prev = v;
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
        let id = MachineId::DubSiren;
        let base = id.default_macros();
        for &v in &[0.0f32, 1.0f32] {
            let mut macros = base;
            for m in macros.iter_mut() {
                *m = v;
            }
            let mut s = DubSiren::new(&macros);
            s.trigger(1.0);
            for _ in 0..(3.0 * SAMPLE_RATE) as usize {
                let v = s.tick();
                assert!(v.is_finite(), "non-finite output at macro={v}: {v}");
            }
        }
    }
}
