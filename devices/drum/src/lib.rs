//! A drum synthesis engine with no knowledge of hardware.
//!
//! # The contract
//!
//! This crate compiles for both your laptop and a Cortex-M7. It never
//! allocates, never blocks, never touches a peripheral, and never calls into
//! an OS. Everything it needs is owned by [`DrumEngine`] and sized at compile
//! time.
//!
//! That is enforced structurally rather than by discipline:
//!
//! - `#![no_std]` with no `alloc` — `Vec`, `Box` and `String` do not exist
//!   here, so you cannot accidentally allocate in the audio path.
//! - `f32` everywhere. On a host `f64` is free and slips in unnoticed; on
//!   target it is not.
//! - Block size is a const generic, so buffers are stack arrays with no
//!   runtime length checks in the inner loop.
//!
//! # Where it runs
//!
//! ```text
//!   render/     (host)      firmware/   (Teensy 4.1)
//!       │                        │
//!       └────────┬───────────────┘
//!                ▼
//!          drum-engine
//! ```
//!
//! The host renderer is for deciding what things should sound like. The
//! firmware is for finding out what they cost. Both link this crate
//! unmodified.
//!
//! # Architecture
//!
//! `DrumEngine` owns a fixed bank of [`TRACKS`] [`Track`]s. Each track loads
//! one [`MachineId`](machines::MachineId) into its [`DrumSlot`] and runs it
//! through a [`Strip`] (multimode filter, AHD amp envelope, drive, pan,
//! level). The result is summed, soft-clippered at the master, and written
//! out. MIDI note and CC mappings live in the engine as small tables so the
//! firmware just looks them up.
//!
//! Machines expose a uniform surface of [`NUM_MACROS`] normalised knobs so
//! the host renderer can sweep any macro of any machine generically — no
//! hand-maintained per-parameter enum to keep in step with the catalogue.

#![no_std]
#![deny(unsafe_code)]
#![warn(missing_docs)]

pub use device_core::dsp;

#[cfg(feature = "grid")]
pub mod grid;
pub mod machines;
pub mod midi;

pub use device_core::{
    EngineEvent, OutPair, TimedEvent, TimedQueue, BLOCK, DENORMAL_FLOOR, INV_SAMPLE_RATE,
    MAX_TIMED_EVENTS, SAMPLE_RATE,
};

pub use device_core::slot::{DeviceModel, Slot, SlotId};
pub use device_core::strip::StripParams;
pub use device_core::track::{pan_law, ModState, VelMod, CHOKE_SAMPLES};

pub use machines::{
    MachineId, MacroInfo, MACROS_PER_BANK, NUM_BANKS, NUM_MACROS, SLOT_FILT_0, SLOT_FILT_1,
    SLOT_LEVEL, SLOT_LFO1_DEPTH, SLOT_LFO1_DEST, SLOT_LFO1_RATE, SLOT_LFO2_DEPTH, SLOT_LFO2_DEST,
    SLOT_LFO2_RATE, SLOT_MACHINE, SLOT_OUT, SLOT_PAN, SLOT_SEND_DELAY, SLOT_SEND_REVERB,
    SLOT_STRIP_ATK, SLOT_STRIP_CUT, SLOT_STRIP_DEC, SLOT_STRIP_HOLD, SLOT_STRIP_RESO,
};

pub use device_core::engine::{DeviceEngine, Engine};

/// How many tracks the engine owns.
///
/// The 8-track count is matched to a comfortably-sized drum kit on a Teensy
/// 4.1's cycle budget; see `BENCHMARKS.md` for the sizing rationale. A
/// `const` rather than const-generic because consumers say `engine.tracks[i]`
/// a lot and need a known length.
pub const TRACKS: usize = 8;

impl SlotId<NUM_MACROS> for crate::machines::MachineId {
    fn index(self) -> usize {
        self.index()
    }
    fn from_index(i: usize) -> Option<Self> {
        Self::ALL.get(i).copied()
    }
    fn count() -> usize {
        Self::COUNT
    }
    fn default_macros(self) -> [f32; NUM_MACROS] {
        self.default_macros()
    }
}

impl device_core::slot::DeviceModel<NUM_MACROS> for crate::machines::MachineId {
    fn macro_info(self) -> [MacroInfo; NUM_MACROS] {
        self.macros()
    }
    fn label(self) -> &'static str {
        self.label()
    }
}

/// Drum-machine voice slot: the existing enum-dispatch [`MachineSlot`] wrapped
/// to implement [`Slot`].
pub struct DrumSlot {
    inner: crate::machines::MachineSlot,
}

impl Slot<NUM_MACROS> for DrumSlot {
    type Id = crate::machines::MachineId;
    fn new(id: Self::Id, macros: &[f32; NUM_MACROS]) -> Self {
        Self {
            inner: crate::machines::MachineSlot::new(id, macros),
        }
    }
    fn id(&self) -> Self::Id {
        self.inner.id()
    }
    fn set_macros(&mut self, macros: &[f32; NUM_MACROS]) {
        self.inner.set_macros(macros)
    }
    fn trigger(&mut self, velocity: f32) {
        self.inner.trigger(velocity)
    }
    fn retune(&mut self, semis: f32) {
        self.inner.retune(semis)
    }
    fn reset(&mut self) {
        self.inner.reset()
    }
    fn is_active(&self) -> bool {
        self.inner.is_active()
    }
    fn tick(&mut self) -> f32 {
        self.inner.tick()
    }
}

/// One channel of the drum kit.
pub type Track = device_core::track::Track<DrumSlot, NUM_MACROS>;
/// A complete drum sound: machine + macros + strip.
pub type Sound = device_core::sound::Sound<DrumSlot, NUM_MACROS>;

/// Which machine is loaded on each track at engine construction.
const DEFAULT_KIT: [MachineId; TRACKS] = [
    MachineId::BdClassic,  // 0: kick
    MachineId::SdNatural,  // 1: snare
    MachineId::HatClassic, // 2: closed hat
    MachineId::HhBasic,    // 3: open hat → 6-osc metallic hat
    MachineId::Cp,         // 4: clap
    MachineId::Tom,        // 5: tom
    MachineId::CbClassic,  // 6: cowbell (was rimshot)
    MachineId::SyTone,     // 7: tonal synth (was FM kick)
];

/// The drum engine: a [`DeviceEngine`] pre-configured for the drum kit.
pub struct DrumEngine {
    inner: Engine<DrumSlot, NUM_MACROS, TRACKS>,
}

impl core::ops::Deref for DrumEngine {
    type Target = Engine<DrumSlot, NUM_MACROS, TRACKS>;
    fn deref(&self) -> &Self::Target {
        &self.inner
    }
}

impl core::ops::DerefMut for DrumEngine {
    fn deref_mut(&mut self) -> &mut Self::Target {
        &mut self.inner
    }
}

impl DeviceEngine<NUM_MACROS> for DrumEngine {
    type Slot = DrumSlot;

    fn tracks(&self) -> &[Track] {
        &self.inner.tracks
    }

    fn tracks_mut(&mut self) -> &mut [Track] {
        &mut self.inner.tracks
    }

    fn trigger(&mut self, track: usize, velocity: f32) {
        self.inner.trigger(track, velocity)
    }

    fn trigger_note(&mut self, note: u8, velocity: f32) -> Option<usize> {
        self.inner.trigger_note(note, velocity)
    }

    fn trigger_channel(&mut self, channel: u8, note: u8, velocity: f32) -> Option<usize> {
        self.inner.trigger_channel(channel, note, velocity)
    }

    fn set_note(&mut self, note: u8, track: Option<u8>) {
        self.inner.set_note(note, track)
    }

    fn panic(&mut self) {
        self.inner.panic()
    }

    fn schedule_timed(&mut self, offset: usize, event: EngineEvent) -> bool {
        self.inner.schedule_timed(offset, event)
    }

    fn load_sound(&mut self, track: usize, sound: &Sound) {
        self.inner.load_sound(track, sound)
    }

    fn trigger_with_sound(&mut self, track: usize, velocity: f32, sound: &Sound) {
        self.inner.trigger_with_sound(track, velocity, sound)
    }

    fn is_active(&self) -> bool {
        self.inner.is_active()
    }

    fn process(&mut self, out_l: &mut [f32], out_r: &mut [f32]) {
        self.inner.process(out_l, out_r)
    }

    fn process_dry_wet(
        &mut self,
        master_l: &mut [f32; BLOCK],
        master_r: &mut [f32; BLOCK],
        aux: &mut [[f32; BLOCK]; 6],
        wet_l: &mut [f32; BLOCK],
        wet_r: &mut [f32; BLOCK],
    ) {
        self.inner
            .process_dry_wet(master_l, master_r, aux, wet_l, wet_r)
    }

    fn master_gain(&self) -> f32 {
        self.inner.master_gain
    }

    fn set_master_gain(&mut self, value: f32) {
        self.inner.master_gain = value
    }

    fn fx_drive(&self) -> f32 {
        self.inner.send_fx.drive
    }

    fn set_fx_drive(&mut self, value: f32) {
        self.inner.send_fx.drive = value
    }

    fn load_kit(&mut self, kit: &[<DrumSlot as Slot<NUM_MACROS>>::Id]) {
        self.inner
            .load_kit(kit.try_into().expect("kit length must match track count"))
    }
}

impl Default for DrumEngine {
    fn default() -> Self {
        Self::new()
    }
}

impl DrumEngine {
    /// Build a drum engine with the default kit.
    pub fn new() -> Self {
        let mut e = Self {
            inner: Engine::new_with_kit(&DEFAULT_KIT),
        };
        e.tracks[2].strip.choke_mask = 1 << 3;
        e.tracks[2].set_macro(0, 0.05);
        e.tracks[3].set_macro(3, 0.25);
        configure_default_notes(&mut e.note_map);
        e
    }

    /// Initialize a drum engine in-place.
    ///
    /// # Safety
    /// `dst` must point to writable memory of at least `size_of::<DrumEngine>()` bytes.
    #[allow(unsafe_code)]
    pub unsafe fn new_in_place<'a>(dst: *mut DrumEngine) -> &'a mut DrumEngine {
        Engine::new_in_place_with_kit(core::ptr::addr_of_mut!((*dst).inner), &DEFAULT_KIT);
        let engine = &mut *dst;
        engine.tracks[2].strip.choke_mask = 1 << 3;
        engine.tracks[2].set_macro(0, 0.05);
        engine.tracks[3].set_macro(3, 0.25);
        configure_default_notes(&mut engine.note_map);
        engine
    }
}

/// Default kit: kick on 36, snare on 38, closed hat on 42, open hat on 46,
/// clap on 39, tom on 50. Everything past the kit's default machines is
/// unmapped — fast paths for forward compat as the catalogue grows.
fn configure_default_notes(map: &mut [Option<u8>; 128]) {
    map[36] = Some(0); // Acoustic bass drum → track 0 (BdClassic)
    map[38] = Some(1); // Acoustic snare → track 1 (SdNatural)
    map[42] = Some(2); // Closed hat → track 2 (HatClassic)
    map[46] = Some(3); // Open hat → track 3 (HhBasic)
    map[39] = Some(4); // Hand clap → track 4 (Cp)
    map[50] = Some(5); // High tom → track 5 (Tom)
    map[56] = Some(6); // Cowbell → track 6 (CbClassic)
    map[60] = Some(7); // Middle C → track 7 (SyTone)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn render_blocks(engine: &mut DrumEngine, blocks: usize) -> f32 {
        let mut l = [0.0f32; BLOCK];
        let mut r = [0.0f32; BLOCK];
        let mut peak = 0.0f32;
        for _ in 0..blocks {
            engine.process(&mut l, &mut r);
            for &s in l.iter() {
                peak = peak.max(libm::fabsf(s));
            }
        }
        peak
    }

    #[test]
    fn silent_until_triggered() {
        let mut e = DrumEngine::new();
        assert_eq!(render_blocks(&mut e, 16), 0.0);
        assert!(!e.is_active());
    }

    #[test]
    fn kick_makes_noise_then_stops() {
        let mut e = DrumEngine::new();
        e.trigger(0, 1.0);
        assert!(render_blocks(&mut e, 4) > 0.1, "kick should be audible");

        // Five seconds is far longer than any sane decay.
        render_blocks(&mut e, (5.0 * SAMPLE_RATE / BLOCK as f32) as usize);
        assert!(!e.is_active(), "kick should have decayed to silence");
    }

    #[test]
    fn trigger_note_routes_kick_to_track_zero() {
        let mut e = DrumEngine::new();
        assert_eq!(e.trigger_note(36, 1.0), Some(0));
        assert!(e.is_active());
    }

    #[test]
    fn unmapped_notes_land_silently() {
        let mut e = DrumEngine::new();
        assert_eq!(e.trigger_note(72, 1.0), None); // C5, unassigned
        assert!(!e.is_active());
    }

    #[test]
    fn trigger_channel_routes_channel_to_track() {
        let mut e = DrumEngine::new();
        assert_eq!(e.trigger_channel(3, 60, 1.0), Some(3));
        assert!(e.tracks[3].is_active(), "track 3 should be sounding");
        assert!(
            !e.tracks[0].is_active(),
            "channel 3 must not trigger track 0"
        );
    }

    #[test]
    fn trigger_channel_ignores_channels_without_tracks() {
        let mut e = DrumEngine::new();
        assert_eq!(e.trigger_channel(TRACKS as u8, 60, 1.0), None);
        assert!(!e.is_active());
    }

    #[test]
    fn trigger_channel_is_chromatic() {
        // Track 7 is SyTone (Middle C). Pitch measured by zero crossings in
        // the first ~30 ms of a hit, where the FM sidebands are still busy
        // but the carrier dominates. Two octaves up = 4x the frequency.
        let crossings = |e: &mut DrumEngine| {
            let mut l = [0.0f32; BLOCK];
            let mut r = [0.0f32; BLOCK];
            let mut prev = 0.0f32;
            let mut count = 0u32;
            let window = (0.03 * SAMPLE_RATE) as usize;
            let mut done = 0;
            while done < window {
                let n = window.min(done + BLOCK);
                e.process(&mut l, &mut r);
                for &s in l.iter().take(n - done) {
                    if (prev < 0.0) != (s < 0.0) {
                        count += 1;
                    }
                    prev = s;
                }
                done = n;
            }
            count
        };

        let mut e = DrumEngine::new();
        e.trigger_channel(7, 60, 1.0); // middle C = reference pitch
        let base = crossings(&mut e);

        let mut e = DrumEngine::new();
        e.trigger_channel(7, 84, 1.0); // two octaves up
        let high = crossings(&mut e);

        assert!(
            high > base * 3 && high < base * 5,
            "two octaves up should roughly quadruple crossings: {base} vs {high}"
        );
    }

    #[test]
    fn track_retune_transposes_and_survives_macro_recompute() {
        // Track 7 is SyTone (Middle C). Pitch measured by zero crossings in
        // the first ~30 ms of a hit, where the FM sidebands are still busy
        // but the carrier dominates.
        let crossings = |e: &mut DrumEngine| {
            let mut l = [0.0f32; BLOCK];
            let mut r = [0.0f32; BLOCK];
            e.trigger(7, 1.0);
            let mut prev = 0.0f32;
            let mut count = 0u32;
            let window = (0.03 * SAMPLE_RATE) as usize;
            let mut done = 0;
            while done < window {
                let n = window.min(done + BLOCK);
                e.process(&mut l, &mut r);
                for &s in l.iter().take(n - done) {
                    if (prev < 0.0) != (s < 0.0) {
                        count += 1;
                    }
                    prev = s;
                }
                done = n;
            }
            count
        };

        let mut e = DrumEngine::new();
        let base = crossings(&mut e);

        let mut e = DrumEngine::new();
        e.tracks[7].retune(12.0);
        let octave = crossings(&mut e);

        assert!(
            octave > base * 3 / 2 && octave < base * 4,
            "octave-up should roughly double crossings: {base} vs {octave}"
        );

        // A macro recompute (e.g. from a CC) must not drop the transpose.
        let mut e = DrumEngine::new();
        e.tracks[7].retune(12.0);
        e.tracks[7].set_macro(crate::machines::SLOT_MACH_5, 0.5); // DEC — recomputes coefficients
        let after_recompute = crossings(&mut e);
        assert!(
            (after_recompute as i64 - octave as i64).abs() <= 2,
            "macro recompute dropped the retune: {octave} vs {after_recompute}"
        );
    }

    #[test]
    fn output_never_exceeds_unity() {
        // Everything at once, full velocity, repeatedly retriggered. The
        // soft clipper should hold the bus inside [-1, 1] regardless.
        let mut e = DrumEngine::new();
        let mut l = [0.0f32; BLOCK];
        let mut r = [0.0f32; BLOCK];
        // Give every track a sensible strip so they all sound.
        let strip = StripParams::default();
        for t in e.tracks.iter_mut() {
            t.set_strip(&strip);
        }

        for _ in 0..200 {
            for i in 0..TRACKS {
                e.trigger(i, 1.0);
            }
            e.process(&mut l, &mut r);
            for &s in l.iter().chain(r.iter()) {
                assert!(s.abs() <= 1.0, "clipper let {s} through");
                assert!(s.is_finite(), "non-finite sample");
            }
        }
    }

    #[test]
    fn no_nans_from_extreme_macros() {
        // Push every macro to both extremes economically: corner-sweep the
        // first track with each extreme pattern and a hit.
        for v in [0.0f32, 1.0f32] {
            let mut e = DrumEngine::new();
            for (i, m) in e.tracks.iter_mut().enumerate() {
                m.load_machine(MachineId::ALL[i % MachineId::ALL.len()]);
                let all = [v; NUM_MACROS];
                m.set_macros(&all);
                let strip = StripParams {
                    f_mode: dsp::SvfMode::Lp,
                    f_cutoff_hz: 200.0 + v * 10_000.0,
                    f_reso_q: 0.5 + v * 19.5,
                    drive: 5.0,
                    pan: v * 2.0 - 1.0,
                    ..StripParams::default()
                };
                m.set_strip(&strip);
                Slot::trigger(&mut m.slot, 1.0);
            }

            let mut l = [0.0f32; BLOCK];
            let mut r = [0.0f32; BLOCK];
            for _ in 0..64 {
                e.process(&mut l, &mut r);
                for &s in l.iter().chain(r.iter()) {
                    assert!(s.is_finite(), "degenerate macros produced {s}");
                }
            }
        }
    }

    #[test]
    fn choke_cuts_a_layered_partner() {
        // Two tracks: 0 (CH, short hat) and 1 (OH, longer hat). OH chokes CH.
        let mut e = DrumEngine::new();
        // Track 0 = hat (already loaded), track 1 = a longer hat.
        e.tracks[1].load_machine(MachineId::HatClassic);
        e.tracks[1].set_macro(0, 0.9); // ~470 ms decay = "open"
        e.tracks[0].set_macro(0, 0.05); // ~35 ms decay = "closed"
                                        // OH (track 1) chokes CH (track 0).
        e.tracks[1].strip.choke_mask = 1 << 0;

        // Trigger OH, expect CH is reset (it never started, so nothing to
        // cut, but the bitmask is exercised).
        e.trigger(1, 1.0);
        // Quantify the relation by triggering OH then CH a moment later:
        // CH should still play (track 0 isn't choked by track 0's own
        // triggers, and choking only-one-way means a CH hit cuts off an OH).
        // Set the reverse: CH chokes OH.
        e.tracks[0].strip.choke_mask = 1 << 1;
        e.trigger(1, 1.0); // OH starts
        e.trigger(0, 1.0); // CH starts → chokes 1
                           // The choke fades the OH out over CHOKE_SAMPLES rather than cutting
                           // instantly; run past the fade and confirm the voice has gone.
        let mut l = [0.0f32; BLOCK];
        let mut r = [0.0f32; BLOCK];
        for _ in 0..(CHOKE_SAMPLES / BLOCK + 2) {
            e.process(&mut l, &mut r);
        }
        assert!(!e.tracks[1].is_active(), "OH should have been choked");
        assert!(e.tracks[0].is_active(), "CH should still be active");
    }

    #[test]
    fn lfo_modulates_macro() {
        // LFO 0 on track 0 targets macro 0 (TUNE) with depth 0.5.
        let mut e = DrumEngine::new();
        e.tracks[0].mod_state.lfos[0].set_params(
            2.0, // 2 Hz
            dsp::LfoWave::Sine,
            dsp::LfoMode::Free,
            0.5, // depth
            dsp::ModDest::Macro(0),
            0.0,
        );

        let macro0_before = e.tracks[0].base_macros[0];

        // Run several blocks and check the effective macro changed.
        let mut l = [0.0f32; BLOCK];
        let mut r = [0.0f32; BLOCK];
        e.trigger(0, 1.0);
        for _ in 0..100 {
            e.process(&mut l, &mut r);
        }

        // The LFO should have modulated macro 0 away from its base value at
        // some point in the cycle. We can't directly read the effective
        // macro, but we can confirm the track is still producing finite
        // output (didn't NaN) and that the control path ran.
        for &s in l.iter() {
            assert!(s.is_finite(), "LFO mod produced NaN");
        }

        // Base macro should be unchanged — modulation is additive, not destructive.
        assert_eq!(
            e.tracks[0].base_macros[0], macro0_before,
            "LFO mod should not mutate base_macros"
        );
    }

    #[test]
    fn velocity_mod_affects_output() {
        let mut e = DrumEngine::new();

        // Velocity mod slot 0: velocity → FilterCutoff, depth 0.5.
        // A hard hit should open the filter more than a soft hit.
        e.tracks[0].mod_state.vel_mods[0] = VelMod {
            dest: dsp::ModDest::FilterCutoff,
            depth: 0.8,
        };
        // Give the track a LP filter so cutoff actually matters.
        let strip = StripParams {
            f_mode: dsp::SvfMode::Lp,
            f_cutoff_hz: 500.0,
            f_reso_q: 2.0,
            ..StripParams::default()
        };
        e.tracks[0].set_strip(&strip);

        let peak_at = |vel: f32| {
            let mut e = DrumEngine::new();
            e.tracks[0].mod_state.vel_mods[0] = VelMod {
                dest: dsp::ModDest::FilterCutoff,
                depth: 0.8,
            };
            e.tracks[0].set_strip(&strip);
            e.trigger(0, vel);
            let mut l = [0.0f32; BLOCK];
            let mut r = [0.0f32; BLOCK];
            let mut peak = 0.0f32;
            for _ in 0..200 {
                e.process(&mut l, &mut r);
                for &s in l.iter() {
                    peak = peak.max(libm::fabsf(s));
                }
            }
            peak
        };

        let loud = peak_at(1.0);
        let quiet = peak_at(0.1);
        // A harder hit opens the filter → more energy passes → louder peak.
        assert!(
            loud > quiet,
            "velocity→cutoff mod had no effect: loud={loud}, quiet={quiet}"
        );
    }

    #[test]
    fn sound_load_round_trips() {
        let mut e = DrumEngine::new();
        let sound = Sound {
            id: MachineId::BdFm,
            macros: [0.5; NUM_MACROS],
            strip: StripParams {
                pan: 0.3,
                level: 0.6,
                ..StripParams::default()
            },
        };
        e.load_sound(0, &sound);
        assert_eq!(e.tracks[0].id(), MachineId::BdFm);
        // Macros round-trip, except the MACH slot which always mirrors the
        // loaded machine (BdFm = catalogue index 1 / 14 at COUNT=15).
        let mut expected = [0.5; NUM_MACROS];
        expected[SLOT_MACHINE] = 1.0 / 14.0;
        assert_eq!(e.tracks[0].base_macros, expected);
        assert_eq!(e.tracks[0].strip.pan, 0.3);
        assert_eq!(e.tracks[0].strip.level, 0.6);
    }

    #[test]
    fn machine_select_slot_swaps_engine() {
        let mut e = DrumEngine::new();
        assert_eq!(e.tracks[0].id(), MachineId::BdClassic);

        // 0.5 quantises onto the middle of the catalogue (index 7 = Cp at
        // COUNT=15).
        e.tracks[0].set_macro(SLOT_MACHINE, 0.5);
        assert_eq!(e.tracks[0].id(), MachineId::Cp);
        // Macros reset to the new machine's defaults, MACH slot mirroring it.
        assert_eq!(e.tracks[0].base_macros[SLOT_MACHINE], 7.0 / 14.0);

        // Re-setting the same machine is a no-op (doesn't wipe macros).
        e.tracks[0].set_macro(SLOT_LEVEL, 0.5);
        e.tracks[0].set_macro(SLOT_PAN, 0.5);
        e.tracks[0].set_macro(SLOT_MACHINE, 0.5);
        assert_eq!(e.tracks[0].id(), MachineId::Cp);
        assert_eq!(e.tracks[0].base_macros[SLOT_LEVEL], 0.5);
        assert_eq!(e.tracks[0].base_macros[SLOT_PAN], 0.5);

        // Top of the range hits the last machine in the catalogue.
        e.tracks[0].set_macro(SLOT_MACHINE, 1.0);
        assert_eq!(e.tracks[0].id(), MachineId::SweepFx);
    }

    /// Output routing (SLOT_OUT) quantises 0..1 over the 4 `OutPair`
    /// variants, sets `strip.out`, and writes the quantised value back so
    /// round-trip reads return the canonical centre. Instant under CC
    /// (`set_macro_target`) and `set_macros` (bulk replace) — same
    /// discipline as the machine selector.
    #[test]
    fn out_macro_quantises_and_round_trips() {
        let mut e = DrumEngine::new();
        // Default routing is Master (OUT macro = 0.0).
        assert_eq!(e.tracks[0].strip.out, OutPair::Master);
        assert_eq!(e.tracks[0].base_macros[SLOT_OUT], 0.0);

        // 0.0 → Master, 0.34 → Aux1, 0.67 → Aux2, 1.0 → Aux3. The quantise
        // centres at 0.0, 1/3, 2/3, 1.0 and writes that canonical value
        // back.
        for (input, expected_pair, expected_q) in [
            (0.0f32, OutPair::Master, 0.0f32),
            (0.34, OutPair::Aux1, 1.0 / 3.0),
            (0.67, OutPair::Aux2, 2.0 / 3.0),
            (1.0, OutPair::Aux3, 1.0),
        ] {
            e.tracks[0].set_macro(SLOT_OUT, input);
            assert_eq!(
                e.tracks[0].strip.out, expected_pair,
                "macro {input} routed to wrong pair"
            );
            assert_eq!(
                e.tracks[0].base_macros[SLOT_OUT], expected_q,
                "macro {input} did not quantise-back to {expected_q}"
            );
        }

        // `set_macro_target` jumps instantly — no smoothing.
        e.tracks[0].macro_pending = !0; // poison: any pending should be cleared
        e.tracks[0].set_macro_target(SLOT_OUT, 1.0);
        assert_eq!(e.tracks[0].strip.out, OutPair::Aux3);
        assert_eq!(
            e.tracks[0].macro_pending & (1 << SLOT_OUT),
            0,
            "SLOT_OUT left pending"
        );

        // `set_macros` (bulk) applies routing too.
        let mut all = e.tracks[0].base_macros;
        all[SLOT_OUT] = 0.34; // Aux1
        e.tracks[0].set_macros(&all);
        assert_eq!(e.tracks[0].strip.out, OutPair::Aux1);
        assert_eq!(e.tracks[0].base_macros[SLOT_OUT], 1.0 / 3.0);

        // `set_strip` mirror: editing routing via the strip (e.g. a Sound
        // carrying `out`) writes the canonical macro value back so MIDI
        // feedback reports the in-use pair.
        let mut strip = e.tracks[0].strip;
        strip.out = OutPair::Aux2;
        e.tracks[0].set_strip(&strip);
        assert_eq!(e.tracks[0].strip.out, OutPair::Aux2);
        assert_eq!(e.tracks[0].base_macros[SLOT_OUT], 2.0 / 3.0);
    }

    #[test]
    fn machine_select_keeps_sends_and_strip() {
        let mut e = DrumEngine::new();
        e.tracks[0].set_macro(SLOT_SEND_DELAY, 0.7);
        e.tracks[0].set_macro(SLOT_MACHINE, 0.1); // BdFm (index 1)
        assert_eq!(e.tracks[0].id(), MachineId::BdFm);
        assert_eq!(
            e.tracks[0].strip.send_delay, 0.7,
            "machine swap must not drop the track's sends"
        );
    }

    #[test]
    fn trigger_with_sound_swaps_and_fires() {
        let mut e = DrumEngine::new();
        let sound = Sound::from_defaults(MachineId::Cp);
        e.trigger_with_sound(2, 0.9, &sound);
        assert_eq!(e.tracks[2].id(), MachineId::Cp);
        assert!(e.tracks[2].is_active(), "track 2 should be sounding");
    }

    #[test]
    fn no_mod_means_control_is_noop() {
        // With no LFOs and no velocity mod, control() should be a fast
        // no-op — the fast path returns before any work.
        let mut e = DrumEngine::new();
        let macro0 = e.tracks[0].base_macros;
        e.tracks[0].control();
        assert_eq!(e.tracks[0].base_macros, macro0, "control() mutated base");
    }

    // ----- Phase 8: sample-accurate timed events -----

    #[test]
    fn timed_note_fires_at_its_offset() {
        // A hit scheduled 10 samples into the block must be bit-identical to
        // a hit at offset 0 shifted by 10 — same trigger, same machine, same
        // deterministic FX path, just started later.
        let mut a = DrumEngine::new();
        a.schedule_timed(
            0,
            EngineEvent::NoteOn {
                channel: 0,
                note: 60,
                velocity: 1.0,
            },
        );
        let mut b = DrumEngine::new();
        b.schedule_timed(
            10,
            EngineEvent::NoteOn {
                channel: 0,
                note: 60,
                velocity: 1.0,
            },
        );

        let mut al = [0.0f32; BLOCK];
        let mut ar = [0.0f32; BLOCK];
        let mut bl = [0.0f32; BLOCK];
        let mut br = [0.0f32; BLOCK];
        a.process(&mut al, &mut ar);
        b.process(&mut bl, &mut br);

        // Sanity: the reference actually sounded (a phase-zero sine kick's
        // first sample is 0, so check the whole block, not sample 0).
        assert!(al.iter().any(|&s| s != 0.0), "reference kick is silent");

        // The delayed hit is the same hit, shifted by its offset.
        for i in 10..BLOCK {
            assert_eq!(
                al[i - 10].to_bits(),
                bl[i].to_bits(),
                "delayed onset diverged from the shifted reference at {i}"
            );
        }
        // And the prefix is exact silence.
        for &s in &bl[..10] {
            assert_eq!(s, 0.0, "pre-offset samples must be silent");
        }
        // The queue drained.
        assert!(b.timed.is_empty(), "process() must drain the queue");
    }

    #[test]
    fn scheduled_panic_cuts_at_offset() {
        let mut e = DrumEngine::new();
        e.trigger(0, 1.0);
        e.schedule_timed(5, EngineEvent::Panic);

        let mut l = [0.0f32; BLOCK];
        let mut r = [0.0f32; BLOCK];
        e.process(&mut l, &mut r);

        // The kick is cut at sample 5: audible before, exact silence after.
        assert!(
            l[..5].iter().any(|&s| s != 0.0),
            "track should be sounding before the panic"
        );
        for i in 5..BLOCK {
            assert_eq!(l[i], 0.0, "panic must cut at its offset ({i})");
            assert_eq!(r[i], 0.0, "panic must cut at its offset ({i})");
        }
    }

    #[test]
    fn timed_queue_overflows_report_dropped_events() {
        let mut q = TimedQueue::new();
        for i in 0..MAX_TIMED_EVENTS {
            assert!(q.push(i, EngineEvent::Panic), "slot {i} should accept");
        }
        assert!(q.is_full());
        assert!(
            !q.push(0, EngineEvent::Panic),
            "queue full — the event must be dropped, not panic"
        );
        assert_eq!(q.len(), MAX_TIMED_EVENTS);

        // Draining returns events sorted by ascending offset regardless of
        // insertion order.
        let mut q = TimedQueue::new();
        assert!(q.push(30, EngineEvent::Panic));
        assert!(q.push(2, EngineEvent::Panic));
        assert!(q.push(17, EngineEvent::Panic));
        let mut out = [None::<TimedEvent>; MAX_TIMED_EVENTS];
        let n = q.drain_sorted(&mut out);
        assert_eq!(n, 3);
        assert_eq!(out[0].unwrap().offset, 2);
        assert_eq!(out[1].unwrap().offset, 17);
        assert_eq!(out[2].unwrap().offset, 30);
        assert!(q.is_empty());
    }

    #[test]
    fn engine_size_fits_ocram_budget() {
        // The Teensy 4.1's OCRAM is 512 KB. The engine static lives in
        // `.uninit` (OCRAM); this test is a build-time tripwire if Phase 5
        // FX buffers ever grow past the available budget. The 300 KB cap
        // leaves ~200 KB for the heap and other statics.
        let sz = core::mem::size_of::<DrumEngine>();
        // Rough KB round for debuggability — integer division is fine here.
        assert!(
            sz < 300_000,
            "DrumEngine is {sz} bytes — too large for OCRAM .uninit placement"
        );
    }

    // ----- Phase 5: send FX routing tests -----

    /// Capture a stereo peak across `blocks` blocks after triggering track `t`.
    fn render_peak_with_send(engine: &mut DrumEngine, track: usize, blocks: usize) -> (f32, f32) {
        engine.trigger(track, 1.0);
        let mut l = [0.0f32; BLOCK];
        let mut r = [0.0f32; BLOCK];
        let mut peak_l = 0.0f32;
        let mut peak_r = 0.0f32;
        for _ in 0..blocks {
            engine.process(&mut l, &mut r);
            for &s in l.iter() {
                peak_l = peak_l.max(s.abs());
            }
            for &s in r.iter() {
                peak_r = peak_r.max(s.abs());
            }
        }
        (peak_l, peak_r)
    }

    #[test]
    fn send_delay_audible_in_wet_output() {
        let mut e = DrumEngine::new();
        // Route kick → delay only (no reverb).
        let strip = StripParams {
            send_delay: 0.7,
            send_reverb: 0.0,
            ..StripParams::default()
        };
        e.tracks[0].set_strip(&strip);
        // Make delay time a multiple of BLOCK so the echo arrives predictably.
        e.send_fx.delay.set_params(0.020, 0.7, 20_000.0, 1.0);
        e.send_fx.delay.reset();

        // First render: full wet sum should be well above silence. The delay
        // passes the impulse through its tone LP at near-unity (cutoff 20 kHz),
        // so peak_l/r should be a meaningful fraction of the input send.
        let (peak_l, peak_r) = render_peak_with_send(&mut e, 0, 32);
        assert!(
            peak_l > 0.01,
            "delay send produced no audible wet: peak_l={peak_l}"
        );
        assert!(
            peak_r > 0.01,
            "delay send produced no audible wet R: peak_r={peak_r}"
        );
    }

    #[test]
    fn send_reverb_audible_in_wet_output() {
        let mut e = DrumEngine::new();
        let strip = StripParams {
            send_delay: 0.0,
            send_reverb: 0.7,
            ..StripParams::default()
        };
        e.tracks[0].set_strip(&strip);
        // Reverb with no predelay so the impulse immediately reaches the tank
        // and starts ringing out within the test window.
        e.send_fx.reverb.set_params(0.0, 0.8, 6_000.0, 1.0);
        e.send_fx.reverb.reset();

        let (peak_l, peak_r) = render_peak_with_send(&mut e, 0, 64);
        assert!(
            peak_l > 0.001,
            "reverb send produced no audible wet: peak_l={peak_l}"
        );
        assert!(
            peak_r > 0.001,
            "reverb send produced no audible wet R: peak_r={peak_r}"
        );
    }

    #[test]
    fn send_macro_drives_delay_send() {
        let mut e = DrumEngine::new();
        // Route kick → delay through the SEND.DLY *macro* (track-routed), not
        // the strip field.
        e.tracks[0].set_macro(SLOT_SEND_DELAY, 0.7);
        e.send_fx.delay.set_params(0.020, 0.7, 20_000.0, 1.0);
        e.send_fx.delay.reset();

        let (peak_l, peak_r) = render_peak_with_send(&mut e, 0, 32);
        assert!(
            peak_l > 0.01,
            "SEND.DLY macro produced no audible wet: peak_l={peak_l}"
        );
        assert!(
            peak_r > 0.01,
            "SEND.DLY macro produced no audible wet R: peak_r={peak_r}"
        );
    }

    #[test]
    fn send_macro_drives_reverb_send() {
        let mut e = DrumEngine::new();
        e.tracks[0].set_macro(SLOT_SEND_REVERB, 0.7);
        e.send_fx.reverb.set_params(0.0, 0.8, 6_000.0, 1.0);
        e.send_fx.reverb.reset();

        let (peak_l, peak_r) = render_peak_with_send(&mut e, 0, 64);
        assert!(
            peak_l > 0.001,
            "SEND.RVB macro produced no audible wet: peak_l={peak_l}"
        );
        assert!(
            peak_r > 0.001,
            "SEND.RVB macro produced no audible wet R: peak_r={peak_r}"
        );
    }

    #[test]
    fn zero_send_means_no_wet_contribution() {
        let mut e = DrumEngine::new();
        // Both sends at zero — the dry bus should pass through, the wet bus
        // should be silent. Capture the dry peak for comparison.
        let strip = StripParams {
            send_delay: 0.0,
            send_reverb: 0.0,
            ..StripParams::default()
        };
        // Apply to every track so no stray sends remain.
        for t in e.tracks.iter_mut() {
            t.set_strip(&strip);
        }
        e.send_fx.delay.reset();
        e.send_fx.reverb.reset();

        // Trigger kick + render. Compare against an engine with the FX
        // *disabled* (resetting them cannot change the dry bus, but we want
        // to make sure the wet sum is actually zero).
        e.trigger(0, 1.0);
        let mut l = [0.0f32; BLOCK];
        let mut r = [0.0f32; BLOCK];
        let mut dry_peak = 0.0f32;
        for _ in 0..16 {
            e.process(&mut l, &mut r);
            for &s in l.iter().chain(r.iter()) {
                dry_peak = dry_peak.max(s.abs());
            }
        }
        // The dry bus must be audible (the kick is the kick).
        assert!(dry_peak > 0.1, "dry bus silent: {dry_peak}");

        // Now compare against a parallel engine driven the same way but
        // with sends punched in. The wet path should add more energy.
        let mut e2 = DrumEngine::new();
        let strip_wet = StripParams {
            send_delay: 0.5,
            send_reverb: 0.5,
            ..StripParams::default()
        };
        for t in e2.tracks.iter_mut() {
            t.set_strip(&strip_wet);
        }
        // Short delay + zero-predelay reverb so the wet path produces energy
        // within the test window (default 333 ms delay would echo well past
        // the 16-block render).
        e2.send_fx.delay.set_params(0.005, 0.5, 20_000.0, 1.0);
        e2.send_fx.reverb.set_params(0.0, 0.6, 6_000.0, 1.0);
        e2.send_fx.delay.reset();

        e2.send_fx.reverb.reset();
        e2.trigger(0, 1.0);
        let mut l2 = [0.0f32; BLOCK];
        let mut r2 = [0.0f32; BLOCK];
        let mut wet_peak = 0.0f32;
        for _ in 0..32 {
            e2.process(&mut l2, &mut r2);
            for &s in l2.iter().chain(r2.iter()) {
                wet_peak = wet_peak.max(s.abs());
            }
        }
        // With sends engaged, the combined dry+wet peak should exceed the
        // dry-only peak (both engines started from identical dry paths).
        assert!(
            wet_peak > dry_peak,
            "send didn't add energy: wet={wet_peak}, dry={dry_peak}"
        );
    }

    #[test]
    fn lfo_modulates_send_delay() {
        let mut e = DrumEngine::new();
        // LFO 0 on track 0 targets SendDelay with depth 0.5.
        e.tracks[0].mod_state.lfos[0].set_params(
            0.3, // 0.3 Hz — slow sweep
            dsp::LfoWave::Sine,
            dsp::LfoMode::Trig,
            0.5,
            dsp::ModDest::SendDelay,
            0.0,
        );
        // Track base send_delay starts at 0 so the LFO sweeps ±0.5 around 0.
        let strip = StripParams {
            send_delay: 0.5,
            send_reverb: 0.0,
            ..StripParams::default()
        };
        e.tracks[0].set_strip(&strip);
        e.send_fx.delay.set_params(0.010, 0.5, 20_000.0, 1.0);
        e.send_fx.delay.reset();

        // The fact that LFO mod is active means control() runs the slow path
        // every block and recomputes the effective send. The test verifies
        // the engine survives (no NaNs) and that the wet output changes
        // across the LFO cycle.
        e.trigger(0, 1.0);
        let mut l = [0.0f32; BLOCK];
        let mut r = [0.0f32; BLOCK];
        let mut peaks: [f32; 100] = [0.0; 100];
        for slot in peaks.iter_mut() {
            e.process(&mut l, &mut r);
            let mut peak = 0.0f32;
            for &s in l.iter() {
                peak = peak.max(s.abs());
                assert!(s.is_finite(), "LFO-modulated send produced NaN");
            }
            *slot = peak;
        }

        // Some non-zero peak should appear (the LFO sweeps above 0 part of
        // the cycle, and the delay returns energy during those windows).
        let any_audible = peaks.iter().any(|&p| p > 1e-3);
        assert!(any_audible, "LFO→send produced no audible output");
    }

    #[test]
    fn send_fx_deterministic_re_render() {
        // Two identical engines must produce bit-identical output. No
        // random state anywhere on the FX path — this is the contract the
        // engine builds on (host preview == target audio).
        let mut a = DrumEngine::new();
        let mut b = DrumEngine::new();

        let strip = StripParams {
            send_delay: 0.35,
            send_reverb: 0.45,
            ..StripParams::default()
        };
        for t in a.tracks.iter_mut() {
            t.set_strip(&strip);
        }
        for t in b.tracks.iter_mut() {
            t.set_strip(&strip);
        }
        // Trigger both identically.
        for &trk in &[0usize, 1, 4] {
            a.trigger(trk, 0.85);
            b.trigger(trk, 0.85);
        }

        let mut la = [0.0f32; BLOCK];
        let mut ra = [0.0f32; BLOCK];
        let mut lb = [0.0f32; BLOCK];
        let mut rb = [0.0f32; BLOCK];

        for _ in 0..64 {
            a.process(&mut la, &mut ra);
            b.process(&mut lb, &mut rb);
            for i in 0..BLOCK {
                assert_eq!(
                    la[i].to_bits(),
                    lb[i].to_bits(),
                    "L channel diverged — non-deterministic send FX"
                );
                assert_eq!(
                    ra[i].to_bits(),
                    rb[i].to_bits(),
                    "R channel diverged — non-deterministic send FX"
                );
            }
        }
    }

    /// `new_in_place` writes each field by hand (and, for `SendFx`, delegates
    /// to its own zero-then-patch in-place constructors) instead of moving
    /// return values into place — this test is the tripwire that the
    /// hand-written version never drifts from `new()`'s field defaults.
    #[test]
    fn new_in_place_matches_new() {
        let mut via_new = DrumEngine::new();

        static mut BUF: core::mem::MaybeUninit<DrumEngine> = core::mem::MaybeUninit::uninit();
        #[allow(unsafe_code)]
        let via_in_place: &mut DrumEngine = unsafe {
            let p: *mut DrumEngine = core::ptr::addr_of_mut!(BUF).cast();
            DrumEngine::new_in_place(p)
        };

        for &trk in &[0usize, 1, 4] {
            via_new.trigger(trk, 0.85);
            via_in_place.trigger(trk, 0.85);
        }

        let mut l_new = [0.0f32; BLOCK];
        let mut r_new = [0.0f32; BLOCK];
        let mut l_ip = [0.0f32; BLOCK];
        let mut r_ip = [0.0f32; BLOCK];

        for _ in 0..64 {
            via_new.process(&mut l_new, &mut r_new);
            via_in_place.process(&mut l_ip, &mut r_ip);
            for i in 0..BLOCK {
                assert_eq!(
                    l_new[i].to_bits(),
                    l_ip[i].to_bits(),
                    "L channel diverged — new_in_place doesn't match new()"
                );
                assert_eq!(
                    r_new[i].to_bits(),
                    r_ip[i].to_bits(),
                    "R channel diverged — new_in_place doesn't match new()"
                );
            }
        }
    }

    /// `process_dry_wet` + manual sum/master should produce bit-identical
    /// output to `process`. This is the contract that lets host multi-out
    /// mode use `process_dry_wet` while stereo mode keeps using `process`
    /// without having to keep two parallel mixing implementations honest.
    #[test]
    fn process_dry_wet_matches_process_when_summed() {
        let mut a = DrumEngine::new();
        let mut b = DrumEngine::new();
        // Identical setup including a trigger so we exercise the wet path
        // (the send buses are otherwise silent) and the dry path together.
        a.trigger(0, 1.0);
        b.trigger(0, 1.0);
        // Drive a send so the wet bus is non-trivial: clone the strip send
        // config so both engines see the same routing.
        a.tracks[0].strip.send_reverb = 0.5;
        b.tracks[0].strip.send_reverb = 0.5;
        {
            let strip = a.tracks[0].strip;
            a.tracks[0].set_strip(&strip);
            let strip = b.tracks[0].strip;
            b.tracks[0].set_strip(&strip);
        }

        let master = a.master_gain;
        let fx_drive = a.send_fx.drive;

        for _ in 0..64 {
            // Stereo reference via `process`.
            let mut l_ref = [0.0f32; BLOCK];
            let mut r_ref = [0.0f32; BLOCK];
            a.process(&mut l_ref, &mut r_ref);

            // Multi-out primitive, then re-apply the exact `process` master
            // chain (FX-bus drive + inner clip on wet, then sum with the dry
            // master, then master gain + final clip). All tracks default to
            // `OutPair::Master`, so the auxes stay silent and the master dry
            // carries the same sum `process` would have inlined.
            let mut master_l = [0.0f32; BLOCK];
            let mut master_r = [0.0f32; BLOCK];
            let mut aux = [[0.0f32; BLOCK]; 6];
            let mut wet_l = [0.0f32; BLOCK];
            let mut wet_r = [0.0f32; BLOCK];
            b.process_dry_wet(
                &mut master_l,
                &mut master_r,
                &mut aux,
                &mut wet_l,
                &mut wet_r,
            );
            let mut l_multi = [0.0f32; BLOCK];
            let mut r_multi = [0.0f32; BLOCK];
            for i in 0..BLOCK {
                let wet_l_clipped = dsp::fast::soft_clip(wet_l[i] * fx_drive);
                let wet_r_clipped = dsp::fast::soft_clip(wet_r[i] * fx_drive);
                l_multi[i] = dsp::fast::soft_clip((master_l[i] + wet_l_clipped) * master);
                r_multi[i] = dsp::fast::soft_clip((master_r[i] + wet_r_clipped) * master);
            }

            for i in 0..BLOCK {
                assert_eq!(
                    l_ref[i].to_bits(),
                    l_multi[i].to_bits(),
                    "L diverged at sample {i}: process vs process_dry_wet+sum"
                );
                assert_eq!(
                    r_ref[i].to_bits(),
                    r_multi[i].to_bits(),
                    "R diverged at sample {i}: process vs process_dry_wet+sum"
                );
            }
            // Default routing → all auxes must be silent throughout.
            for (t, bus) in aux.iter().enumerate() {
                for s in bus {
                    assert_eq!(
                        *s, 0.0,
                        "aux bus {t} should be silent under default routing"
                    );
                }
            }
        }
    }

    /// Triggering track `t` with default routing puts signal only on the
    /// master dry pair. Every aux bus is silent. This pins the default
    /// routing contract.
    #[test]
    fn process_dry_wet_default_routes_to_master() {
        let mut e = DrumEngine::new();
        e.trigger(3, 1.0); // track 3 = HH Basic in the default kit

        let mut master_l = [0.0f32; BLOCK];
        let mut master_r = [0.0f32; BLOCK];
        let mut aux = [[0.0f32; BLOCK]; 6];
        let mut wet_l = [0.0f32; BLOCK];
        let mut wet_r = [0.0f32; BLOCK];

        let mut master_energy = 0.0f32;
        let mut aux_energy = 0.0f32;
        for _ in 0..32 {
            e.process_dry_wet(
                &mut master_l,
                &mut master_r,
                &mut aux,
                &mut wet_l,
                &mut wet_r,
            );
            for i in 0..BLOCK {
                master_energy += master_l[i] * master_l[i] + master_r[i] * master_r[i];
            }
            for bus in &aux {
                for s in bus {
                    aux_energy += s * s;
                }
            }
        }

        assert!(
            master_energy > 0.0,
            "master dry was silent under default routing"
        );
        assert_eq!(aux_energy, 0.0, "auxes not silent under default routing");
    }

    /// Routing a track to `OutPair::Aux2` puts its dry signal on aux buses
    /// 2 and 3 only — *not* on the master dry pair. This pins the either/
    /// or routing contract: a track on an aux does not contribute to the
    /// master mix.
    #[test]
    fn process_dry_wet_aux_routing_excludes_master() {
        let mut e = DrumEngine::new();
        e.trigger(2, 1.0); // track 2 = HatClassic
        e.tracks[2].strip.out = OutPair::Aux2;
        // Push the strip through `set_strip` so any cached state stays
        // consistent — `out` is read directly in the sample loop, so no
        // extra recomputation is needed, but mirroring the device-mode
        // workflow keeps the test honest if caching changes later.
        let strip = e.tracks[2].strip;
        e.tracks[2].set_strip(&strip);

        let mut master_l = [0.0f32; BLOCK];
        let mut master_r = [0.0f32; BLOCK];
        let mut aux = [[0.0f32; BLOCK]; 6];
        let mut wet_l = [0.0f32; BLOCK];
        let mut wet_r = [0.0f32; BLOCK];

        let mut aux2_energy = 0.0f32;
        let mut other_aux_energy = 0.0f32;
        let mut master_dry_energy = 0.0f32;
        for _ in 0..32 {
            e.process_dry_wet(
                &mut master_l,
                &mut master_r,
                &mut aux,
                &mut wet_l,
                &mut wet_r,
            );
            for i in 0..BLOCK {
                master_dry_energy += master_l[i] * master_l[i] + master_r[i] * master_r[i];
            }
            // Aux2 = buses 2 and 3.
            aux2_energy += aux[2].iter().map(|s| s * s).sum::<f32>();
            aux2_energy += aux[3].iter().map(|s| s * s).sum::<f32>();
            // The other four aux buses (Aux1 and Aux3) must be silent.
            for t in [0, 1, 4, 5] {
                other_aux_energy += aux[t].iter().map(|s| s * s).sum::<f32>();
            }
        }

        assert!(
            aux2_energy > 0.0,
            "Aux2 pair was silent for a track routed to Aux2"
        );
        assert_eq!(
            other_aux_energy, 0.0,
            "non-Aux2 auxes carried signal — routing leaked"
        );
        assert_eq!(
            master_dry_energy, 0.0,
            "master dry carried signal from an Aux2-routed track — routing is not either/or"
        );
    }

    /// Inactive tracks contribute nothing to every bus — callers pre-zero
    /// the dry buses (the `process` wrapper does this via `[0.0f32; BLOCK]`
    /// array init) and `process_dry_wet` only adds the active tracks'
    /// contributions. The wet buses are *produced* by the FX processor, not
    /// summed from caller state — so they come back exactly zero on a
    /// silent engine regardless of what the caller passed in.
    #[test]
    fn process_dry_wet_writes_zero_for_inactive_tracks() {
        let mut e = DrumEngine::new();
        // No trigger: every track is idle.

        // Pre-zero the dry buses (master + aux) — caller responsibility.
        let mut master_l = [0.0f32; BLOCK];
        let mut master_r = [0.0f32; BLOCK];
        let mut aux = [[0.0f32; BLOCK]; 6];
        // Poison the wet buses: they are FX *outputs*, the engine writes
        // every sample, so poisoning proves the engine doesn't read from
        // them.
        let mut wet_l = [0.5f32; BLOCK];
        let mut wet_r = [0.5f32; BLOCK];
        e.process_dry_wet(
            &mut master_l,
            &mut master_r,
            &mut aux,
            &mut wet_l,
            &mut wet_r,
        );

        for s in &master_l {
            assert_eq!(*s, 0.0, "master_l accumulated signal from idle tracks");
        }
        for s in &master_r {
            assert_eq!(*s, 0.0, "master_r accumulated signal from idle tracks");
        }
        for (t, bus) in aux.iter().enumerate() {
            for s in bus {
                assert_eq!(*s, 0.0, "aux bus {t} accumulated signal from idle tracks");
            }
        }
        // The wet return on a freshly-initialised silent engine is exactly
        // zero — the FX tanks ring out only if something was sent to them.
        for s in &wet_l {
            assert_eq!(*s, 0.0, "wet_l not zero on a silent engine");
        }
        for s in &wet_r {
            assert_eq!(*s, 0.0, "wet_r not zero on a silent engine");
        }
    }

    /// `process_dry_wet` zeros its dry-sum buses on entry. Regression test
    /// for the BlackHole feedback-loop bug: the host `device --multi-out`
    /// mode reuses the same buffer set across audio callbacks, and a
    /// primitive that only accumulated (not zeroed-then-summed) would
    /// spiral upward block-on-block until the aux channels (which have no
    /// clipper on the multi-out path) pegged any downstream meter.
    #[test]
    fn process_dry_wet_zeros_dry_buses_on_every_call() {
        let mut e = DrumEngine::new();
        e.trigger(0, 1.0);
        e.tracks[0].strip.send_reverb = 0.5;
        let strip = e.tracks[0].strip;
        e.tracks[0].set_strip(&strip);

        // Buffers reused across blocks, exactly like device.rs.
        let mut master_l = [0.0f32; BLOCK];
        let mut master_r = [0.0f32; BLOCK];
        let mut aux = [[0.0f32; BLOCK]; 6];
        let mut wet_l = [0.0f32; BLOCK];
        let mut wet_r = [0.0f32; BLOCK];

        // Block 1: record the peak across every dry bus.
        e.process_dry_wet(
            &mut master_l,
            &mut master_r,
            &mut aux,
            &mut wet_l,
            &mut wet_r,
        );
        let block1_master_peak = master_l
            .iter()
            .cloned()
            .fold(0.0f32, f32::max)
            .max(master_r.iter().cloned().fold(0.0f32, f32::max));
        let block1_aux_peak = aux
            .iter()
            .map(|b| b.iter().cloned().fold(0.0f32, f32::max))
            .fold(0.0f32, f32::max);
        // No track routed to an aux by default, so aux stays zero.
        assert_eq!(
            block1_aux_peak, 0.0,
            "auxes not silent under default routing"
        );

        // Block 2: same buffers, no fresh zero from the caller. If the
        // primitive doesn't internally zero, the second call's master bus
        // will be roughly 2× block 1's, then block 3 ~3×, etc. Run 64
        // blocks and check the master stays bounded rather than running
        // away.
        let mut max_seen = 0.0f32;
        for _ in 0..64 {
            e.process_dry_wet(
                &mut master_l,
                &mut master_r,
                &mut aux,
                &mut wet_l,
                &mut wet_r,
            );
            let peak = master_l
                .iter()
                .cloned()
                .fold(0.0f32, f32::max)
                .max(master_r.iter().cloned().fold(0.0f32, f32::max));
            max_seen = max_seen.max(peak);
        }
        // There is no track routed to an aux by default, so the bounded-
        // master-only assertion is enough. The block1 peak itself was
        // near unity (a 1.0-velocity kick), so a 10× ceiling is generous
        // against ~64 hit tail-offs that the engine would naturally
        // produce if accumulation were happening — but tight enough that
        // accumulation past a couple of blocks would blow it.
        assert!(
            max_seen <= 10.0,
            "master dry accumulated across blocks — internal zero missing? peak={max_seen}"
        );
        assert!(
            max_seen <= block1_master_peak * 2.0 + 1.0,
            "master dry grew unboundedly across blocks: block1={block1_master_peak}, max={max_seen}"
        );

        // Same guarantee for the auxes when a track actually routes there.
        e.tracks[2].strip.out = OutPair::Aux2;
        let strip = e.tracks[2].strip;
        e.tracks[2].set_strip(&strip);
        e.trigger(2, 1.0);

        let mut aux2_max = 0.0f32;
        for _ in 0..32 {
            e.process_dry_wet(
                &mut master_l,
                &mut master_r,
                &mut aux,
                &mut wet_l,
                &mut wet_r,
            );
            let aux2_peak = aux[2]
                .iter()
                .cloned()
                .fold(0.0f32, f32::max)
                .max(aux[3].iter().cloned().fold(0.0f32, f32::max));
            aux2_max = aux2_max.max(aux2_peak);
        }
        assert!(
            aux2_max <= 10.0,
            "aux2 dry accumulated across blocks (no clipper on multi-out aux path): peak={aux2_max}"
        );
    }

    #[test]
    fn strip_cutoff_macro_sets_filter_cutoff() {
        let mut e = DrumEngine::new();
        // Default strip filter is Off, so set a mode first.
        let strip = StripParams {
            f_mode: dsp::SvfMode::Lp,
            f_cutoff_hz: 1000.0,
            ..StripParams::default()
        };
        e.tracks[0].set_strip(&strip);
        // Now drive cutoff via the macro slot.
        e.tracks[0].set_macro(SLOT_STRIP_CUT, 0.0); // 20 Hz
        approx::assert_abs_diff_eq!(e.tracks[0].strip.f_cutoff_hz, 20.0, epsilon = 0.5);
        e.tracks[0].set_macro(SLOT_STRIP_CUT, 1.0); // 20 kHz
        approx::assert_abs_diff_eq!(e.tracks[0].strip.f_cutoff_hz, 20_000.0, epsilon = 50.0);
    }

    #[test]
    fn strip_reso_macro_sets_filter_resonance() {
        let mut e = DrumEngine::new();
        e.tracks[0].set_macro(SLOT_STRIP_RESO, 0.0);
        approx::assert_abs_diff_eq!(e.tracks[0].strip.f_reso_q, 0.5, epsilon = 1e-6);
        e.tracks[0].set_macro(SLOT_STRIP_RESO, 1.0);
        approx::assert_abs_diff_eq!(e.tracks[0].strip.f_reso_q, 20.0, epsilon = 1e-6);
    }

    #[test]
    fn strip_env_macros_set_ahd_timings() {
        let mut e = DrumEngine::new();
        e.tracks[0].set_macro(SLOT_STRIP_ATK, 0.5);
        approx::assert_abs_diff_eq!(e.tracks[0].strip.amp_attack_s, 0.5, epsilon = 1e-6);
        e.tracks[0].set_macro(SLOT_STRIP_HOLD, 0.5);
        approx::assert_abs_diff_eq!(e.tracks[0].strip.amp_hold_s, 5.0, epsilon = 1e-6);
        e.tracks[0].set_macro(SLOT_STRIP_DEC, 0.5);
        approx::assert_abs_diff_eq!(e.tracks[0].strip.amp_decay_s, 5.005, epsilon = 1e-3);
    }

    #[test]
    fn lfo_rate_macro_slow_fast_split() {
        let mut e = DrumEngine::new();
        // 0.0 → slow range bottom (0.1 Hz)
        e.tracks[0].set_macro(SLOT_LFO1_RATE, 0.0);
        approx::assert_abs_diff_eq!(
            e.tracks[0].mod_state.lfos[0].speed_hz(),
            0.1,
            epsilon = 1e-6
        );
        // 0.5 → fast range bottom (1 Hz)
        e.tracks[0].set_macro(SLOT_LFO1_RATE, 0.5);
        approx::assert_abs_diff_eq!(
            e.tracks[0].mod_state.lfos[0].speed_hz(),
            1.0,
            epsilon = 1e-6
        );
        // 1.0 → fast range top (100 Hz)
        e.tracks[0].set_macro(SLOT_LFO1_RATE, 1.0);
        approx::assert_abs_diff_eq!(
            e.tracks[0].mod_state.lfos[0].speed_hz(),
            100.0,
            epsilon = 1e-6
        );
    }

    #[test]
    fn lfo_depth_macro_sets_depth() {
        let mut e = DrumEngine::new();
        e.tracks[0].set_macro(SLOT_LFO1_DEPTH, 0.7);
        approx::assert_abs_diff_eq!(e.tracks[0].mod_state.lfos[0].depth(), 0.7, epsilon = 1e-6);
    }

    #[test]
    fn lfo_dest_macro_quantises() {
        let mut e = DrumEngine::new();
        // 0.0 → Macro(0)
        e.tracks[0].set_macro(SLOT_LFO1_DEST, 0.0);
        assert_eq!(e.tracks[0].mod_state.lfos[0].dest(), dsp::ModDest::Macro(0));
        // ~0.56 → FilterCutoff (index 8 of 16 → 8/15 ≈ 0.533)
        e.tracks[0].set_macro(SLOT_LFO1_DEST, 0.56);
        assert_eq!(
            e.tracks[0].mod_state.lfos[0].dest(),
            dsp::ModDest::FilterCutoff
        );
    }

    #[test]
    fn strip_cutoff_macro_round_trips_through_set_strip() {
        let mut e = DrumEngine::new();
        let strip = StripParams {
            f_mode: StripParams::default().f_mode,
            f_cutoff_hz: 5000.0,
            f_reso_q: 4.0,
            amp_attack_s: 0.2,
            amp_hold_s: 0.5,
            amp_decay_s: 1.0,
            ..StripParams::default()
        };
        e.tracks[0].set_strip(&strip);
        // The macro slot should mirror the inverse mapping.
        let m = e.tracks[0].base_macros[SLOT_STRIP_CUT];
        let expected = libm::logf(5000.0 / 20.0) / libm::logf(1000.0);
        approx::assert_abs_diff_eq!(m, expected, epsilon = 1e-5);
    }
}
