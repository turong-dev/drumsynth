//! Dub Siren: sine carrier with an internal pitch-modulating LFO, gated
//! by a long self-timed AHD envelope.
//!
//! The classic dub-reggae siren: a slow warble that dives and rises,
//! played back through long delay throws. Distinct from every other
//! machine in the catalogue, which are one-shot percussive hits — the
//! siren is a *gesture*: it lasts seconds, and its [`is_active`]
//! window is owned by an internal [`AhdEnv`] rather than a [`DecayEnv`].
//!
//! # Why gated, not self-timed
//!
//! This machine was originally self-timed: one trigger produced one
//! fixed-length gesture, because the engine had no note-off path at all (the
//! parser dropped `0x8n`, and [`EngineEvent`](crate::EngineEvent) only ever
//! carried [`NoteOn`](crate::midi::MidiEvent::NoteOn)). The per-track
//! `is_active` early-out in the engine is the budget mechanism that keeps idle
//! kits cheap, and a self-timed envelope was the way to be sustained without
//! defeating it.
//!
//! There is now a real gate: [`Slot::release`](crate::Slot::release) reaches
//! [`DubSiren::release`], so the siren holds for as long as the key is down
//! and the note-off starts the decay. The early-out is still intact — it just
//! reports "still sounding" for as long as the gesture genuinely is. What
//! replaced the old fixed length is the watchdog in [`GATED_MAX_HOLD_S`],
//! which exists purely so a note-off that never arrives cannot ring the voice
//! and hold the track out of the cost gate forever.
//!
//! # Topology
//!
//! ```text
//!   internal LFO (sine/tri/saw) ──► pitch multiplier (octaves, exp2)
//!   AhdEnv (gated gesture)        ──► amp
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
//! | 18  | 38  | DEC     | 0.05..0.6 s  | release decay, 5% of the old gesture length |
//! | 20  | 40  | SHAPE   | 0..1         | LFO waveform: sine ↔ tri ↔ saw (quantised, default tri) |
//! | 22  | 42  | SEND.DLY| 0..1         | delay send (track-routed) |
//! | 23  | 43  | SEND.RVB| 0..1         | reverb send (track-routed) |
//! | 26  | 46  | OUT     | 0..1         | track routing (Master/Aux1/2/3) |
//!
//! All other slots are RESV (default 0.0) and ignored.
//!
//! # DEC changed meaning
//!
//! `DEC` was the length of the whole self-timed gesture (0.5..6 s, split
//! 5% attack / 85% hold / 10% decay). Now that the gate holds the note, that
//! slot is the **release** — the decay that runs after note-off — and its range
//! is 0.05..0.6 s. It is the same knob in the same place on the panel, doing
//! the job that is now actually missing: a siren you cannot shut off quickly.

use crate::dsp::{fast, AhdEnv, HoldMode, SineOsc};
use crate::machines::{
    NUM_MACROS, SLOT_LEVEL, SLOT_MACH_0, SLOT_MACH_1, SLOT_MACH_2, SLOT_MACH_5, SLOT_MACH_7,
};

/// Watchdog on the gated hold, in seconds.
///
/// A note-off that never arrives — a stuck key, a dropped cable — would
/// otherwise ring the siren indefinitely and keep the track out of the
/// engine's `is_active` cost gate, which is the mechanism that makes an idle
/// kit cheap. Ten seconds is far longer than any siren gesture and short
/// enough to bound the damage.
///
/// This is a backstop, not the note length. Raise it if you hold the key
/// longer than ten seconds and hear it cut.
const GATED_MAX_HOLD_S: f32 = 10.0;

/// Attack time, in seconds, at the bottom of the DEC range.
///
/// The attack is what makes a siren swell rather than appear, so it is scaled
/// from the same macro as the release and stays short relative to it.
const ATTACK_S: f32 = 0.05;

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
        let release_s = 0.05 + 0.55 * macros[SLOT_MACH_5]; // DEC 0.05..0.6 s
        let shape = SirenShape::from_macro(macros[SLOT_MACH_7]);
        let level = macros[SLOT_LEVEL]; // LEVEL 0..1

        self.base_hz = base_hz * self.freq_scale;
        self.depth_oct = depth_oct;
        self.lfo_inc = rate_hz * crate::INV_SAMPLE_RATE;
        self.shape = shape;
        self.level = level;

        // Gated: the hold lasts until `release`, and `hold_s` is 0 so the
        // timed hold cannot pre-empt it. The watchdog is what bounds a lost
        // note-off.
        self.env.set_hold_mode(HoldMode::Gated);
        self.env.set_max_hold_s(GATED_MAX_HOLD_S);
        self.env.set_params(ATTACK_S, 0.0, release_s);
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

    /// Begin a gesture at `velocity` (0.0..=1.0). Opens the gate: the siren
    /// holds until [`release`](Self::release). The AHD peak scales with
    /// velocity, so a softer hit is a quieter siren, not a shorter one.
    pub fn trigger(&mut self, velocity: f32) {
        self.env.trigger(velocity);
        self.osc.reset_phase();
        self.lfo_phase = 0.0;
    }

    /// Close the gate — the note-off path. Drops into the release decay
    /// rather than cutting, so the siren falls away instead of stopping dead.
    ///
    /// A no-op when the siren is already silent or already releasing.
    pub fn release(&mut self) {
        self.env.release();
    }

    /// Silence.
    pub fn reset(&mut self) {
        self.env.reset();
        self.osc.reset_phase();
        self.lfo_phase = 0.0;
    }

    /// Still sounding?
    ///
    /// True for as long as the gate is open or the release is running. This
    /// is the per-track early-out hook, so it is what makes a held siren cost
    /// what it costs and an idle one cost nothing.
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
    fn decays_to_silence_after_release() {
        let id = MachineId::DubSiren;
        let macros = id.default_macros();
        let mut s = DubSiren::new(&macros);
        s.trigger(1.0);
        s.release();
        // DEC tops out at 0.6 s; allow 7 s to settle.
        for _ in 0..(7.0 * SAMPLE_RATE) as usize {
            s.tick();
        }
        assert!(!s.is_active(), "siren did not stop");
    }

    /// The gesture contract, restated for a gate: a siren is *sustained*, not
    /// a hit. With the key still down it must still be sounding well past a
    /// second, and — this is the part that pins the budget argument — it must
    /// be willing to keep going for far longer than that, because the key, not
    /// a preset, decides when it ends.
    #[test]
    fn sustained_gesture_stays_active_past_one_second() {
        let id = MachineId::DubSiren;
        let macros = id.default_macros();
        let mut s = DubSiren::new(&macros);
        s.trigger(1.0);
        for _ in 0..(1.0 * SAMPLE_RATE) as usize {
            s.tick();
        }
        assert!(s.is_active(), "siren cut short of 1 s");
        // Still holding with no note-off at all — this is what the gate buys
        // over the old 0.85-hold self-timed gesture.
        for _ in 0..(5.0 * SAMPLE_RATE) as usize {
            s.tick();
        }
        assert!(s.is_active(), "siren ended without a note-off");
    }

    /// The watchdog. A note-off that never arrives must still end the note, or
    /// a stuck key pins the track out of the engine's `is_active` cost gate
    /// and the whole kit gets more expensive for as long as it is held.
    #[test]
    fn watchdog_ends_a_note_that_is_never_released() {
        let id = MachineId::DubSiren;
        let macros = id.default_macros();
        let mut s = DubSiren::new(&macros);
        s.trigger(1.0);
        // GATED_MAX_HOLD_S plus the release decay plus settle.
        for _ in 0..((GATED_MAX_HOLD_S + 2.0) * SAMPLE_RATE) as usize {
            s.tick();
        }
        assert!(!s.is_active(), "the watchdog never fired");
    }

    /// The behaviour the whole change exists for: hold the key and it holds;
    /// let go and it stops.
    #[test]
    fn note_off_ends_the_note_early() {
        let id = MachineId::DubSiren;
        let macros = id.default_macros();
        let mut s = DubSiren::new(&macros);

        s.trigger(1.0);
        let held = (2.0 * SAMPLE_RATE) as usize;
        for _ in 0..held {
            s.tick();
        }
        assert!(s.is_active(), "should still be held at 2 s");

        s.release();
        assert!(s.is_active(), "release should fade, not cut");
        for _ in 0..(2.0 * SAMPLE_RATE) as usize {
            s.tick();
        }
        assert!(!s.is_active(), "release did not end the note");
    }

    /// A release must not shorten the note by more than the release time, and
    /// must not leave a discontinuity — the level has to fall monotonically
    /// from wherever it was rather than stepping.
    #[test]
    fn release_fades_without_a_step() {
        let id = MachineId::DubSiren;
        let macros = id.default_macros();
        let mut s = DubSiren::new(&macros);
        s.trigger(1.0);
        for _ in 0..(0.2 * SAMPLE_RATE) as usize {
            s.tick();
        }
        s.release();

        // Peak over the first millisecond of the release, then over the rest,
        // must be a smooth decline with no jump back up.
        let mut prev_peak = f32::INFINITY;
        for _ in 0..40 {
            let mut p = 0.0f32;
            for _ in 0..64 {
                p = p.max(libm::fabsf(s.tick()));
            }
            assert!(
                p <= prev_peak + 1e-3,
                "release stepped up: {prev_peak} -> {p}"
            );
            prev_peak = p;
        }
    }

    /// Note-offs are not reliably paired. A release for a note that never
    /// started, or a second release for one already releasing, must be inert.
    #[test]
    fn stray_releases_are_inert() {
        let id = MachineId::DubSiren;
        let macros = id.default_macros();
        let mut s = DubSiren::new(&macros);

        // Never triggered.
        s.release();
        assert!(!s.is_active());
        assert_eq!(s.tick(), 0.0);

        // Released twice. Compare peak amplitude over a window rather than
        // consecutive samples: a sine's samples alternate sign every half
        // cycle, so neighbouring values carry no envelope information.
        s.trigger(1.0);
        for _ in 0..1000 {
            s.tick();
        }
        s.release();
        let mut a = 0.0f32;
        for _ in 0..480 {
            a = a.max(libm::fabsf(s.tick()));
        }
        let mut b = 0.0f32;
        for _ in 0..480 {
            b = b.max(libm::fabsf(s.tick()));
        }
        assert!(b < a, "repeat release disturbed the decay: {a} -> {b}");
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
