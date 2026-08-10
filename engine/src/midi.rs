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
//! Event-to-engine routing also lives here as [`handle_midi`] / [`apply_cc`]
//! and [`schedule_midi`], shared verbatim by the firmware (MIDI over UART +
//! USB MIDI) and the host `device` harness (MIDI over a virtual CoreMIDI
//! port) so both targets interpret the same bytes identically. Parsing bytes
//! into events is this module; deciding what an event *means* is the shared
//! router.
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

/// A parsed message the engine cares about.
#[derive(Clone, Copy, PartialEq, Debug)]
pub enum MidiEvent {
    /// Note on with velocity `0.0..=1.0`.
    NoteOn {
        /// MIDI channel, `0..=15`.
        channel: u8,
        /// MIDI note number.
        note: u8,
        /// Normalised velocity.
        velocity: f32,
    },
    /// Continuous controller.
    ControlChange {
        /// MIDI channel, `0..=15`.
        channel: u8,
        /// Controller number.
        controller: u8,
        /// Normalised value, `0.0..=1.0`.
        value: f32,
    },
    /// All notes off / all sound off.
    Panic,
}

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

// --- Shared event routing -----------------------------------------------
//
// These numbers and the two functions below are the one place that defines
// how an incoming MIDI message becomes an engine action. Firmware and the
// host `device` harness both call [`handle_midi`], so a controller or DAW
// behaves identically against the Teensy and against the Mac tuning rig.

/// MIDI note that plays a track at its macro-configured pitch. Middle C.
/// Every semitone away transposes the voice by that many semitones.
pub const CHROMATIC_REFERENCE_NOTE: u8 = 60;

/// First CC number reserved for track macros. On channel `N`, CC
/// `CC_TRACK_BASE..CC_TRACK_BASE + NUM_MACROS` sets track `N`'s macros.
pub const CC_TRACK_BASE: u8 = 20;

/// CC for the master gain. Following MIDI convention. Global on any channel.
pub const CC_MASTER_GAIN: u8 = 7;

/// Apply a MIDI event to the engine.
///
/// `NoteOn` routes through [`crate::DrumEngine::trigger_channel`]: the
/// channel picks the track, the note picks the pitch. `ControlChange`
/// reaches that channel's track-macro block or the master gain. `Panic`
/// silences everything.
pub fn handle_midi(engine: &mut crate::DrumEngine, event: MidiEvent) {
    match event {
        MidiEvent::NoteOn {
            channel,
            note,
            velocity,
        } => {
            engine.trigger_channel(channel, note, velocity);
        }
        MidiEvent::ControlChange {
            channel,
            controller,
            value,
        } => {
            apply_cc(engine, channel, controller, value);
        }
        MidiEvent::Panic => engine.panic(),
    }
}

/// Route a MIDI event to the engine, sample-accurately.
///
/// `NoteOn` events are queued as
/// [`TimedEvent`](crate::TimedEvent)s to fire `offset` samples into the
/// next [`DrumEngine::process`](crate::DrumEngine::process) block — the
/// firmware's MIDI path computes `offset` from its sample counter so notes
/// land where the groove box placed them rather than at the next block
/// boundary.
///
/// `ControlChange` and `Panic` are applied immediately via [`handle_midi`]:
/// CCs are control-rate (their coefficient recompute is block-rate smoothed,
/// see [`apply_cc`]), and a panic must cut instantly, not a block later.
///
/// Returns `true` when the event was queued/applied, `false` when the timed
/// queue was full and the note was dropped (CCs and panics always return
/// `true`).
pub fn schedule_midi(engine: &mut crate::DrumEngine, event: MidiEvent, offset: usize) -> bool {
    match event {
        MidiEvent::NoteOn {
            channel,
            note,
            velocity,
        } => engine.schedule_timed(
            offset,
            crate::EngineEvent::NoteOn {
                channel,
                note,
                velocity,
            },
        ),
        _ => {
            handle_midi(engine, event);
            true
        }
    }
}

/// Map CC numbers onto engine parameters — channel-scoped track macros +
/// master.
///
/// A CC on channel `N` edits track `N`, matching the note routing. `CC 7`
/// stays global. Macros are set through [`Track::set_macro_target`](crate::Track::set_macro_target)
/// so the value ramps to its target at block rate inside the audio
/// callback's control pass (see the module docs, "CC smoothing") — the
/// `expf`-heavy coefficient recompute happens at most once per block per
/// moving macro, which is what keeps CCs safe to handle in the main loop
/// rather than the audio interrupt. The host harness keeps the same
/// discipline (parsed on the CoreMIDI thread, applied between engine blocks).
pub fn apply_cc(engine: &mut crate::DrumEngine, channel: u8, controller: u8, value: f32) {
    if controller == CC_MASTER_GAIN {
        engine.master_gain = value;
        return;
    }

    // Channel-scoped track macros: CC 20..27 on channel N sets track N.
    if controller >= CC_TRACK_BASE {
        let macro_idx = (controller - CC_TRACK_BASE) as usize;
        let track = channel as usize;
        if track < crate::TRACKS && macro_idx < crate::NUM_MACROS {
            engine.tracks[track].set_macro_target(macro_idx, value);
        }
    }
}

/// Incremental MIDI parser.
///
/// Handles running status, which matters more than it might seem: a sequencer
/// firing dense note data will drop the status byte on repeated messages, and
/// a parser that does not track it will silently lose most of your notes.
#[derive(Default)]
pub struct MidiParser {
    status: u8,
    data: [u8; 2],
    index: usize,
    /// Channel to listen on, or `None` for omni.
    channel_filter: Option<u8>,
}

impl MidiParser {
    /// Omni-mode parser.
    pub const fn new() -> Self {
        Self {
            status: 0,
            data: [0; 2],
            index: 0,
            channel_filter: None,
        }
    }

    /// Parser listening to a single channel, `0..=15`.
    pub const fn with_channel(channel: u8) -> Self {
        Self {
            status: 0,
            data: [0; 2],
            index: 0,
            channel_filter: Some(channel),
        }
    }

    /// Push one byte. Returns an event when a message completes.
    pub fn push(&mut self, byte: u8) -> Option<MidiEvent> {
        if byte >= 0xF8 {
            // System real-time. Interleaves anywhere, including mid-message,
            // and must not disturb running status. Clock and transport would
            // be handled here if you want to sync to the groove box.
            return None;
        }

        if byte >= 0x80 {
            if byte >= 0xF0 {
                // System common cancels running status.
                self.status = 0;
                self.index = 0;
                return None;
            }
            self.status = byte;
            self.index = 0;
            return None;
        }

        if self.status == 0 {
            // Data byte with no status yet — stream started mid-message.
            return None;
        }

        self.data[self.index] = byte;
        self.index += 1;

        let expected = match self.status & 0xF0 {
            0xC0 | 0xD0 => 1, // program change, channel pressure
            _ => 2,
        };

        if self.index < expected {
            return None;
        }
        self.index = 0;

        if let Some(want) = self.channel_filter {
            if self.status & 0x0F != want {
                return None;
            }
        }

        self.decode()
    }

    fn decode(&self) -> Option<MidiEvent> {
        const INV_127: f32 = 1.0 / 127.0;

        match self.status & 0xF0 {
            0x90 => {
                let velocity = self.data[1];
                if velocity == 0 {
                    // Note-on with zero velocity is a note-off. Drums are
                    // one-shots, so there is nothing to do.
                    None
                } else {
                    Some(MidiEvent::NoteOn {
                        channel: self.status & 0x0F,
                        note: self.data[0],
                        velocity: velocity as f32 * INV_127,
                    })
                }
            }
            0x80 => None, // note-off, irrelevant for one-shots
            0xB0 => {
                let cc = self.data[0];
                // 120 = all sound off, 123 = all notes off.
                if cc == 120 || cc == 123 {
                    Some(MidiEvent::Panic)
                } else {
                    Some(MidiEvent::ControlChange {
                        channel: self.status & 0x0F,
                        controller: cc,
                        value: self.data[1] as f32 * INV_127,
                    })
                }
            }
            _ => None,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn feed(p: &mut MidiParser, bytes: &[u8]) -> heapless_vec::Events {
        let mut out = heapless_vec::Events::default();
        for &b in bytes {
            if let Some(e) = p.push(b) {
                out.push(e);
            }
        }
        out
    }

    /// Run enough control passes for the CC smoother on `track` to reach its
    /// targets. 200 blocks at k = 0.0851 leaves a residual below the 1e-4
    /// snap threshold, so this converges exactly.
    fn converge_macros(e: &mut crate::DrumEngine, track: usize) {
        for _ in 0..200 {
            e.tracks[track].control();
        }
    }

    /// Tiny fixed-capacity collector so the tests stay allocation-free too.
    mod heapless_vec {
        use super::MidiEvent;

        #[derive(Default)]
        pub struct Events {
            items: [Option<MidiEvent>; 16],
            len: usize,
        }

        impl Events {
            pub fn push(&mut self, e: MidiEvent) {
                assert!(self.len < 16, "test collector overflow");
                self.items[self.len] = Some(e);
                self.len += 1;
            }
            pub fn len(&self) -> usize {
                self.len
            }
            pub fn get(&self, i: usize) -> MidiEvent {
                self.items[i].expect("no event at index")
            }
        }
    }

    #[test]
    fn parses_a_note_on() {
        let mut p = MidiParser::new();
        let events = feed(&mut p, &[0x99, 36, 127]);
        assert_eq!(events.len(), 1);
        match events.get(0) {
            MidiEvent::NoteOn {
                channel,
                note,
                velocity,
            } => {
                assert_eq!(channel, 9, "channel must be preserved");
                assert_eq!(note, 36);
                approx::assert_abs_diff_eq!(velocity, 1.0, epsilon = 1e-6);
            }
            other => panic!("unexpected {other:?}"),
        }
    }

    #[test]
    fn control_change_carries_channel() {
        let mut p = MidiParser::new();
        let events = feed(&mut p, &[0xB2, 1, 64]); // ch2, CC 1
        assert_eq!(events.len(), 1);
        match events.get(0) {
            MidiEvent::ControlChange {
                channel,
                controller,
                value,
            } => {
                assert_eq!(channel, 2);
                assert_eq!(controller, 1);
                approx::assert_abs_diff_eq!(value, 64.0 / 127.0, epsilon = 1e-6);
            }
            other => panic!("unexpected {other:?}"),
        }
    }

    #[test]
    fn handles_running_status() {
        // One status byte, three note pairs after it.
        let mut p = MidiParser::new();
        let events = feed(&mut p, &[0x99, 36, 100, 38, 90, 42, 80]);
        assert_eq!(events.len(), 3, "running status dropped notes");
    }

    #[test]
    fn realtime_bytes_do_not_break_running_status() {
        let mut p = MidiParser::new();
        // MIDI clock (0xF8) injected mid-message, which is legal and common.
        let events = feed(&mut p, &[0x99, 36, 0xF8, 100, 0xF8, 38, 90]);
        assert_eq!(events.len(), 2, "clock bytes corrupted the stream");
    }

    #[test]
    fn zero_velocity_note_on_is_ignored() {
        let mut p = MidiParser::new();
        let events = feed(&mut p, &[0x99, 36, 0]);
        assert_eq!(events.len(), 0);
    }

    #[test]
    fn channel_filter_rejects_other_channels() {
        let mut p = MidiParser::with_channel(9);
        let on_nine = feed(&mut p, &[0x99, 36, 100]);
        assert_eq!(on_nine.len(), 1);

        let mut p = MidiParser::with_channel(9);
        let on_one = feed(&mut p, &[0x90, 36, 100]);
        assert_eq!(on_one.len(), 0);
    }

    #[test]
    fn cc_120_and_123_are_panics() {
        let mut p = MidiParser::new();
        assert_eq!(feed(&mut p, &[0xB9, 120, 0]).get(0), MidiEvent::Panic);
        let mut p = MidiParser::new();
        assert_eq!(feed(&mut p, &[0xB9, 123, 0]).get(0), MidiEvent::Panic);
    }

    #[test]
    fn data_before_status_is_discarded() {
        let mut p = MidiParser::new();
        let events = feed(&mut p, &[36, 100, 0x99, 38, 90]);
        assert_eq!(events.len(), 1);
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

        // CC 25 = CC_TRACK_BASE + SLOT_MACHINE (PITCH slot 5). 0.5 → index 5.
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
            crate::MachineId::Rs,
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
        assert_eq!(e.tracks[1].id(), crate::MachineId::SyTone);
        assert_eq!(
            e.tracks[0].id(),
            crate::MachineId::Rs,
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
