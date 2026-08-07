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
//! Note-to-track routing lives in [`crate::DrumEngine`], not here — this
//! module parses bytes into events, the engine decides what an event means.

/// A parsed message the engine cares about.
#[derive(Clone, Copy, PartialEq, Debug)]
pub enum MidiEvent {
    /// Note on with velocity `0.0..=1.0`.
    NoteOn {
        /// MIDI note number.
        note: u8,
        /// Normalised velocity.
        velocity: f32,
    },
    /// Continuous controller.
    ControlChange {
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
            MidiEvent::NoteOn { note, velocity } => {
                assert_eq!(note, 36);
                approx::assert_abs_diff_eq!(velocity, 1.0, epsilon = 1e-6);
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
}
