//! MIDI parser and shared event router.
//!
//! A byte-at-a-time parser with running status, no allocation, no buffering
//! of anything larger than a three-byte message. Feed it bytes from wherever
//! they arrive — a UART interrupt, a USB MIDI packet, a test vector — and it
//! hands back events.
//!
//! [`handle_midi`], [`apply_cc`] and [`schedule_midi`] are generic over
//! [`DeviceEngine`](crate::engine::DeviceEngine) so every device interprets
//! MIDI bytes the same way. Device-specific routing (for example, which param
//! slot swaps the machine) is handled inside the engine's `set_param_target`;
//! the router itself only knows the flat param index and the track count.
//!
//! # Control surface: CC, and NRPN over it
//!
//! Two address spaces reach the same `Track::set_param_target`. The flat
//! param map — channel `N`, CC [`CC_TRACK_BASE` + flat] sets track `N`'s
//! param — is the address the Deluge and the firmware knobs speak. NRPN
//! addresses the same params as `(group, index)` (`ParamId`, resolved
//! through [`resolve`](crate::params::resolve)) at 14-bit resolution.
//!
//! The NRPN machine lives in [`MidiParser`], because the selection is
//! per-channel state the byte stream carries between messages:
//!
//! | CC                        | Role |
//! |---------------------------|------|
//! | [`CC_NRPN_PARAM_MSB`] (99) | parameter-select MSB — the group |
//! | [`CC_NRPN_PARAM_LSB`] (98) | parameter-select LSB — the index in the group |
//! | [`CC_DATA_ENTRY_MSB`] (6)  | data MSB; sets the 7 high bits, own event |
//! | [`CC_DATA_ENTRY_LSB`] (38) | data LSB; recomputes the full 14-bit value |
//! | [`CC_DATA_INCREMENT`] (96) | +`max(v,1)` LSBs, current value + step |
//! | [`CC_DATA_DECREMENT`] (97) | -`max(v,1)` LSBs |
//! | [`CC_RPN_PARAM_MSB`]/[`CC_RPN_PARAM_LSB`] (101/100) | RPN select — disarms the NRPN selection |
//!
//! A data message arrives as [`MidiEvent::Nrpn`] or
//! [`MidiEvent::NrpnStep`] with the address already attached;
//! [`handle_midi`] routes them through [`apply_nrpn`] /
//! [`apply_nrpn_step`], which resolve the address against the track's
//! *machine* table (group membership is per machine: VOICE has four rows on
//! Peaks and nine on Plaits, and only the track knows which is loaded).
//! Both paths end at the same `set_param_target`, so a param edited by a
//! CC or by an NRPN behaves identically, smoother included.
//!
//! All eight CCs are spec-assigned NRPN/RPN/data-entry controls, so the
//! parser consumes them outright: they never fall through to the CC param
//! map, CC 38 included, which the flat map used as pan until phase F
//! removes the map. `apply_cc` is unchanged; the wire no longer carries
//! CC 38.

use crate::engine::DeviceEngine;
use crate::params::{resolve, ParamGroup, ParamId};
use crate::slot::DeviceModel;

/// MIDI note that plays a track at its param-configured pitch. Middle C.
/// Every semitone away transposes the voice by that many semitones.
pub const CHROMATIC_REFERENCE_NOTE: u8 = 60;

/// First CC number reserved for track params. On channel `N`, CC
/// `CC_TRACK_BASE..CC_TRACK_BASE + NUM_PARAMS` sets track `N`'s params, where
/// `NUM_PARAMS` is the device's own count.
pub const CC_TRACK_BASE: u8 = 20;

/// CC for the master gain. Following MIDI convention. Global on any channel.
pub const CC_MASTER_GAIN: u8 = 7;

/// CC 99 — NRPN parameter-select MSB. Selects the [`ParamGroup`](crate::params::ParamGroup).
pub const CC_NRPN_PARAM_MSB: u8 = 99;
/// CC 98 — NRPN parameter-select LSB. Selects the parameter within the group.
pub const CC_NRPN_PARAM_LSB: u8 = 98;
/// CC 6 — data-entry MSB. The high 7 bits of a 14-bit value; applying it
/// alone sets the coarse value.
pub const CC_DATA_ENTRY_MSB: u8 = 6;
/// CC 38 — data-entry LSB. The low 7 bits; recomputes the value as the full
/// 14-bit `msb:lsb` when it follows a data-entry MSB.
pub const CC_DATA_ENTRY_LSB: u8 = 38;
/// CC 96 — data increment. Steps the selected parameter by up to
/// `max(v, 1)` LSBs, so a controller's acceleration curve rides in the
/// data byte.
pub const CC_DATA_INCREMENT: u8 = 96;
/// CC 97 — data decrement.
pub const CC_DATA_DECREMENT: u8 = 97;
/// CC 101 — RPN parameter-select MSB. Any RPN select disarms the NRPN
/// selection, so a following data entry does not reach a parameter.
pub const CC_RPN_PARAM_MSB: u8 = 101;
/// CC 100 — RPN parameter-select LSB.
pub const CC_RPN_PARAM_LSB: u8 = 100;
/// Full-scale of a 14-bit NRPN data value: `(msb << 7) | lsb`.
pub const NRPN_MAX: u16 = 16383;
/// The "no parameter selected" group flag for a channel's NRPN selector:
/// no valid group (a group MSB is 7-bit, and
/// [`ParamGroup::Reserved`](crate::params::ParamGroup::Reserved), the only
/// other value outside it, is never selected).
const NRPN_NO_SELECTION: u8 = ParamGroup::Reserved as u8;

/// Apply a MIDI event to a device engine.
///
/// `NoteOn` routes through [`DeviceEngine::trigger_channel`]: the channel
/// picks the track, the note picks the pitch. `NoteOff` routes through
/// [`DeviceEngine::release_channel`], the same channel-to-track mapping, so a
/// voice that sustains ends when the key comes up.
/// `ControlChange` reaches that channel's track-param block or the master
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
        MidiEvent::Nrpn {
            channel,
            group,
            index,
            value,
        } => {
            if let Some(id) = nrpn_id(group, index) {
                apply_nrpn(engine, channel, id, value);
            }
        }
        MidiEvent::NrpnStep {
            channel,
            group,
            index,
            step,
        } => {
            if let Some(id) = nrpn_id(group, index) {
                apply_nrpn_step(engine, channel, id, step);
            }
        }
        MidiEvent::Panic => engine.panic(),
    }
}

/// Map CC numbers onto engine parameters — channel-scoped track params +
/// master.
///
/// A CC on channel `N` edits track `N`, matching the note routing. `CC 7`
/// stays global. Params are set through the track's `set_param_target` so
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
        let param_idx = (controller - CC_TRACK_BASE) as usize;
        let track = channel as usize;
        let n_tracks = engine.tracks().len();
        if track < n_tracks && param_idx < N {
            engine.tracks_mut()[track].set_param_target(param_idx, value);
        }
    }
}

/// A wire `(group, index)` pair as a typed [`ParamId`].
///
/// `None` for a group this build has no identifier for — the controller is
/// addressing a parameter that cannot exist here, and the message is
/// dropped rather than guessed at.
fn nrpn_id(group: u8, index: u8) -> Option<ParamId> {
    ParamGroup::from_u8(group).map(|group| ParamId { group, index })
}

/// Resolve an addressed parameter against the selected track's machine table.
///
/// The table comes from the track's *own* machine — group membership is
/// per machine, so only the track knows which table the address speaks —
/// and [`resolve`] scans it. `None` if the channel holds no track or the
/// group has no such member (unknown group, index past the group's size,
/// or a reserved row: a reserved slot stays an unaddressable hole).
///
/// The scan is control-rate, so the linear lookup is deliberate: a
/// `GROUP_BASE`-style table would make it O(1) but force every group's
/// rows contiguous, which would reorder the flat array the CC map
/// addresses.
fn resolve_track_param<E: DeviceEngine<N>, const N: usize>(
    engine: &E,
    channel: u8,
    id: ParamId,
) -> Option<(usize, usize)> {
    let track = channel as usize;
    let table = engine.tracks().get(track)?.id().param_table();
    let idx = resolve(&table, id)?;
    Some((track, idx))
}

/// Set one parameter from an NRPN data entry.
///
/// The raw 14-bit value is normalized once, here, against [`NRPN_MAX`]:
/// discrete parameters can later be quantised exactly from the raw `u16`
/// (there is more information in it than in an `f32` in `0..1`), and the
/// value is set through the same [`set_param_target`](crate::track::Track::set_param_target)
/// a CC takes — identical smoother, identical routing, identical ramp.
pub fn apply_nrpn<E: DeviceEngine<N>, const N: usize>(
    engine: &mut E,
    channel: u8,
    id: ParamId,
    value: u16,
) {
    let Some((track, idx)) = resolve_track_param(engine, channel, id) else {
        return;
    };
    let v = value as f32 / NRPN_MAX as f32;
    engine.tracks_mut()[track].set_param_target(idx, v);
}

/// Move the selected parameter by `step` 14-bit LSBs (NRPN data
/// increment/decrement, CC 96/97).
///
/// Relative, so it reads the param's *destination*
/// ([`param_target`](crate::track::Track::param_target)) rather than its
/// base: a burst of steps arriving inside one block accumulates instead of
/// overwriting itself. The value quantizes onto the 14-bit grid, takes the
/// step, clamps to `0..=NRPN_MAX`, and is handed to the smoother, so a
/// one-LSB nudge applies exactly on the next block (below the snap
/// epsilon) while a large accelerated jump ramps.
///
/// The step magnitude is whatever the data byte carried, unmodified by
/// any engine-side scaling: acceleration is the controller's curve, which
/// is what keeps the engine free of per-param timing state.
pub fn apply_nrpn_step<E: DeviceEngine<N>, const N: usize>(
    engine: &mut E,
    channel: u8,
    id: ParamId,
    step: i16,
) {
    let Some((track, idx)) = resolve_track_param(engine, channel, id) else {
        return;
    };
    let current = engine.tracks()[track].param_target(idx);
    // +0.5 then truncate: round-to-nearest, matching the `set_param` Out
    // route's quantisation style (`round` is std-only in a no_std world).
    let raw = (current * NRPN_MAX as f32 + 0.5) as i32 + step as i32;
    let v = raw.clamp(0, NRPN_MAX as i32) as f32 / NRPN_MAX as f32;
    engine.tracks_mut()[track].set_param_target(idx, v);
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
    /// NRPN data entry — an addressed parameter takes an absolute value.
    ///
    /// `value` is the raw `0..=`[`NRPN_MAX`] data value, not yet
    /// normalized, so the router can quantise a discrete parameter
    /// exactly (there is more information in a raw `u16` than in an `f32`
    /// in `0..1`) and normalize once, in [`apply_nrpn`].
    Nrpn {
        /// MIDI channel, `0..=15`.
        channel: u8,
        /// NRPN parameter MSB as sent: the
        /// [`ParamGroup`](crate::params::ParamGroup) wire value. A group
        /// this build has no identifier for resolves no parameter.
        group: u8,
        /// NRPN parameter LSB as sent: the index within the group.
        index: u8,
        /// Raw 14-bit data value, `0..=`[`NRPN_MAX`].
        value: u16,
    },
    /// NRPN data increment/decrement — nudge the selected parameter.
    ///
    /// A relative data message, because the parameter's current value
    /// lives in the engine: only the router can turn "step by k" into an
    /// absolute target ([`apply_nrpn_step`]).
    NrpnStep {
        /// MIDI channel, `0..=15`.
        channel: u8,
        /// NRPN parameter MSB as sent (the group wire value).
        group: u8,
        /// NRPN parameter LSB as sent (the index within the group).
        index: u8,
        /// Signed step, in 14-bit LSBs. Magnitude is `max(v, 1)`, so a
        /// controller that sends a zero data byte (MIDI allows it, and
        /// plenty of hardware does) still moves one LSB.
        step: i16,
    },
}

/// Incremental MIDI parser.
///
/// Handles running status, which matters more than it might seem: a sequencer
/// firing dense note data will drop the status byte on repeated messages, and
/// a parser that does not track it will silently lose most of your notes.
///
/// Also holds the per-channel NRPN state — a selection and a retained data
/// MSB — because an NRPN transaction spans messages and the wire carries no
/// "end of selection" marker. The parser is the only stateful component in
/// the receive path, which is why the state sits in it: every consumer
/// (firmware UART + USB, render, tests) gets NRPN by owning a parser.
pub struct MidiParser {
    status: u8,
    data: [u8; 2],
    index: usize,
    /// Channel to listen on, or `None` for omni.
    channel_filter: Option<u8>,
    /// Per-channel NRPN parameter selection, `(group, index)`. The group
    /// half holds [`NRPN_NO_SELECTION`] until CC 99 names one, so data
    /// arriving with no selection (a stray CC 6, or an RPN's data entry)
    /// is ignored rather than landing on TRACK 0.
    nrpn_sel: [(u8, u8); 16],
    /// Per-channel last data-entry MSB, so CC 38 can recompute the 14-bit
    /// value a controller that splits the pair means to send as one.
    nrpn_msb: [u8; 16],
}

impl Default for MidiParser {
    fn default() -> Self {
        Self::new()
    }
}

impl MidiParser {
    /// Omni-mode parser.
    pub const fn new() -> Self {
        Self {
            status: 0,
            data: [0; 2],
            index: 0,
            channel_filter: None,
            nrpn_sel: [(NRPN_NO_SELECTION, 0); 16],
            nrpn_msb: [0; 16],
        }
    }

    /// Parser listening to a single channel, `0..=15`.
    pub const fn with_channel(channel: u8) -> Self {
        Self {
            status: 0,
            data: [0; 2],
            index: 0,
            channel_filter: Some(channel),
            nrpn_sel: [(NRPN_NO_SELECTION, 0); 16],
            nrpn_msb: [0; 16],
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
                    return Some(MidiEvent::Panic);
                }
                // The NRPN CCs are spec-assigned controls, so the parser
                // consumes every one of them outright — none falls through
                // to the CC param map, CC 38 included.
                if is_nrpn_cc(controller) {
                    return self.nrpn_message(channel, controller, value);
                }
                Some(MidiEvent::ControlChange {
                    channel,
                    controller,
                    value: value as f32 / 127.0,
                })
            }
            _ => None,
        }
    }

    /// Advance the NRPN state machine on one consumer-controller message.
    ///
    /// The message is consumed whether or not it produces an event: a
    /// parameter select, an RPN select, and a data or step message with no
    /// selection armed are all consumed. Events come from the two data
    /// forms — an absolute value (CC 6 / CC 38) or a signed step (CC 96 /
    /// CC 97).
    fn nrpn_message(&mut self, channel: u8, controller: u8, value: u8) -> Option<MidiEvent> {
        let ch = channel as usize;
        match controller {
            CC_NRPN_PARAM_MSB => {
                // Arms the selection. The group half is the sentinel holder,
                // so index can arrive earlier (98 before 99 is legal and
                // both orders must select the same parameter).
                self.nrpn_sel[ch].0 = value;
            }
            CC_NRPN_PARAM_LSB => {
                self.nrpn_sel[ch].1 = value;
            }
            // Any RPN select takes the data-entry pair back, so the NRPN
            // selection on this channel is dropped: a data entry that
            // follows must not reach a parameter.
            CC_RPN_PARAM_MSB | CC_RPN_PARAM_LSB => {
                self.nrpn_sel[ch] = (NRPN_NO_SELECTION, 0);
            }
            CC_DATA_ENTRY_MSB => {
                self.nrpn_msb[ch] = value;
                let (group, index) = self.nrpn_selected(ch)?;
                // A controller that sends the MSB alone gets the coarse
                // value: `msb << 7 | msb` normalises to exactly `msb/127`
                // under the single /16383 divisor, so 127 reaches 1.0.
                // When the LSB follows, CC 38 recomputes the full 14-bit
                // value from the retained MSB and the smoother hides the
                // change, so the two-step apply is inaudible.
                return Some(MidiEvent::Nrpn {
                    channel,
                    group,
                    index,
                    value: ((value as u16) << 7) | value as u16,
                });
            }
            CC_DATA_ENTRY_LSB => {
                let (group, index) = self.nrpn_selected(ch)?;
                return Some(MidiEvent::Nrpn {
                    channel,
                    group,
                    index,
                    value: ((self.nrpn_msb[ch] as u16) << 7) | value as u16,
                });
            }
            CC_DATA_INCREMENT | CC_DATA_DECREMENT => {
                let (group, index) = self.nrpn_selected(ch)?;
                // The data byte is the step magnitude in LSBs, clamped up
                // to one so a controller that sends 0 still moves.
                let magnitude = (value as i16).max(1);
                return Some(MidiEvent::NrpnStep {
                    channel,
                    group,
                    index,
                    step: if controller == CC_DATA_INCREMENT {
                        magnitude
                    } else {
                        -magnitude
                    },
                });
            }
            _ => {}
        }
        None
    }

    /// The channel's NRPN selection, if one is armed.
    fn nrpn_selected(&self, ch: usize) -> Option<(u8, u8)> {
        let sel = self.nrpn_sel[ch];
        if sel.0 == NRPN_NO_SELECTION {
            None
        } else {
            Some(sel)
        }
    }
}

/// True for the CCs the NRPN state machine owns.
///
/// Every one is a MIDI-spec-assigned NRPN/RPN/data-entry control, so they
/// never reach [`apply_cc`] — including CC 38, which the flat param map
/// used as pan before phase F removes the map. The param map runs
/// CC 20..=51, and 96..=101 sits above it, so 38 was the only collision.
fn is_nrpn_cc(controller: u8) -> bool {
    matches!(
        controller,
        CC_NRPN_PARAM_MSB
            | CC_NRPN_PARAM_LSB
            | CC_DATA_ENTRY_MSB
            | CC_DATA_ENTRY_LSB
            | CC_DATA_INCREMENT
            | CC_DATA_DECREMENT
            | CC_RPN_PARAM_MSB
            | CC_RPN_PARAM_LSB
    )
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
            pub fn is_empty(&self) -> bool {
                self.len == 0
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

    // ----- NRPN -----

    /// The order every editor sends: select the group, select the index,
    /// then the data pair.
    #[test]
    fn nrpn_selects_then_data_entry() {
        let mut p = MidiParser::new();
        let events = feed(
            &mut p,
            &[
                0xB0,
                CC_NRPN_PARAM_MSB,
                1,
                0xB0,
                CC_NRPN_PARAM_LSB,
                2,
                0xB0,
                CC_DATA_ENTRY_MSB,
                64,
                0xB0,
                CC_DATA_ENTRY_LSB,
                100,
            ],
        );
        assert_eq!(
            events.len(),
            2,
            "the data pair is two applies, coarse then fine"
        );
        // The MSB applies immediately at 7-bit resolution: raw = msb·129,
        // so the router's /16383 lands on exactly 64/127.
        match events.get(0) {
            MidiEvent::Nrpn {
                channel,
                group,
                index,
                value,
            } => {
                assert_eq!(channel, 0);
                assert_eq!(group, 1, "group MSB");
                assert_eq!(index, 2, "group LSB");
                assert_eq!(value, (64 << 7) | 64);
            }
            other => panic!("unexpected {other:?}"),
        }
        // The LSB recomputes the 14-bit value from the retained MSB.
        match events.get(1) {
            MidiEvent::Nrpn { value, .. } => {
                assert_eq!(value, (64 << 7) | 100);
            }
            other => panic!("unexpected {other:?}"),
        }
    }

    /// 98 before 99 is legal on the wire and must select the same
    /// parameter as 99 before 98.
    #[test]
    fn nrpn_index_can_precede_the_group() {
        let mut p = MidiParser::new();
        let events = feed(
            &mut p,
            &[
                0xB0,
                CC_NRPN_PARAM_LSB,
                2,
                0xB0,
                CC_NRPN_PARAM_MSB,
                1,
                0xB0,
                CC_DATA_ENTRY_MSB,
                64,
            ],
        );
        assert_eq!(events.len(), 1);
        assert_eq!(
            events.get(0),
            MidiEvent::Nrpn {
                channel: 0,
                group: 1,
                index: 2,
                value: (64 << 7) | 64,
            }
        );
    }

    /// A controller that sends only CC 6 must reach full scale.
    #[test]
    fn cc6_alone_reaches_full_scale() {
        let mut p = MidiParser::new();
        let events = feed(
            &mut p,
            &[
                0xB0,
                CC_NRPN_PARAM_MSB,
                2,
                0xB0,
                CC_NRPN_PARAM_LSB,
                0,
                0xB0,
                CC_DATA_ENTRY_MSB,
                127,
            ],
        );
        assert_eq!(events.len(), 1);
        match events.get(0) {
            MidiEvent::Nrpn { value, .. } => {
                assert_eq!(value, NRPN_MAX, "msb 127 is exactly full scale");
            }
            other => panic!("unexpected {other:?}"),
        }
    }

    /// The selection persists across data entries: re-arming only what
    /// changed is the normal editing pattern.
    #[test]
    fn repeated_cc6_retains_the_selection() {
        let mut p = MidiParser::new();
        let events = feed(
            &mut p,
            &[
                0xB0,
                CC_NRPN_PARAM_MSB,
                1,
                0xB0,
                CC_NRPN_PARAM_LSB,
                2,
                0xB0,
                CC_DATA_ENTRY_MSB,
                10,
                0xB0,
                CC_NRPN_PARAM_MSB,
                1,
                0xB0,
                CC_DATA_ENTRY_MSB,
                20,
            ],
        );
        assert_eq!(events.len(), 2);
        assert_eq!(
            events.get(0),
            MidiEvent::Nrpn {
                channel: 0,
                group: 1,
                index: 2,
                value: 10 * 129,
            }
        );
        assert_eq!(
            events.get(1),
            MidiEvent::Nrpn {
                channel: 0,
                group: 1,
                index: 2,
                value: 20 * 129,
            }
        );
    }

    /// The step pair: CC 96 / CC 97 with a step magnitude in the data byte,
    /// and the max(v,1) floor for senders that always send zero.
    #[test]
    fn nrpn_increment_and_decrement_carry_a_step() {
        let mut p = MidiParser::new();
        let events = feed(
            &mut p,
            &[0xB0, CC_NRPN_PARAM_MSB, 1, 0xB0, CC_NRPN_PARAM_LSB, 0],
        );
        assert!(events.is_empty(), "selects produce no events");

        let events = feed(&mut p, &[0xB0, CC_DATA_INCREMENT, 1]);
        assert_eq!(events.len(), 1);
        match events.get(0) {
            MidiEvent::NrpnStep {
                channel,
                group,
                index,
                step,
            } => {
                assert_eq!(channel, 0);
                assert_eq!(group, 1);
                assert_eq!(index, 0);
                assert_eq!(step, 1, "one LSB forward");
            }
            other => panic!("unexpected {other:?}"),
        }

        let events = feed(&mut p, &[0xB0, CC_DATA_DECREMENT, 5]);
        assert_eq!(
            events.get(0),
            MidiEvent::NrpnStep {
                channel: 0,
                group: 1,
                index: 0,
                step: -5
            }
        );
        let events = feed(&mut p, &[0xB0, CC_DATA_INCREMENT, 0]);
        assert_eq!(
            events.get(0),
            MidiEvent::NrpnStep {
                channel: 0,
                group: 1,
                index: 0,
                step: 1
            },
            "a zero data byte still moves one LSB"
        );
    }

    /// No selection: a data entry (CC 6/38) or a step (CC 96/97) is
    /// consumed and dropped. It must not reach the CC map either — CC 38
    /// was the map's pan entry until phase F removes the map.
    #[test]
    fn nrpn_data_without_a_selection_is_ignored() {
        let mut p = MidiParser::new();
        let events = feed(
            &mut p,
            &[
                0xB0,
                CC_DATA_ENTRY_MSB,
                64,
                0xB0,
                CC_DATA_ENTRY_LSB,
                100,
                0xB0,
                CC_DATA_INCREMENT,
                7,
                0xB0,
                CC_DATA_DECREMENT,
                7,
                0xB0,
                CC_DATA_ENTRY_LSB,
                127,
            ],
        );
        assert!(
            events.is_empty(),
            "unselected NRPN data must be swallowed, never a ControlChange"
        );
    }

    /// Any RPN select (101/100, either order, any value) disarms the NRPN
    /// selection, so a following data entry does not reach a parameter.
    #[test]
    fn rpn_null_clears_the_selection() {
        let mut p = MidiParser::new();
        let events = feed(
            &mut p,
            &[0xB0, CC_NRPN_PARAM_MSB, 1, 0xB0, CC_NRPN_PARAM_LSB, 2],
        );
        assert!(events.is_empty());

        let events = feed(
            &mut p,
            &[0xB0, CC_RPN_PARAM_MSB, 127, 0xB0, CC_RPN_PARAM_LSB, 127],
        );
        assert!(events.is_empty());

        let events = feed(
            &mut p,
            &[0xB0, CC_DATA_ENTRY_MSB, 64, 0xB0, CC_DATA_ENTRY_LSB, 100],
        );
        assert!(
            events.is_empty(),
            "a data entry after RPN-null must reach no parameter"
        );

        // And a fresh select arms again, so the parser is reusable.
        let events = feed(
            &mut p,
            &[0xB0, CC_NRPN_PARAM_MSB, 1, 0xB0, CC_DATA_ENTRY_MSB, 64],
        );
        assert_eq!(events.len(), 1);
    }

    /// The NRPN transaction under running status: one status byte, the
    /// pairs stream after it, exactly as a dense sequencer sends CCs.
    #[test]
    fn nrpn_pairs_under_running_status() {
        let mut p = MidiParser::new();
        let events = feed(
            &mut p,
            &[
                0xB0,
                CC_NRPN_PARAM_MSB,
                1,
                CC_NRPN_PARAM_LSB,
                2,
                CC_DATA_ENTRY_MSB,
                64,
                CC_DATA_ENTRY_LSB,
                100,
            ],
        );
        assert_eq!(
            events.len(),
            2,
            "running status dropped part of the transaction"
        );
        assert_eq!(
            events.get(1),
            MidiEvent::Nrpn {
                channel: 0,
                group: 1,
                index: 2,
                value: (64 << 7) | 100,
            }
        );
    }

    /// Real-time bytes interleave anywhere, including mid-pair, and must
    /// not disturb the selection.
    #[test]
    fn realtime_bytes_do_not_break_nrpn() {
        let mut p = MidiParser::new();
        let events = feed(
            &mut p,
            &[
                0xB0,
                CC_NRPN_PARAM_MSB,
                1,
                0xF8,
                CC_NRPN_PARAM_LSB,
                2,
                0xF8,
                CC_DATA_ENTRY_MSB,
                64,
            ],
        );
        assert_eq!(events.len(), 1);
        assert_eq!(
            events.get(0),
            MidiEvent::Nrpn {
                channel: 0,
                group: 1,
                index: 2,
                value: (64 << 7) | 64,
            }
        );
    }

    /// The selection is per channel: an NRPN on channel 0 arms nothing on
    /// channel 1, matching the one-channel-per-track routing.
    #[test]
    fn nrpn_selection_is_per_channel() {
        let mut p = MidiParser::new();
        let events = feed(
            &mut p,
            &[0xB0, CC_NRPN_PARAM_MSB, 1, 0xB0, CC_NRPN_PARAM_LSB, 2],
        );
        assert!(events.is_empty());

        let events = feed(&mut p, &[0xB1, CC_DATA_ENTRY_MSB, 64]);
        assert!(
            events.is_empty(),
            "channel 1 has no selection — the data must fall through"
        );

        let events = feed(&mut p, &[0xB0, CC_DATA_ENTRY_MSB, 64]);
        assert_eq!(
            events.get(0),
            MidiEvent::Nrpn {
                channel: 0,
                group: 1,
                index: 2,
                value: (64 << 7) | 64,
            }
        );
    }

    /// A single-channel parser keeps its own selection, and only answers
    /// on its channel.
    #[test]
    fn channel_filter_scopes_the_nrpn_selection() {
        let mut p = MidiParser::with_channel(9);
        let events = feed(
            &mut p,
            &[0xB0, CC_NRPN_PARAM_MSB, 1, 0xB0, CC_DATA_ENTRY_MSB, 64],
        );
        assert!(events.is_empty(), "channel 0 is filtered out");

        let events = feed(
            &mut p,
            &[
                0xB9,
                CC_NRPN_PARAM_MSB,
                1,
                0xB9,
                CC_NRPN_PARAM_LSB,
                2,
                0xB9,
                CC_DATA_ENTRY_MSB,
                64,
            ],
        );
        assert_eq!(events.len(), 1);
        match events.get(0) {
            MidiEvent::Nrpn { channel, .. } => assert_eq!(channel, 9),
            other => panic!("unexpected {other:?}"),
        }
    }

    /// The parser stays protocol-only: a group this build has no
    /// identifier for is still emitted, and the router drops it. Decision
    /// at the router keeps the parser free of device knowledge.
    #[test]
    fn parser_passes_unknown_groups_through_to_the_router() {
        let mut p = MidiParser::new();
        let events = feed(
            &mut p,
            &[
                0xB0,
                CC_NRPN_PARAM_MSB,
                8,
                0xB0,
                CC_NRPN_PARAM_LSB,
                0,
                0xB0,
                CC_DATA_ENTRY_MSB,
                64,
            ],
        );
        assert_eq!(
            events.get(0),
            MidiEvent::Nrpn {
                channel: 0,
                group: 8,
                index: 0,
                value: (64 << 7) | 64,
            },
            "group parsing is the wire, not the device — the router drops it"
        );
    }

    /// `Default` must arm no selection, same as `new`. A derived Default
    /// would leave the group half at 0 (= TRACK) and silently make an
    /// unselected channel addressable.
    #[test]
    fn default_parser_has_no_selection() {
        let mut p = MidiParser::default();
        let events = feed(
            &mut p,
            &[0xB0, CC_DATA_ENTRY_MSB, 64, 0xB0, CC_DATA_INCREMENT, 3],
        );
        assert!(events.is_empty());
    }

    /// The router layer's group mapping: wire value in, `ParamId` out,
    /// with unselectable groups dropped.
    #[test]
    fn unselectable_groups_resolve_no_parameter() {
        assert_eq!(
            nrpn_id(0, 0),
            Some(ParamId {
                group: ParamGroup::Track,
                index: 0
            })
        );
        assert_eq!(
            nrpn_id(127, 3),
            Some(ParamId {
                group: ParamGroup::Global,
                index: 3
            })
        );
        assert_eq!(
            nrpn_id(8, 0),
            None,
            "no identifier for group 8 in this build"
        );
        assert_eq!(
            nrpn_id(255, 0),
            None,
            "Reserved is a row label, not a selectable group"
        );
    }
}
