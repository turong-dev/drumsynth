//! MIDI parser and shared event router.
//!
//! A byte-at-a-time parser with running status, no allocation, no buffering
//! of anything larger than a three-byte message. Feed it bytes from wherever
//! they arrive — a UART interrupt, a USB MIDI packet, a test vector — and it
//! hands back events.
//!
//! [`handle_midi`], [`apply_cc`] and [`schedule_midi`] are generic over
//! [`DeviceEngine`](crate::engine::DeviceEngine) so every device interprets
//! MIDI bytes the same way. Device-specific routing (for example, which macro
//! slot swaps the machine) is handled inside the engine's `set_macro_target`;
//! the router itself only knows the flat macro index and the track count.

use crate::engine::DeviceEngine;

/// MIDI note that plays a track at its macro-configured pitch. Middle C.
/// Every semitone away transposes the voice by that many semitones.
pub const CHROMATIC_REFERENCE_NOTE: u8 = 60;

/// First CC number reserved for track macros. On channel `N`, CC
/// `CC_TRACK_BASE..CC_TRACK_BASE + NUM_MACROS` sets track `N`'s macros.
pub const CC_TRACK_BASE: u8 = 20;

/// CC for the master gain. Following MIDI convention. Global on any channel.
pub const CC_MASTER_GAIN: u8 = 7;

/// Apply a MIDI event to a device engine.
///
/// `NoteOn` routes through [`DeviceEngine::trigger_channel`]: the channel
/// picks the track, the note picks the pitch. `NoteOff` routes through
/// [`DeviceEngine::release_channel`], the same channel-to-track mapping, so a
/// voice that sustains ends when the key comes up.
/// `ControlChange` reaches that channel's track-macro block or the master
/// gain. `Panic` silences everything.
pub fn handle_midi<E: DeviceEngine<N>, const N: usize>(engine: &mut E, event: MidiEvent) {
    match event {
        MidiEvent::NoteOn {
            channel,
            note,
            velocity,
        } => {
            engine.trigger_channel(channel, note, velocity);
        }
        MidiEvent::NoteOff { channel, note } => {
            engine.release_channel(channel, note);
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

/// Map CC numbers onto engine parameters — channel-scoped track macros +
/// master.
///
/// A CC on channel `N` edits track `N`, matching the note routing. `CC 7`
/// stays global. Macros are set through the track's `set_macro_target` so
/// the value ramps to its target at block rate inside the audio callback's
/// control pass.
pub fn apply_cc<E: DeviceEngine<N>, const N: usize>(
    engine: &mut E,
    channel: u8,
    controller: u8,
    value: f32,
) {
    if controller == CC_MASTER_GAIN {
        engine.set_master_gain(value);
        return;
    }

    if controller >= CC_TRACK_BASE {
        let macro_idx = (controller - CC_TRACK_BASE) as usize;
        let track = channel as usize;
        let n_tracks = engine.tracks().len();
        if track < n_tracks && macro_idx < N {
            engine.tracks_mut()[track].set_macro_target(macro_idx, value);
        }
    }
}

/// Route a MIDI event to the engine, sample-accurately.
///
/// `NoteOn` and `NoteOff` events are queued as
/// [`EngineEvent`](crate::EngineEvent)s to fire `offset` samples into the next
/// process block, so a held note's length is honoured to the sample and a
/// gate close lands where the sequencer put it. `ControlChange` and `Panic`
/// are applied immediately via [`handle_midi`].
///
/// Returns `true` when the event was queued/applied, `false` when the timed
/// queue was full and the note was dropped.
pub fn schedule_midi<E: DeviceEngine<N>, const N: usize>(
    engine: &mut E,
    event: MidiEvent,
    offset: usize,
) -> bool {
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
        // Queued for the same reason as NoteOn: a release applied between
        // blocks rather than at its offset would make the note's length
        // quantise to the block grid in one direction only, and the tail
        // would not line up with the host's playback.
        MidiEvent::NoteOff { channel, note } => {
            engine.schedule_timed(offset, crate::EngineEvent::NoteOff { channel, note })
        }
        _ => {
            handle_midi(engine, event);
            true
        }
    }
}

/// A parsed message the device cares about.
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
    /// Note off — the gate closed.
    ///
    /// Emitted for status `0x8n` and, per the convention every sequencer and
    /// keyboard follows, for a `0x9n` note-on with velocity 0. A voice that
    /// sustains treats this as the end of the note; a one-shot drum machine
    /// ignores it and lets the hit run its own envelope out.
    NoteOff {
        /// MIDI channel, `0..=15`.
        channel: u8,
        /// MIDI note number.
        note: u8,
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

        if let Some(filter) = self.channel_filter {
            if (self.status & 0x0F) != filter {
                return None;
            }
        }

        let channel = self.status & 0x0F;
        match self.status & 0xF0 {
            0x80 => Some(MidiEvent::NoteOff {
                channel,
                note: self.data[0],
            }),
            0x90 => {
                let velocity = self.data[1];
                if velocity == 0 {
                    // Note-on with zero velocity is a note-off. Synths that
                    // only listened for 0x8n silently hang on any controller
                    // that reports releases this way, which is most of them.
                    Some(MidiEvent::NoteOff {
                        channel,
                        note: self.data[0],
                    })
                } else {
                    Some(MidiEvent::NoteOn {
                        channel,
                        note: self.data[0],
                        velocity: velocity as f32 / 127.0,
                    })
                }
            }
            0xB0 => {
                let controller = self.data[0];
                let value = self.data[1];
                if controller == 120 || controller == 123 {
                    Some(MidiEvent::Panic)
                } else {
                    Some(MidiEvent::ControlChange {
                        channel,
                        controller,
                        value: value as f32 / 127.0,
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
    fn parses_a_note_off() {
        let mut p = MidiParser::new();
        let events = feed(&mut p, &[0x89, 36, 64]); // ch9, note 36
        assert_eq!(events.len(), 1);
        assert_eq!(
            events.get(0),
            MidiEvent::NoteOff {
                channel: 9,
                note: 36
            }
        );
    }

    /// Note-on velocity is a 7-bit *level* on the wire, so a controller that
    /// cannot send a 0x8n status byte reports a release as note-on 0. Every
    /// sequencer and DAW does this at some point; treating it as silence
    /// leaves the note hanging forever.
    #[test]
    fn zero_velocity_note_on_is_a_note_off() {
        let mut p = MidiParser::new();
        let events = feed(&mut p, &[0x99, 36, 0]);
        assert_eq!(events.len(), 1);
        assert_eq!(
            events.get(0),
            MidiEvent::NoteOff {
                channel: 9,
                note: 36
            }
        );
    }

    #[test]
    fn note_on_then_note_off_pairs_up() {
        let mut p = MidiParser::new();
        // Running status across both statuses is not legal — a new status
        // byte interrupts — so send them as two complete messages.
        let events = feed(&mut p, &[0x90, 60, 100, 0x80, 60, 0]);
        assert_eq!(events.len(), 2);
        assert!(matches!(events.get(0), MidiEvent::NoteOn { note: 60, .. }));
        assert_eq!(
            events.get(1),
            MidiEvent::NoteOff {
                channel: 0,
                note: 60
            }
        );
    }

    #[test]
    fn running_status_keeps_note_offs() {
        // A held chord: one status byte, note-on pairs, then a bare status
        // change to note-off and its pairs. A parser that reset running
        // status on the 0x80 byte would still pass the test above, so the
        // three note-offs here are what pins it.
        let mut p = MidiParser::new();
        let events = feed(
            &mut p,
            &[0x90, 60, 100, 64, 100, 67, 100, 0x80, 60, 0, 64, 0],
        );
        assert_eq!(events.len(), 5, "note-offs dropped under running status");
        for i in 3..5 {
            assert!(
                matches!(events.get(i), MidiEvent::NoteOff { .. }),
                "expected a note-off at {i}"
            );
        }
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
}
