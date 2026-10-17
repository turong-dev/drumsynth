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
//!   the machine's param pitch; each semitone away transposes the voice by
//!   that many (track retune relative, so the param pitch still maps the
//!   same). Velocity 1..127 scales the voice output.
//! * A `NoteOff` (or a `NoteOn` with zero velocity, which is the same thing on
//!   the wire) closes the gate on track `N-1`. **Most machines ignore it**:
//!   the drum voices are one-shots, and a note-off arriving after the attack
//!   has passed must not shorten the hit. The two sustained machines —
//!   [`DubSiren`](crate::machines::DubSiren) and
//!   [`SweepFx`](crate::machines::SweepFx) — act on it and start their
//!   release decay, which is what makes them playable as held gestures rather
//!   than fixed-length ones.
//!
//! Zero-velocity note-on is a note-off, not silence. Controllers that cannot
//! send a `0x8n` status byte report releases that way, and treating it as a
//! no-op leaves the note hanging.
//!
//! # Parameters: NRPN
//!
//! Params are addressed `(group, index)` at 14-bit resolution: CC 99
//! selects the [`ParamGroup`](device_core::params::ParamGroup), CC 98 the
//! index within it, and the data-entry pair (CC 6 / CC 38) or the step pair
//! (CC 96 / CC 97) carries the value. The channel still picks the track, so
//! `(VOICE, 0)` on channel 3 is track 2's tune.
//!
//! A data message arrives as [`MidiEvent::Nrpn`] / [`MidiEvent::NrpnStep`],
//! resolves against the track's own machine table through [`apply_nrpn`] /
//! [`apply_nrpn_step`], and sets a smoothed target. A machine's VOICE rows
//! are its own, so `(VOICE, 3)` means whatever the loaded machine's fourth
//! voice param is — `MOD.HZ` on BD FM, nothing at all on BD Classic, which
//! has three.
//!
//! | Group     | wire value | on this device |
//! |-----------|-----------:|----------------|
//! | `TRACK`   | 0 | machine, out, pan, level, delay send, reverb send |
//! | `VOICE`   | 1 | the machine's own synthesis params |
//! | `FILTER`  | 2 | strip SVF cutoff, resonance |
//! | `AMP`     | 4 | strip AHD attack, hold, decay |
//! | `LFO`     | 5 | both LFOs: rate, depth, destination |
//!
//! `(TRACK, 0)` is the machine selector: the value is quantised over
//! [`crate::MachineId::ALL`] and loads that machine on the track, so an
//! engine can be swapped from MIDI without a program-change message. It is
//! a discrete param, so a 14-bit value snaps onto a machine rather than
//! landing between two, and a CC 96/97 step moves exactly one machine.
//!
//! # Control Changes
//!
//! Three, and no more:
//!
//! | CC        | role |
//! |-----------|------|
//! | 7         | master gain, global on any channel |
//! | 120, 123  | panic — all sound / all notes off, any channel |
//!
//! Everything else is either an NRPN/RPN/data-entry control the parser
//! consumes, or ignored.
//!
//! **The flat `CC 20 + index` map is gone** (phase F of #1). It was 7 bits,
//! which is roughly a semitone per step on a filter cutoff, and 32 entries
//! wide, which mi-drum had already outgrown. Removing it is what makes the
//! flat param index internal: nothing off-device addresses it now.
//!
//! # Sample-accurate notes
//!
//! [`schedule_midi`] is the main-loop variant of [`handle_midi`]. `NoteOn`
//! and `NoteOff` events are queued into the engine's
//! [`TimedQueue`](crate::TimedQueue) with a sample offset (where in the
//! *next* audio block they should fire), so the firmware can land notes where
//! the groove box placed them instead of at the next block boundary, and a
//! held note's length is honoured to the sample rather than quantised in one
//! direction only. `ControlChange` and `Panic` still apply immediately — CCs
//! are control-rate, and a panic must cut instantly.
//!
//! # Param smoothing
//!
//! Track params are set through [`Track::set_param_target`](crate::Track::set_param_target),
//! not `set_param`: the value ramps to its target at block rate (~7.5 ms
//! one-pole) inside the audio callback's control pass. A burst of CCs (a
//! knob being spun) therefore costs one coefficient recompute per param per
//! block instead of one per message, which keeps the message path cheap
//! enough for the main loop and keeps the `expf`-heavy recomputes off the
//! audio hot path. The machine selector and the output routing are exempt —
//! they jump instantly, because ramping a discrete choice would walk
//! through every value in between.

pub use device_core::midi::{
    apply_cc, apply_nrpn, apply_nrpn_step, handle_midi, schedule_midi, MidiEvent, MidiParser,
    CC_DATA_DECREMENT, CC_DATA_ENTRY_LSB, CC_DATA_ENTRY_MSB, CC_DATA_INCREMENT, CC_MASTER_GAIN,
    CC_NRPN_PARAM_LSB, CC_NRPN_PARAM_MSB, CC_RPN_PARAM_LSB, CC_RPN_PARAM_MSB,
    CHROMATIC_REFERENCE_NOTE, NRPN_MAX,
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
    fn converge_params(e: &mut crate::DrumEngine, track: usize) {
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
        // transpose), so the kick plays at its param pitch.
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

    /// The flat `CC 20 + index` map is gone. Master gain is the only
    /// parameter still on a plain CC, and a CC in the old param range
    /// reaches nothing.
    #[test]
    fn plain_cc_reaches_only_the_master_gain() {
        use crate::DrumEngine;
        // One engine for the whole test — DrumEngine is ~260 KB, so a fresh
        // one per assertion would stack-overflow the 2 MB test thread.
        let mut e = DrumEngine::new();

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

        // Every CC the old map used is now inert. 20 was param 0, 27 was
        // param 7, 36 was the machine selector, 51 was the last LFO row.
        let before = e.tracks[0].base_params;
        let machine_before = e.tracks[0].id();
        for controller in [20u8, 27, 36, 51] {
            handle_midi(
                &mut e,
                MidiEvent::ControlChange {
                    channel: 0,
                    controller,
                    value: 1.0,
                },
            );
        }
        converge_params(&mut e, 0);
        assert_eq!(
            e.tracks[0].base_params, before,
            "the flat CC param map was removed in phase F"
        );
        assert_eq!(e.tracks[0].id(), machine_before);
        assert_eq!(e.master_gain, 0.25, "and none of them is the master gain");
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

    /// Bytes in, gate closed. The end-to-end path a keyboard exercises: parse,
    /// route by channel, release the track that channel owns.
    #[test]
    fn note_off_bytes_release_the_channel_s_track() {
        use crate::{DrumEngine, MachineId, BLOCK};

        let mut e = DrumEngine::new();
        e.tracks[0].load_machine(MachineId::SweepFx);

        let mut p = MidiParser::new();
        for b in [0x90u8, 60, 127] {
            if let Some(ev) = p.push(b) {
                handle_midi(&mut e, ev);
            }
        }
        assert!(e.tracks[0].is_active(), "note-on should sound track 0");

        let mut l = [0.0f32; BLOCK];
        let mut r = [0.0f32; BLOCK];
        for _ in 0..16 {
            e.process(&mut l, &mut r);
        }

        // Note-off on the same channel.
        for b in [0x80u8, 60, 0] {
            if let Some(ev) = p.push(b) {
                handle_midi(&mut e, ev);
            }
        }
        // A gated machine fades rather than cutting, so the track stays active
        // briefly and then rests.
        for _ in 0..(3.0 * crate::SAMPLE_RATE / BLOCK as f32) as usize {
            e.process(&mut l, &mut r);
        }
        assert!(
            !e.is_active(),
            "note-off should have ended the sustained gesture"
        );
    }

    /// A zero-velocity note-on is the same message on the wire, and the gate
    /// has to close either way. A controller that only reports releases this
    /// way would otherwise hang every note.
    #[test]
    fn zero_velocity_note_on_also_closes_the_gate() {
        use crate::{DrumEngine, MachineId, BLOCK};

        let mut e = DrumEngine::new();
        e.tracks[0].load_machine(MachineId::DubSiren);

        let mut p = MidiParser::new();
        for b in [0x90u8, 60, 127] {
            if let Some(ev) = p.push(b) {
                handle_midi(&mut e, ev);
            }
        }
        let mut l = [0.0f32; BLOCK];
        let mut r = [0.0f32; BLOCK];
        for _ in 0..16 {
            e.process(&mut l, &mut r);
        }
        assert!(e.tracks[0].is_active());

        for b in [0x90u8, 60, 0] {
            if let Some(ev) = p.push(b) {
                handle_midi(&mut e, ev);
            }
        }
        for _ in 0..(3.0 * crate::SAMPLE_RATE / BLOCK as f32) as usize {
            e.process(&mut l, &mut r);
        }
        assert!(!e.is_active(), "velocity-0 note-on should close the gate");
    }

    /// A one-shot drum machine must ignore the note-off. If it did not, every
    /// hit from a controller that sends note-offs would be cut short — which is
    /// the pre-existing behaviour of the whole catalogue and must stay that
    /// way.
    #[test]
    fn one_shot_machines_ignore_the_note_off() {
        use crate::{DrumEngine, MachineId, BLOCK};

        let mut e = DrumEngine::new();
        // Track 0 is the kick in the default kit.
        assert_eq!(e.tracks[0].id(), MachineId::BdClassic);

        e.trigger(0, 1.0);
        let mut l = [0.0f32; BLOCK];
        let mut r = [0.0f32; BLOCK];
        for _ in 0..4 {
            e.process(&mut l, &mut r);
        }

        e.release(0);
        for _ in 0..16 {
            e.process(&mut l, &mut r);
        }
        assert!(
            e.tracks[0].is_active(),
            "a note-off must not shorten a one-shot drum hit"
        );
    }

    #[test]
    fn schedule_midi_applies_params_and_panic_immediately() {
        use crate::{DrumEngine, BLOCK};
        use device_core::params::ParamGroup;
        let mut e = DrumEngine::new();

        // A param message lands immediately as a target (it still slews;
        // the machine selector and panics are the instant path).
        // VOICE 4 is BD Classic's DRIVE, flat 7, default 0.16 — far enough
        // from half scale that a ramp is distinguishable from a jump.
        schedule_midi(
            &mut e,
            MidiEvent::Nrpn {
                channel: 0,
                group: ParamGroup::Voice as u8,
                index: 4,
                value: NRPN_MAX / 2,
            },
            0,
        );
        assert!(
            (e.tracks[0].base_params[7] - 0.5).abs() > 0.3,
            "an NRPN value should be smoothed, not instant"
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
            "the param edit + note must not sit in the queue"
        );
    }

    #[test]
    fn a_param_value_ramps_to_its_target() {
        use crate::DrumEngine;
        use device_core::params::ParamGroup;
        let mut e = DrumEngine::new();
        // VOICE 4 is BD Classic's DRIVE, flat 7, default 0.16.
        let before = e.tracks[0].base_params[7];

        handle_midi(
            &mut e,
            MidiEvent::Nrpn {
                channel: 0,
                group: ParamGroup::Voice as u8,
                index: 4,
                value: NRPN_MAX / 2,
            },
        );
        // The base param must not have jumped; it is slewing.
        assert!(
            (e.tracks[0].base_params[7] - 0.5).abs() > 0.3,
            "the value applied instantly"
        );
        assert_eq!(
            e.tracks[0].base_params[7], before,
            "base should be untouched mid-ramp"
        );

        // One control pass nudges it partway toward the target.
        e.tracks[0].control();
        assert!(
            e.tracks[0].base_params[7] > before,
            "first block must move toward the target"
        );

        // Enough blocks converge to the exact target.
        converge_params(&mut e, 0);
        approx::assert_abs_diff_eq!(
            e.tracks[0].base_params[7],
            (NRPN_MAX / 2) as f32 / NRPN_MAX as f32,
            epsilon = 1e-6
        );
    }

    #[test]
    fn direct_set_param_cancels_a_pending_ramp() {
        use crate::DrumEngine;
        let mut e = DrumEngine::new();
        e.tracks[0].set_param_target(7, 1.0);
        // A programmatic set mid-ramp is authoritative: the ramp stops.
        e.tracks[0].set_param(7, 0.2);
        converge_params(&mut e, 0);
        assert_eq!(
            e.tracks[0].base_params[7], 0.2,
            "the pending ramp should have been cancelled"
        );
    }

    // ----- NRPN -----

    /// Push wire bytes through a parser and route the events, exactly as
    /// the firmware receives them. Keep the parser: an NRPN transaction
    /// spans messages, and the selection is per-parser state.
    fn feed_nrpn(p: &mut MidiParser, e: &mut crate::DrumEngine, bytes: &[u8]) {
        for &b in bytes {
            if let Some(ev) = p.push(b) {
                handle_midi(e, ev);
            }
        }
    }

    /// NRPN bytes for one transaction: select `(group, index)`, then the
    /// 14-bit data value.
    fn nrpn_bytes(group: u8, index: u8, msb: u8, lsb: u8) -> [u8; 9] {
        [
            0xB0,
            CC_NRPN_PARAM_MSB,
            group,
            CC_NRPN_PARAM_LSB,
            index,
            CC_DATA_ENTRY_MSB,
            msb,
            CC_DATA_ENTRY_LSB,
            lsb,
        ]
    }

    /// The path the issue cares about: bytes in, a param set through the
    /// *track's machine table* — which is what makes a per-machine group
    /// ordinal land on the right flat row without any reordering.
    #[test]
    fn nrpn_sets_a_param_the_track_machine_names() {
        use crate::DrumEngine;
        use device_core::params::{resolve, ParamGroup, ParamId};
        use device_core::slot::DeviceModel;
        let mut e = DrumEngine::new();
        let mut p = MidiParser::new();
        let table = e.tracks[0].id().param_table();
        let id = ParamId {
            group: ParamGroup::Voice,
            index: 0,
        };
        let flat = resolve(table, id).expect("VOICE 0 on the default machine");
        let neighbour = e.tracks[1].base_params[flat];

        let msb = 80u8;
        let lsb = 33u8;
        feed_nrpn(
            &mut p,
            &mut e,
            &nrpn_bytes(ParamGroup::Voice as u8, 0, msb, lsb),
        );
        converge_params(&mut e, 0);
        let raw = ((msb as u16) << 7) | lsb as u16;
        approx::assert_abs_diff_eq!(
            e.tracks[0].base_params[flat],
            raw as f32 / NRPN_MAX as f32,
            epsilon = 1e-4
        );
        assert_eq!(
            e.tracks[1].base_params[flat], neighbour,
            "channel 0 must not touch track 1"
        );
    }

    /// Channel scoping: an NRPN on channel `N` edits track `N` and no
    /// other, matching the note routing, and a channel past the track
    /// count is dropped.
    #[test]
    fn nrpn_is_channel_scoped() {
        use crate::DrumEngine;
        use device_core::params::{resolve, ParamGroup, ParamId};
        use device_core::slot::DeviceModel;
        let mut e = DrumEngine::new();
        let mut p = MidiParser::new();
        let table = e.tracks[0].id().param_table();
        let flat = resolve(
            table,
            ParamId {
                group: ParamGroup::Voice,
                index: 1,
            },
        )
        .expect("VOICE 1 on the default machine");
        let track1_before = e.tracks[1].base_params[flat];

        feed_nrpn(
            &mut p,
            &mut e,
            &nrpn_bytes(ParamGroup::Voice as u8, 1, 64, 0),
        );
        converge_params(&mut e, 0);
        approx::assert_abs_diff_eq!(
            e.tracks[0].base_params[flat],
            8192.0 / 16383.0,
            epsilon = 1e-4
        );
        assert_eq!(
            e.tracks[1].base_params[flat], track1_before,
            "channel 0 must not touch track 1"
        );

        // The same address on channel 3 lands on track 3 instead.
        let mut bytes = nrpn_bytes(ParamGroup::Voice as u8, 1, 32, 0);
        bytes[0] = 0xB3;
        feed_nrpn(&mut p, &mut e, &bytes);
        converge_params(&mut e, 3);
        approx::assert_abs_diff_eq!(
            e.tracks[3].base_params[flat],
            4096.0 / 16383.0,
            epsilon = 1e-4
        );
        approx::assert_abs_diff_eq!(
            e.tracks[0].base_params[flat],
            8192.0 / 16383.0,
            epsilon = 1e-4
        );
    }

    /// Pan is now reached by name — `(TRACK, 2)` — and the wire CC that
    /// used to be pan is data-entry LSB, consumed by the NRPN parser.
    #[test]
    fn nrpn_sets_pan_via_the_track_group() {
        use crate::{DrumEngine, SLOT_PAN};
        let mut e = DrumEngine::new();
        let mut p = MidiParser::new();
        feed_nrpn(&mut p, &mut e, &nrpn_bytes(0, 2, 100, 0));
        converge_params(&mut e, 0);
        // The data pair lands the 14-bit value: (100 << 7) | 0.
        let v = (100u16 << 7) as f32 / NRPN_MAX as f32;
        approx::assert_abs_diff_eq!(e.tracks[0].base_params[SLOT_PAN], v, epsilon = 1e-4);
        approx::assert_abs_diff_eq!(e.tracks[0].strip.pan, v * 2.0 - 1.0, epsilon = 1e-4);
        assert!(
            e.tracks[0].strip.pan > 0.1,
            "an off-centre pan must move the strip's pan off centre"
        );

        // CC 38 on the wire: it was pan on the flat map, it is data LSB to
        // the parser now, and it reaches no param when there is no active
        // NRPN selection.
        let before = e.tracks[0].base_params[SLOT_PAN];
        let mut fresh = MidiParser::new();
        feed_nrpn(&mut fresh, &mut e, &[0xB0, CC_DATA_ENTRY_LSB, 127]);
        converge_params(&mut e, 0);
        assert_eq!(
            e.tracks[0].base_params[SLOT_PAN], before,
            "CC 38 is NRPN data LSB, not the pan entry"
        );
    }

    /// The machine selector is TRACK 0: an NRPN data entry onto it jumps
    /// instantly, exactly as CC 36 does — and resets params to the new
    /// machine's defaults.
    #[test]
    fn nrpn_swaps_the_machine_instantly() {
        use crate::{DrumEngine, MachineId, SLOT_MACHINE};
        let mut e = DrumEngine::new();
        let mut p = MidiParser::new();
        assert_eq!(e.tracks[0].id(), MachineId::BdClassic);
        feed_nrpn(&mut p, &mut e, &nrpn_bytes(0, 0, 64, 0));
        assert_eq!(
            e.tracks[0].id().index(),
            7,
            "0.5 quantises to catalogue index 7 and jumps without a ramp"
        );
        let divisor = (MachineId::ALL.len() - 1) as f32;
        approx::assert_abs_diff_eq!(
            e.tracks[0].base_params[SLOT_MACHINE],
            7.0f32 / divisor,
            epsilon = 1e-4
        );
    }

    /// Increments and decrements: one LSB snaps on the first block, a
    /// burst inside one block accumulates, and the value clamps at the
    /// 14-bit rails.
    #[test]
    fn nrpn_step_nudges_by_lsbs_and_clamps() {
        use crate::DrumEngine;
        use device_core::params::{resolve, ParamGroup, ParamId};
        use device_core::slot::DeviceModel;
        let mut e = DrumEngine::new();
        // One parser for the whole transaction: the selection persists
        // across messages, exactly as it does on a real controller.
        let mut p = MidiParser::new();
        let table = e.tracks[0].id().param_table();
        let flat = resolve(
            table,
            ParamId {
                group: ParamGroup::Voice,
                index: 0,
            },
        )
        .expect("VOICE 0 on the default machine");

        // Land on an exact 14-bit value: (62 << 7) | 64 = 8000.
        feed_nrpn(
            &mut p,
            &mut e,
            &nrpn_bytes(ParamGroup::Voice as u8, 0, 62, 64),
        );
        converge_params(&mut e, 0);
        approx::assert_abs_diff_eq!(
            e.tracks[0].base_params[flat],
            8000.0 / 16383.0,
            epsilon = 1e-5
        );

        // One LSB: below the snap epsilon, so it applies exactly on the
        // first block after the message.
        feed_nrpn(&mut p, &mut e, &[0xB0, CC_DATA_INCREMENT, 1]);
        e.tracks[0].control();
        assert_eq!(
            e.tracks[0].base_params[flat],
            8001.0 / 16383.0,
            "a single-LSB nudge snaps on the first block"
        );

        // Three steps inside one block accumulate on the destination.
        feed_nrpn(
            &mut p,
            &mut e,
            &[
                0xB0,
                CC_DATA_INCREMENT,
                100,
                0xB0,
                CC_DATA_INCREMENT,
                100,
                0xB0,
                CC_DATA_INCREMENT,
                100,
            ],
        );
        converge_params(&mut e, 0);
        approx::assert_abs_diff_eq!(
            e.tracks[0].base_params[flat],
            8301.0 / 16383.0,
            epsilon = 1e-5
        );

        // A big accelerated jump ramps rather than snapping: mid-ramp the
        // param is pending and has not arrived.
        feed_nrpn(&mut p, &mut e, &[0xB0, CC_DATA_INCREMENT, 100]);
        e.tracks[0].control();
        assert!(
            e.tracks[0].is_param_pending(flat),
            "a 100-LSB jump must still be moving"
        );

        // Decrement past the bottom rail clamps there.
        for _ in 0..200 {
            feed_nrpn(&mut p, &mut e, &[0xB0, CC_DATA_DECREMENT, 127]);
        }
        converge_params(&mut e, 0);
        assert_eq!(
            e.tracks[0].base_params[flat], 0.0,
            "clamped at the bottom rail"
        );

        // Increment past the top rail clamps at full scale.
        for _ in 0..200 {
            feed_nrpn(&mut p, &mut e, &[0xB0, CC_DATA_INCREMENT, 127]);
        }
        converge_params(&mut e, 0);
        assert_eq!(
            e.tracks[0].base_params[flat], 1.0,
            "200 steps of 127 pass 16383 and clamp at full scale"
        );
    }

    /// A discrete parameter is addressed by option, not by raw value.
    ///
    /// The catalogue has 15 machines, so option `k` is the exact value
    /// `k / 14`. Anything inside option `k`'s slice of the 14-bit range
    /// must land on it and store that exact value back, so reading the
    /// parameter and re-sending it is a fixed point rather than a slow
    /// drift down the catalogue.
    #[test]
    fn nrpn_snaps_a_discrete_param_onto_an_exact_option() {
        use crate::{DrumEngine, MachineId, SLOT_MACHINE};
        let steps = (MachineId::COUNT - 1) as f32;

        for option in 0..MachineId::COUNT {
            // Dead centre of the option's slice, and both edges of it.
            let centre = (option as f32 / steps * NRPN_MAX as f32) as u16;
            let half_slice = (NRPN_MAX as f32 / steps / 2.0) as u16;
            for raw in [
                centre,
                centre.saturating_sub(half_slice.saturating_sub(1)),
                (centre + half_slice.saturating_sub(1)).min(NRPN_MAX),
            ] {
                let mut e = DrumEngine::new();
                let mut p = MidiParser::new();
                feed_nrpn(
                    &mut p,
                    &mut e,
                    &nrpn_bytes(0, 0, (raw >> 7) as u8, (raw & 0x7F) as u8),
                );
                assert_eq!(
                    e.tracks[0].id().index(),
                    option,
                    "raw {raw} should select machine {option}"
                );
                approx::assert_abs_diff_eq!(
                    e.tracks[0].base_params[SLOT_MACHINE],
                    option as f32 / steps,
                    epsilon = 1e-6
                );
            }
        }
    }

    /// A step on a discrete parameter moves one option, whatever magnitude
    /// the controller sent: one detent of an encoder is one machine. A
    /// magnitude in LSBs would round back to the same option and the
    /// selector would feel dead.
    #[test]
    fn nrpn_step_on_a_discrete_param_moves_one_option() {
        use crate::{DrumEngine, MachineId};
        let mut e = DrumEngine::new();
        let mut p = MidiParser::new();
        assert_eq!(e.tracks[0].id().index(), 0);

        // Select the machine param, then step it with a magnitude that, read
        // as LSBs, would not even cross one option's width.
        feed_nrpn(
            &mut p,
            &mut e,
            &[0xB0, CC_NRPN_PARAM_MSB, 0, CC_NRPN_PARAM_LSB, 0],
        );
        for expected in 1..4 {
            feed_nrpn(&mut p, &mut e, &[0xB0, CC_DATA_INCREMENT, 1]);
            assert_eq!(e.tracks[0].id().index(), expected);
        }
        // A large accelerated step is still one option: the catalogue is a
        // list, not a continuum, and the controller's curve means nothing
        // here.
        feed_nrpn(&mut p, &mut e, &[0xB0, CC_DATA_INCREMENT, 127]);
        assert_eq!(e.tracks[0].id().index(), 4);

        feed_nrpn(&mut p, &mut e, &[0xB0, CC_DATA_DECREMENT, 1]);
        assert_eq!(e.tracks[0].id().index(), 3);

        // Clamped at the bottom.
        for _ in 0..10 {
            feed_nrpn(&mut p, &mut e, &[0xB0, CC_DATA_DECREMENT, 1]);
        }
        assert_eq!(e.tracks[0].id().index(), 0);

        // And at the top.
        for _ in 0..(MachineId::COUNT + 5) {
            feed_nrpn(&mut p, &mut e, &[0xB0, CC_DATA_INCREMENT, 1]);
        }
        assert_eq!(e.tracks[0].id().index(), MachineId::COUNT - 1);
    }

    /// An address the device cannot answer resolves nothing: an index past
    /// its group, a group this build has no identifier for, and a channel
    /// with no track are all dropped without touching a param.
    #[test]
    fn nrpn_addresses_nothing_is_dropped_silently() {
        use crate::DrumEngine;
        use device_core::params::{resolve, ParamGroup, ParamId};
        use device_core::slot::DeviceModel;
        let mut e = DrumEngine::new();
        let mut p = MidiParser::new();
        let table = e.tracks[0].id().param_table();
        let flat = resolve(
            table,
            ParamId {
                group: ParamGroup::Voice,
                index: 0,
            },
        )
        .expect("VOICE 0 on the default machine");
        let before = e.tracks[0].base_params[flat];

        // Index past the group's members.
        feed_nrpn(
            &mut p,
            &mut e,
            &nrpn_bytes(ParamGroup::Voice as u8, 40, 100, 100),
        );
        converge_params(&mut e, 0);
        assert_eq!(e.tracks[0].base_params[flat], before);

        // A group this build has no identifier for: the parser emits it,
        // the router drops it.
        feed_nrpn(&mut p, &mut e, &nrpn_bytes(8, 0, 100, 100));
        converge_params(&mut e, 0);
        assert_eq!(e.tracks[0].base_params[flat], before);

        // A channel with no track (drum routes channels 1..=8 to tracks).
        feed_nrpn(
            &mut p,
            &mut e,
            &[
                0xBF,
                CC_NRPN_PARAM_MSB,
                1,
                0xBF,
                CC_NRPN_PARAM_LSB,
                0,
                0xBF,
                CC_DATA_ENTRY_MSB,
                100,
            ],
        );
        converge_params(&mut e, 0);
        assert_eq!(e.tracks[0].base_params[flat], before);
    }
}
