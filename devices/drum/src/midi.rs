//! MIDI, reduced to what a drum module actually needs.
//!
//! A byte-at-a-time parser with running status, no allocation, no buffering
//! of anything larger than a three-byte message. Feed it bytes from wherever
//! they arrive — a UART interrupt, a USB MIDI packet, a test vector — and it
//! hands back events.
//!
//! Deliberately in the engine crate rather than the firmware, because it is
//! pure logic and therefore testable on the host. The firmware's job is to
//! get bytes out of a peripheral, nothing more.
//!
//! Event-to-engine routing is the shared generic router from
//! [`device_core::midi`]. The drum crate re-exports it so firmware, render,
//! and tests keep importing from `drum_engine::midi`.
//!
//! Routing is **one channel per track**, `TRACKS` channels, the rest unused.
//! MIDI channels are conventionally numbered **1..=16**; on the wire the
//! nibble is 0-based, so the parser's `channel` field is wire value `N-1`.
//!
//! | MIDI channel (as labelled) | wire value | routes to |
//! |----------------------------|------------|-----------|
//! | 1..=8   | 0..=7  | track 0..=7 |
//! | 9..=16  | 8..=15 | no track — NoteOn is silent, CC is ignored |
//!
//! # Notes
//!
//! * A `NoteOn` on channel `N` (wire `N-1`) plays track `N-1`
//!   *chromatically* — the note number sets the pitch. Note 60 (middle C) is
//!   the machine's macro pitch; each semitone away transposes the voice by
//!   that many (track retune relative, so the macro pitch still maps the
//!   same). Velocity 1..127 scales the voice output.
//! * A `NoteOn` with zero velocity (or a `NoteOff`) is a no-op: drum voices
//!   are one-shots, so there is nothing to release.
//!
//! # Control Changes
//!
//! `CC 7` (master gain) is global on any channel. Everything else is
//! channel-scoped: a CC on channel `N` edits track `N-1`, matching the note
//! routing. The CC map is the same flat macro index used everywhere —
//! [`CC_TRACK_BASE`] = 20, `CC (20 + idx)` sets macro `idx`:
//!
//! | CC range  | bank     | macros     |
//! |-----------|----------|------------|
//! | 20..=27   | PITCH    | 0..=7      |
//! | 28..=35   | FILTER   | 8..=15     |
//! | 36..=43   | AMP      | 16..=23    |
//! | 44..=51   | MOD      | 24..=31    |
//! | 120, 123  | panic    | all sound/notes off, any channel |
//!
//! PITCH CC 25 is the machine selector ([`crate::machines::SLOT_MACHINE`]):
//! the value is quantised over [`crate::MachineId::ALL`] and loads that
//! machine on the track, so an engine can be swapped from MIDI without a
//! program-change message.
//!
//! A CC outside those ranges is ignored. `CC 120` / `CC 123` on any channel
//! emits [`MidiEvent::Panic`], which silences the whole engine.
//!
//! # Sample-accurate notes
//!
//! [`schedule_midi`] is the main-loop variant of [`handle_midi`]. `NoteOn`
//! events are queued into the engine's [`TimedQueue`](crate::TimedQueue)
//! with a sample offset (where in the *next* audio block they should fire),
//! so the firmware can land notes where the groove box placed them instead
//! of at the next block boundary. `ControlChange` and `Panic` still apply
//! immediately — CCs are control-rate, and a panic must cut instantly.
//!
//! # CC smoothing
//!
//! Track macros are set through [`Track::set_macro_target`](crate::Track::set_macro_target),
//! not `set_macro`: the value ramps to its target at block rate (~7.5 ms
//! one-pole) inside the audio callback's control pass. A burst of CCs (a
//! knob being spun) therefore costs one coefficient recompute per macro per
//! block instead of one per message, which keeps `apply_cc` cheap enough for
//! the main loop and keeps the `expf`-heavy recomputes off the audio hot
//! path. The machine selector is exempt — it jumps instantly.

pub use device_core::midi::{
    apply_cc, handle_midi, schedule_midi, MidiEvent, MidiParser, CC_MASTER_GAIN, CC_TRACK_BASE,
    CHROMATIC_REFERENCE_NOTE,
};

/// General MIDI percussion note numbers, for convenience and documentation.
/// Engine routing is table-driven via [`crate::DrumEngine::set_note`]; these
/// are just the values a GM-style note source will send.
pub mod notes {
    /// Acoustic bass drum.
    pub const KICK: u8 = 36;
    /// Acoustic snare.
    pub const SNARE: u8 = 38;
    /// Closed hi-hat.
    pub const CLOSED_HAT: u8 = 42;
    /// Open hi-hat.
    pub const OPEN_HAT: u8 = 46;
    /// Hand clap.
    pub const CLAP: u8 = 39;
    /// High tom.
    pub const HIGH_TOM: u8 = 50;
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Run enough control passes for the CC smoother on `track` to reach its
    /// targets. 200 blocks at k = 0.0851 leaves a residual below the 1e-4
    /// snap threshold, so this converges exactly.
    fn converge_macros(e: &mut crate::DrumEngine, track: usize) {
        for _ in 0..200 {
            e.tracks[track].control();
        }
    }

    #[test]
    fn drum_map_default() {
        // Default engine note map: kick → 0, snare → 1, hat → 2.
        use crate::{DrumEngine, MachineId};
        let e = DrumEngine::new();
        assert_eq!(e.note_map[notes::KICK as usize], Some(0));
        assert_eq!(e.note_map[notes::SNARE as usize], Some(1));
        assert_eq!(e.note_map[notes::CLOSED_HAT as usize], Some(2));
        assert_eq!(e.note_map[notes::OPEN_HAT as usize], Some(3));
        assert_eq!(e.note_map[notes::CLAP as usize], Some(4));
        // Sanity: routed note triggers the right track's machine.
        let mut e = DrumEngine::new();
        assert_eq!(e.trigger_note(notes::KICK, 1.0), Some(0));
        assert_eq!(e.tracks[0].id(), MachineId::BdClassic);
    }

    #[test]
    fn router_note_on_triggers_its_channel() {
        use crate::{DrumEngine, MachineId};
        // Channel 0 → track 0. Note 60 is the chromatic reference (no
        // transpose), so the kick plays at its macro pitch.
        let mut e = DrumEngine::new();
        handle_midi(
            &mut e,
            MidiEvent::NoteOn {
                channel: 0,
                note: CHROMATIC_REFERENCE_NOTE,
                velocity: 0.8,
            },
        );
        assert!(e.tracks[0].is_active(), "kick should be sounding");
        assert_eq!(e.tracks[0].id(), MachineId::BdClassic);
        assert!(!e.tracks[1].is_active(), "channel 0 must not touch track 1");

        // A channel with no track lands silently, like the firmware expects.
        let mut e = DrumEngine::new();
        handle_midi(
            &mut e,
            MidiEvent::NoteOn {
                channel: crate::TRACKS as u8,
                note: CHROMATIC_REFERENCE_NOTE,
                velocity: 1.0,
            },
        );
        assert!(!e.is_active());
    }

    #[test]
    fn router_cc_is_channel_scoped() {
        use crate::DrumEngine;
        // One engine for the whole test — DrumEngine is ~260 KB, so a fresh
        // one per assertion would stack-overflow the 2 MB test thread.
        let mut e = DrumEngine::new();
        let mac7_track1_before = e.tracks[1].base_macros[7];

        // CC 27 = 20 + 7 → macro 7. On channel 0 it edits track 0 only.
        handle_midi(
            &mut e,
            MidiEvent::ControlChange {
                channel: 0,
                controller: CC_TRACK_BASE + 7,
                value: 0.5,
            },
        );
        // CCs ramp at block rate: the base macro must not jump yet.
        assert!(
            (e.tracks[0].base_macros[7] - 0.5).abs() > 0.3,
            "CC should not apply instantly — it is block-rate smoothed"
        );
        converge_macros(&mut e, 0);
        assert_eq!(e.tracks[0].base_macros[7], 0.5, "macro 7 on track 0");
        assert_eq!(
            e.tracks[1].base_macros[7], mac7_track1_before,
            "channel 0 must not touch track 1"
        );

        // The same CC on channel 3 edits track 3 instead.
        handle_midi(
            &mut e,
            MidiEvent::ControlChange {
                channel: 3,
                controller: CC_TRACK_BASE + 7,
                value: 0.25,
            },
        );
        converge_macros(&mut e, 3);
        assert_eq!(e.tracks[3].base_macros[7], 0.25, "macro 7 on track 3");
        assert_eq!(
            e.tracks[0].base_macros[7], 0.5,
            "channel 3 must not touch track 0"
        );

        // CC 7 = master gain, conventionally, on any channel.
        handle_midi(
            &mut e,
            MidiEvent::ControlChange {
                channel: 9,
                controller: CC_MASTER_GAIN,
                value: 0.25,
            },
        );
        assert_eq!(e.master_gain, 0.25);

        // Macro CC on a channel with no track is ignored.
        let mac0_before = e.tracks[0].base_macros[0];
        handle_midi(
            &mut e,
            MidiEvent::ControlChange {
                channel: crate::TRACKS as u8,
                controller: CC_TRACK_BASE,
                value: 1.0,
            },
        );
        assert_eq!(e.master_gain, 0.25);
        assert_eq!(
            e.tracks[0].base_macros[0], mac0_before,
            "channel beyond the track count must be ignored"
        );
    }

    #[test]
    fn cc_machine_selector_swaps_engine() {
        use crate::DrumEngine;
        let mut e = DrumEngine::new();
        assert_eq!(e.tracks[0].id(), crate::MachineId::BdClassic);

        // CC 25 = CC_TRACK_BASE + SLOT_MACHINE (PITCH slot 5). 0.5 → index 7
        // (Cp, at COUNT=15).
        handle_midi(
            &mut e,
            MidiEvent::ControlChange {
                channel: 0,
                controller: CC_TRACK_BASE + crate::machines::SLOT_MACHINE as u8,
                value: 0.5,
            },
        );
        assert_eq!(
            e.tracks[0].id(),
            crate::MachineId::Cp,
            "CC 25 must swap the engine on the track's channel"
        );

        // Channel-scoped, like every other track macro.
        handle_midi(
            &mut e,
            MidiEvent::ControlChange {
                channel: 1,
                controller: CC_TRACK_BASE + crate::machines::SLOT_MACHINE as u8,
                value: 1.0,
            },
        );
        assert_eq!(e.tracks[1].id(), crate::MachineId::SweepFx);
        assert_eq!(
            e.tracks[0].id(),
            crate::MachineId::Cp,
            "channel 1 must not touch track 0"
        );
    }

    #[test]
    fn router_panic_silences() {
        use crate::DrumEngine;
        let mut e = DrumEngine::new();
        handle_midi(
            &mut e,
            MidiEvent::NoteOn {
                channel: 0,
                note: CHROMATIC_REFERENCE_NOTE,
                velocity: 1.0,
            },
        );
        assert!(e.is_active());
        handle_midi(&mut e, MidiEvent::Panic);
        assert!(!e.is_active(), "panic should cut everything");
    }

    #[test]
    fn schedule_midi_queues_notes_and_drains_in_process() {
        use crate::{DrumEngine, BLOCK};
        let mut e = DrumEngine::new();
        assert!(schedule_midi(
            &mut e,
            MidiEvent::NoteOn {
                channel: 0,
                note: CHROMATIC_REFERENCE_NOTE,
                velocity: 0.5,
            },
            7,
        ));
        // Queued, not triggered: nothing sounds until the next process().
        assert_eq!(e.timed.len(), 1);
        assert!(
            !e.tracks[0].is_active(),
            "a queued note must not trigger until process() drains it"
        );

        let mut l = [0.0f32; BLOCK];
        let mut r = [0.0f32; BLOCK];
        e.process(&mut l, &mut r);
        assert!(e.tracks[0].is_active(), "drained note should fire");
        assert!(e.timed.is_empty(), "process() must drain the queue");
    }

    #[test]
    fn schedule_midi_applies_cc_and_panic_immediately() {
        use crate::{DrumEngine, BLOCK};
        let mut e = DrumEngine::new();

        // A CC lands immediately as a target (it still slews; the machine
        // selector and panics are the instant path).
        schedule_midi(
            &mut e,
            MidiEvent::ControlChange {
                channel: 0,
                controller: CC_TRACK_BASE + 7,
                value: 0.5,
            },
            0,
        );
        assert!(
            (e.tracks[0].base_macros[7] - 0.5).abs() > 0.3,
            "CC should be smoothed, not instant"
        );

        // A panic cuts a sounding engine instantly, even via schedule_midi —
        // an offset is irrelevant to a panic.
        assert!(schedule_midi(
            &mut e,
            MidiEvent::NoteOn {
                channel: 0,
                note: CHROMATIC_REFERENCE_NOTE,
                velocity: 1.0,
            },
            0,
        ));
        let mut l = [0.0f32; BLOCK];
        let mut r = [0.0f32; BLOCK];
        e.process(&mut l, &mut r);
        assert!(e.is_active());
        assert!(schedule_midi(&mut e, MidiEvent::Panic, 99));
        assert!(
            !e.is_active(),
            "panic must apply immediately, not at an offset"
        );
        assert!(
            e.timed.is_empty(),
            "the CC + note must not sit in the queue"
        );
    }

    #[test]
    fn cc_value_ramps_to_its_target() {
        use crate::DrumEngine;
        let mut e = DrumEngine::new();
        let before = e.tracks[0].base_macros[7];

        handle_midi(
            &mut e,
            MidiEvent::ControlChange {
                channel: 0,
                controller: CC_TRACK_BASE + 7,
                value: 0.5,
            },
        );
        // The base macro must not have jumped; it is slewing.
        assert!(
            (e.tracks[0].base_macros[7] - 0.5).abs() > 0.3,
            "CC applied instantly"
        );
        assert_eq!(
            e.tracks[0].base_macros[7], before,
            "base should be untouched mid-ramp"
        );

        // One control pass nudges it partway toward the target.
        e.tracks[0].control();
        assert!(
            e.tracks[0].base_macros[7] > before,
            "first block must move toward the target"
        );

        // Enough blocks converge to the exact target.
        converge_macros(&mut e, 0);
        assert_eq!(e.tracks[0].base_macros[7], 0.5);
    }

    #[test]
    fn direct_set_macro_cancels_a_pending_ramp() {
        use crate::DrumEngine;
        let mut e = DrumEngine::new();
        e.tracks[0].set_macro_target(7, 1.0);
        // A programmatic set mid-ramp is authoritative: the ramp stops.
        e.tracks[0].set_macro(7, 0.2);
        converge_macros(&mut e, 0);
        assert_eq!(
            e.tracks[0].base_macros[7], 0.2,
            "the pending ramp should have been cancelled"
        );
    }
}
