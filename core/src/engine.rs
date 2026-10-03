//! Generic device engine.
//!
//! An [`Engine`] owns a fixed bank of tracks, each loaded with a slot that
//! implements [`Slot`](crate::slot::Slot). It provides triggering, MIDI note
//! mapping, sample-accurate event scheduling, and block rendering —
//! everything a synth device needs, independent of the specific voice model.
//!
//! The [`DeviceEngine`] trait exposes the same surface so firmware, render,
//! and MIDI routers can be generic over any device engine implementation.

use crate::dsp;
use crate::midi::CHROMATIC_REFERENCE_NOTE;
use crate::slot::Slot;
use crate::sound::Sound;
use crate::track::Track;
use crate::{EngineEvent, TimedQueue, BLOCK};

/// Trait surface shared by every device engine.
///
/// Consumers (firmware runner, render harness, MIDI router) bind to this
/// rather than to a concrete engine type. `N` is the device's macro count.
pub trait DeviceEngine<const N: usize> {
    /// Slot type hosted on each track.
    type Slot: Slot<N>;

    /// Read-only access to the track array.
    fn tracks(&self) -> &[Track<Self::Slot, N>];

    /// Mutable access to the track array.
    fn tracks_mut(&mut self) -> &mut [Track<Self::Slot, N>];

    /// Trigger a single track by index, applying its layer/choke masks.
    fn trigger(&mut self, track: usize, velocity: f32);

    /// Resolve a MIDI note through the note map and trigger it.
    fn trigger_note(&mut self, note: u8, velocity: f32) -> Option<usize>;

    /// Chromatic trigger for the one-channel-per-track path.
    fn trigger_channel(&mut self, channel: u8, note: u8, velocity: f32) -> Option<usize>;

    /// Close the gate on a track by index. The note-off counterpart to
    /// [`Self::trigger`].
    fn release(&mut self, track: usize);

    /// Release a track resolved through the note map.
    fn release_note(&mut self, note: u8) -> Option<usize>;

    /// Close the gate on a track selected by MIDI channel — the note-off
    /// counterpart to [`Self::trigger_channel`], with the same
    /// channel-to-track mapping.
    fn release_channel(&mut self, channel: u8, note: u8) -> Option<usize>;

    /// Map a MIDI note to a track, or unmap it.
    fn set_note(&mut self, note: u8, track: Option<u8>);

    /// Silence every track immediately.
    fn panic(&mut self);

    /// Schedule an event to fire `offset` samples into the next process block.
    fn schedule_timed(&mut self, offset: usize, event: EngineEvent) -> bool;

    /// Load a complete sound onto a track.
    fn load_sound(&mut self, track: usize, sound: &Sound<Self::Slot, N>);

    /// Load a sound onto a track and trigger it in the same call.
    fn trigger_with_sound(&mut self, track: usize, velocity: f32, sound: &Sound<Self::Slot, N>);

    /// True if any track is still producing output.
    fn is_active(&self) -> bool;

    /// Render one block into planar stereo buffers.
    fn process(&mut self, out_l: &mut [f32], out_r: &mut [f32]);

    /// Render one block into master/aux/wet buffers (pre-master-gain).
    fn process_dry_wet(
        &mut self,
        master_l: &mut [f32; BLOCK],
        master_r: &mut [f32; BLOCK],
        aux: &mut [[f32; BLOCK]; 6],
        wet_l: &mut [f32; BLOCK],
        wet_r: &mut [f32; BLOCK],
    );

    /// Current master gain, linear.
    fn master_gain(&self) -> f32;

    /// Set master gain, linear.
    fn set_master_gain(&mut self, value: f32);

    /// Current send-FX bus drive, linear.
    fn fx_drive(&self) -> f32;

    /// Set send-FX bus drive, linear.
    fn set_fx_drive(&mut self, value: f32);

    /// Load a whole kit in one call. The slice length must equal the engine's
    /// track count; implementations may panic on mismatch.
    fn load_kit(&mut self, kit: &[<Self::Slot as Slot<N>>::Id]);
}

/// The device engine: generic over the slot type, macro count, and track count.
///
/// Construct once, hold for the lifetime of the program. Everything it needs
/// lives inside it.
pub struct Engine<S: Slot<N>, const N: usize, const T: usize> {
    /// All tracks, indexed by their position in the kit.
    pub tracks: [Track<S, N>; T],
    /// Send-FX bus (delay + reverb). Drained by [`Self::process`] after the
    /// dry sample sum and before master clip.
    pub send_fx: dsp::SendFx,
    /// Note-number → track index, for the programmatic [`Self::trigger_note`]
    /// path. `None` = no track handles this note.
    pub note_map: [Option<u8>; 128],
    /// Sample-accurate event schedule for the next block. The main loop
    /// pushes NoteOns here with a sample offset; [`Self::process`] drains it.
    pub timed: TimedQueue,
    /// Post-sum, pre-output limiter gain, linear.
    pub master_gain: f32,
}

fn build_tracks<S: Slot<N>, const N: usize, const T: usize>(kit: &[S::Id; T]) -> [Track<S, N>; T] {
    core::array::from_fn(|i| Track::new(kit[i]))
}

impl<S: Slot<N>, const N: usize, const T: usize> Engine<S, N, T> {
    /// Build an engine with the given kit. `note_map` is initialized to all
    /// `None`; the caller is responsible for filling it.
    pub fn new_with_kit(kit: &[S::Id; T]) -> Self {
        Self {
            tracks: build_tracks(kit),
            send_fx: dsp::SendFx::new(),
            note_map: [None; 128],
            timed: TimedQueue::new(),
            master_gain: 0.8,
        }
    }

    /// Initialize an engine in-place at the given pointer. Used by
    /// firmware that places the engine in a `.uninit` static.
    ///
    /// # Safety
    ///
    /// `dst` must point to writable memory of at least
    /// `size_of::<Engine<S,N,T>>` bytes, valid for the lifetime of the
    /// returned reference. The memory need not be zeroed — this function
    /// writes every field.
    ///
    /// Tracks are constructed one at a time directly in the destination array
    /// so the whole `[Track; T]` is never materialised on the stack. This is
    /// essential when the slot type is large or self-referential (e.g. a
    /// block-buffered Plaits voice).
    #[allow(unsafe_code)]
    pub unsafe fn new_in_place_with_kit<'a>(
        dst: *mut Engine<S, N, T>,
        kit: &[S::Id; T],
    ) -> &'a mut Engine<S, N, T> {
        let tracks_ptr = core::ptr::addr_of_mut!((*dst).tracks);
        for (i, &id) in kit.iter().enumerate() {
            let track_ptr = core::ptr::addr_of_mut!((*tracks_ptr)[i]);
            Track::new_in_place(id, track_ptr);
        }
        dsp::SendFx::new_in_place(core::ptr::addr_of_mut!((*dst).send_fx));
        core::ptr::addr_of_mut!((*dst).note_map).write([None; 128]);
        core::ptr::addr_of_mut!((*dst).timed).write(TimedQueue::new());
        core::ptr::addr_of_mut!((*dst).master_gain).write(0.8);
        &mut *dst
    }

    /// Replace the entire kit one track at a time. Keeps strip + macro
    /// configurations on untouched tracks intact.
    pub fn load_kit(&mut self, kit: &[S::Id; T]) {
        for (i, id) in kit.iter().enumerate() {
            self.tracks[i].load_machine(*id);
        }
    }

    /// Trigger a single track by index. Applies that track's `layer_mask`
    /// and chokes the tracks named in its `choke_mask`.
    pub fn trigger(&mut self, track: usize, velocity: f32) {
        let layer = self.tracks[track].strip.layer_mask;
        let choke = self.tracks[track].strip.choke_mask;

        let mut i = 0;
        while i < T {
            if (layer & (1 << i)) != 0 && i != track {
                self.tracks[i].trigger(velocity);
            }
            i += 1;
        }
        let mut j = 0;
        while j < T {
            if (choke & (1 << j)) != 0 && j != track {
                self.tracks[j].choke();
            }
            j += 1;
        }
        self.tracks[track].trigger(velocity);
    }

    /// Pull a MIDI note into the engine: resolve `note` through [`note_map`]
    /// to a track, and if there is one, trigger it.
    ///
    /// Returns `Some(track)` when the note landed, `None` when nothing is
    /// mapped to it.
    pub fn trigger_note(&mut self, note: u8, velocity: f32) -> Option<usize> {
        let track = self.note_map[note as usize]?;
        self.trigger(track as usize, velocity);
        Some(track as usize)
    }

    /// Chromatic trigger for the one-channel-per-track MIDI path.
    ///
    /// The MIDI channel selects the track (`channel` < `T`); the note
    /// number sets its pitch — [`CHROMATIC_REFERENCE_NOTE`] (middle C)
    /// is the machine's macro-configured pitch, and each semitone away
    /// transposes the whole voice via [`Track::retune`] before the hit.
    ///
    /// Returns `Some(track)` when the note landed, `None` when the channel
    /// has no track.
    pub fn trigger_channel(&mut self, channel: u8, note: u8, velocity: f32) -> Option<usize> {
        let track = channel as usize;
        if track >= T {
            return None;
        }
        let semis = note as f32 - CHROMATIC_REFERENCE_NOTE as f32;
        self.tracks[track].retune(semis);
        self.trigger(track, velocity);
        Some(track)
    }

    /// Map a MIDI note to a track. `None` means unassigned (default).
    pub fn set_note(&mut self, note: u8, track: Option<u8>) {
        self.note_map[note as usize] = track;
    }

    /// Close the gate on a single track by index.
    ///
    /// Unlike [`Self::trigger`], this does not consult `layer_mask` or
    /// `choke_mask`. Those describe what a *hit* does to other tracks; a
    /// release is about this one voice, and firing a choke on note-off would
    /// cut an unrelated track that happened to be sounding.
    pub fn release(&mut self, track: usize) {
        if track < T {
            self.tracks[track].release();
        }
    }

    /// Release the track `note` is mapped to, if any.
    pub fn release_note(&mut self, note: u8) -> Option<usize> {
        let track = self.note_map[note as usize]?;
        self.release(track as usize);
        Some(track as usize)
    }

    /// Close the gate on a track selected by MIDI channel, the note-off
    /// counterpart to [`Self::trigger_channel`].
    ///
    /// Returns `Some(track)` when the channel has one, `None` when it does
    /// not. The note number is accepted for symmetry with
    /// [`Self::trigger_channel`] and is not consulted: a track holds exactly
    /// one voice, so the channel alone identifies what to release.
    pub fn release_channel(&mut self, channel: u8, _note: u8) -> Option<usize> {
        let track = channel as usize;
        if track >= T {
            return None;
        }
        self.release(track);
        Some(track)
    }

    /// Silence every track immediately.
    pub fn panic(&mut self) {
        for t in self.tracks.iter_mut() {
            t.reset();
        }
        self.send_fx.reset();
    }

    /// Schedule an event to fire `offset` samples into the next
    /// [`Self::process`] block. Returns false if the queue is full.
    pub fn schedule_timed(&mut self, offset: usize, event: EngineEvent) -> bool {
        self.timed.push(offset, event)
    }

    /// Load a complete [`Sound`] onto a track.
    pub fn load_sound(&mut self, track: usize, sound: &Sound<S, N>) {
        self.tracks[track].load_sound(sound);
    }

    /// Load a [`Sound`] onto a track *and* trigger it in the same call.
    pub fn trigger_with_sound(&mut self, track: usize, velocity: f32, sound: &Sound<S, N>) {
        self.tracks[track].load_sound(sound);
        let layer = self.tracks[track].strip.layer_mask;
        let choke = self.tracks[track].strip.choke_mask;

        let mut i = 0;
        while i < T {
            if (layer & (1 << i)) != 0 && i != track {
                self.tracks[i].trigger(velocity);
            }
            i += 1;
        }
        let mut j = 0;
        while j < T {
            if (choke & (1 << j)) != 0 && j != track {
                self.tracks[j].reset();
            }
            j += 1;
        }
        self.tracks[track].trigger(velocity);
    }

    /// True if any track is still producing output.
    pub fn is_active(&self) -> bool {
        self.tracks.iter().any(|t| t.is_active())
    }

    /// Render one block into planar stereo buffers.
    ///
    /// # Panics
    ///
    /// Debug builds assert both slices are exactly [`BLOCK`] long.
    pub fn process(&mut self, out_l: &mut [f32], out_r: &mut [f32]) {
        debug_assert_eq!(out_l.len(), BLOCK, "left buffer must be BLOCK frames");
        debug_assert_eq!(out_r.len(), BLOCK, "right buffer must be BLOCK frames");

        let mut master_l = [0.0f32; BLOCK];
        let mut master_r = [0.0f32; BLOCK];
        let mut aux = [[0.0f32; BLOCK]; 6];
        let mut wet_l = [0.0f32; BLOCK];
        let mut wet_r = [0.0f32; BLOCK];
        self.process_dry_wet(
            &mut master_l,
            &mut master_r,
            &mut aux,
            &mut wet_l,
            &mut wet_r,
        );

        let n = out_l.len().min(out_r.len()).min(BLOCK);
        let master = self.master_gain;
        let fx_drive = self.send_fx.drive;

        for i in 0..n {
            let wet_l_clipped = dsp::fast::soft_clip(wet_l[i] * fx_drive);
            let wet_r_clipped = dsp::fast::soft_clip(wet_r[i] * fx_drive);
            out_l[i] = dsp::fast::soft_clip((master_l[i] + wet_l_clipped) * master);
            out_r[i] = dsp::fast::soft_clip((master_r[i] + wet_r_clipped) * master);
        }
    }

    /// Render one block into a master dry pair, three stereo aux pairs, and a
    /// shared wet-FX return pair.
    ///
    /// All five outputs are pre-everything: no FX-bus drive, no master gain,
    /// no master clip.
    ///
    /// # Panics
    ///
    /// Debug builds assert `aux` is exactly 6 entries of [`BLOCK`] frames.
    pub fn process_dry_wet(
        &mut self,
        master_l: &mut [f32; BLOCK],
        master_r: &mut [f32; BLOCK],
        aux: &mut [[f32; BLOCK]; 6],
        wet_l: &mut [f32; BLOCK],
        wet_r: &mut [f32; BLOCK],
    ) {
        let mut timed = [None::<crate::TimedEvent>; crate::MAX_TIMED_EVENTS];
        let n_timed = self.timed.drain_sorted(&mut timed);

        for t in self.tracks.iter_mut() {
            t.control();
        }

        for s in master_l.iter_mut() {
            *s = 0.0;
        }
        for s in master_r.iter_mut() {
            *s = 0.0;
        }
        for bus in aux.iter_mut() {
            for s in bus.iter_mut() {
                *s = 0.0;
            }
        }

        let mut send_dl = [0.0f32; BLOCK];
        let mut send_dr = [0.0f32; BLOCK];
        let mut send_rl = [0.0f32; BLOCK];
        let mut send_rr = [0.0f32; BLOCK];

        // Split the block into segments at timed-event boundaries so that
        // block-rate slots can render each contiguous run in one call while
        // still honouring sample-accurate triggers.
        let mut segment_ends = [BLOCK; crate::MAX_TIMED_EVENTS + 1];
        let mut n_segments = 0;
        let mut last_end = 0;
        for k in 0..n_timed {
            let offset = timed[k].expect("drained entries are Some").offset;
            if offset > last_end && offset <= BLOCK {
                segment_ends[n_segments] = offset;
                n_segments += 1;
                last_end = offset;
            }
        }
        if last_end < BLOCK {
            segment_ends[n_segments] = BLOCK;
            n_segments += 1;
        }

        let mut k = 0;
        let mut start = 0;
        for seg in 0..n_segments {
            let end = segment_ends[seg];

            // Process every timed event at this segment boundary.
            while k < n_timed {
                let ev = timed[k].expect("drained entries are Some");
                if ev.offset != start {
                    break;
                }
                match ev.event {
                    EngineEvent::NoteOn {
                        channel,
                        note,
                        velocity,
                    } => {
                        self.trigger_channel(channel, note, velocity);
                    }
                    EngineEvent::NoteOff { channel, note } => {
                        self.release_channel(channel, note);
                    }
                    EngineEvent::Panic => self.panic(),
                }
                k += 1;
            }

            let n = end - start;

            // Collect source samples for this segment, interleaved across
            // tracks. Voices that share a process-global random generator
            // (e.g. Plaits) must stay sample-interleaved to preserve the
            // exact draw sequence and keep renders bit-identical.
            for i in 0..n {
                let mut t = 0;
                while t < T {
                    self.tracks[t].source_segment[i] = if self.tracks[t].is_active() {
                        self.tracks[t].slot.tick()
                    } else {
                        0.0
                    };
                    t += 1;
                }
            }

            // Let device-specific slots apply their own audio strip (e.g. mi-
            // drum's Warps -> Ripples) to the collected source segment.
            let mut t = 0;
            while t < T {
                if self.tracks[t].is_active() {
                    self.tracks[t]
                        .slot
                        .process_audio_strip(&mut self.tracks[t].source_segment[..n], start);
                }
                t += 1;
            }

            // Apply pan/level/sends/choke/de-click to each track's segment.
            let mut t = 0;
            while t < T {
                if self.tracks[t].is_active() {
                    self.tracks[t].process_segment(
                        start,
                        n,
                        master_l,
                        master_r,
                        aux,
                        &mut send_dl,
                        &mut send_dr,
                        &mut send_rl,
                        &mut send_rr,
                    );
                }
                t += 1;
            }

            start = end;
        }

        let mut wet_dl = [0.0f32; BLOCK];
        let mut wet_dr = [0.0f32; BLOCK];
        let mut wet_rl = [0.0f32; BLOCK];
        let mut wet_rr = [0.0f32; BLOCK];
        self.send_fx
            .delay
            .process_block(&send_dl, &send_dr, &mut wet_dl, &mut wet_dr);
        self.send_fx
            .reverb
            .process_block(&send_rl, &send_rr, &mut wet_rl, &mut wet_rr);
        for i in 0..BLOCK {
            wet_l[i] = wet_dl[i] + wet_rl[i];
            wet_r[i] = wet_dr[i] + wet_rr[i];
        }
    }
}

impl<S: Slot<N>, const N: usize, const T: usize> DeviceEngine<N> for Engine<S, N, T> {
    type Slot = S;

    fn tracks(&self) -> &[Track<S, N>] {
        &self.tracks
    }

    fn tracks_mut(&mut self) -> &mut [Track<S, N>] {
        &mut self.tracks
    }

    fn trigger(&mut self, track: usize, velocity: f32) {
        Engine::trigger(self, track, velocity);
    }

    fn trigger_note(&mut self, note: u8, velocity: f32) -> Option<usize> {
        Engine::trigger_note(self, note, velocity)
    }

    fn trigger_channel(&mut self, channel: u8, note: u8, velocity: f32) -> Option<usize> {
        Engine::trigger_channel(self, channel, note, velocity)
    }

    fn release(&mut self, track: usize) {
        Engine::release(self, track)
    }

    fn release_note(&mut self, note: u8) -> Option<usize> {
        Engine::release_note(self, note)
    }

    fn release_channel(&mut self, channel: u8, note: u8) -> Option<usize> {
        Engine::release_channel(self, channel, note)
    }

    fn set_note(&mut self, note: u8, track: Option<u8>) {
        Engine::set_note(self, note, track);
    }

    fn panic(&mut self) {
        Engine::panic(self);
    }

    fn schedule_timed(&mut self, offset: usize, event: EngineEvent) -> bool {
        Engine::schedule_timed(self, offset, event)
    }

    fn load_sound(&mut self, track: usize, sound: &Sound<S, N>) {
        Engine::load_sound(self, track, sound);
    }

    fn trigger_with_sound(&mut self, track: usize, velocity: f32, sound: &Sound<S, N>) {
        Engine::trigger_with_sound(self, track, velocity, sound);
    }

    fn is_active(&self) -> bool {
        Engine::is_active(self)
    }

    fn process(&mut self, out_l: &mut [f32], out_r: &mut [f32]) {
        Engine::process(self, out_l, out_r);
    }

    fn process_dry_wet(
        &mut self,
        master_l: &mut [f32; BLOCK],
        master_r: &mut [f32; BLOCK],
        aux: &mut [[f32; BLOCK]; 6],
        wet_l: &mut [f32; BLOCK],
        wet_r: &mut [f32; BLOCK],
    ) {
        Engine::process_dry_wet(self, master_l, master_r, aux, wet_l, wet_r);
    }

    fn master_gain(&self) -> f32 {
        self.master_gain
    }

    fn set_master_gain(&mut self, value: f32) {
        self.master_gain = value;
    }

    fn fx_drive(&self) -> f32 {
        self.send_fx.drive
    }

    fn set_fx_drive(&mut self, value: f32) {
        self.send_fx.drive = value;
    }

    fn load_kit(&mut self, kit: &[S::Id]) {
        Engine::load_kit(
            self,
            kit.try_into().expect("kit length must match track count"),
        );
    }
}

/// Helper to configure a default GM-percussion note map.
///
/// Maps acoustic bass drum → 0, snare → 1, closed hat → 2, open hat → 3,
/// clap → 4, high tom → 5, cowbell → 6, middle C → 7. Everything else is
/// left unassigned. Device crates call this after constructing the engine if
/// they want the drum-style defaults.
pub fn configure_default_notes(map: &mut [Option<u8>; 128]) {
    map[36] = Some(0); // Acoustic bass drum → track 0
    map[38] = Some(1); // Acoustic snare → track 1
    map[42] = Some(2); // Closed hat → track 2
    map[46] = Some(3); // Open hat → track 3
    map[39] = Some(4); // Hand clap → track 4
    map[50] = Some(5); // High tom → track 5
    map[56] = Some(6); // Cowbell → track 6
    map[60] = Some(7); // Middle C → track 7
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::macros::NUM_MACROS;
    use crate::slot::{Slot, SlotId};

    /// A dummy slot for testing the generic engine without pulling in the drum
    /// catalogue.
    #[derive(Clone, Copy, PartialEq, Eq, Debug)]
    struct DummyId(usize);

    impl SlotId<NUM_MACROS> for DummyId {
        fn index(self) -> usize {
            self.0
        }
        fn from_index(i: usize) -> Option<Self> {
            if i < 2 {
                Some(DummyId(i))
            } else {
                None
            }
        }
        fn count() -> usize {
            2
        }
        fn default_macros(self) -> [f32; NUM_MACROS] {
            [0.0; NUM_MACROS]
        }
    }

    struct DummySlot {
        active: bool,
    }

    impl Slot<NUM_MACROS> for DummySlot {
        type Id = DummyId;
        fn new(_id: Self::Id, _macros: &[f32; NUM_MACROS]) -> Self {
            Self { active: false }
        }
        fn id(&self) -> Self::Id {
            DummyId(0)
        }
        fn set_macros(&mut self, _macros: &[f32; NUM_MACROS]) {}
        fn trigger(&mut self, _velocity: f32) {
            self.active = true;
        }
        fn retune(&mut self, _semis: f32) {}
        fn reset(&mut self) {
            self.active = false;
        }
        fn is_active(&self) -> bool {
            self.active
        }
        fn tick(&mut self) -> f32 {
            if self.active {
                0.1
            } else {
                0.0
            }
        }
    }

    /// A slot that sustains until released, so the note-off path has something
    /// to actually close. `DummySlot` is a one-shot, which would let a broken
    /// `release` pass every "did the engine stop" test.
    #[derive(Clone, Copy, PartialEq, Eq, Debug)]
    struct GateId(usize);

    impl SlotId<NUM_MACROS> for GateId {
        fn index(self) -> usize {
            self.0
        }
        fn from_index(i: usize) -> Option<Self> {
            if i < 2 {
                Some(GateId(i))
            } else {
                None
            }
        }
        fn count() -> usize {
            2
        }
        fn default_macros(self) -> [f32; NUM_MACROS] {
            [0.0; NUM_MACROS]
        }
    }

    struct GatedSlot {
        gate: bool,
        released: u32,
    }

    impl Slot<NUM_MACROS> for GatedSlot {
        type Id = GateId;
        fn new(_id: Self::Id, _macros: &[f32; NUM_MACROS]) -> Self {
            Self {
                gate: false,
                released: 0,
            }
        }
        fn id(&self) -> Self::Id {
            GateId(0)
        }
        fn set_macros(&mut self, _macros: &[f32; NUM_MACROS]) {}
        fn trigger(&mut self, _velocity: f32) {
            self.gate = true;
        }
        fn release(&mut self) {
            self.released += 1;
            self.gate = false;
        }
        fn retune(&mut self, _semis: f32) {}
        fn reset(&mut self) {
            self.gate = false;
        }
        fn is_active(&self) -> bool {
            self.gate
        }
        fn tick(&mut self) -> f32 {
            if self.gate {
                0.5
            } else {
                0.0
            }
        }
    }

    #[test]
    fn generic_engine_processes_without_nan() {
        let mut engine =
            Engine::<DummySlot, NUM_MACROS, 2>::new_with_kit(&[DummyId(0), DummyId(1)]);
        engine.trigger(0, 1.0);
        let mut l = [0.0f32; BLOCK];
        let mut r = [0.0f32; BLOCK];
        for _ in 0..10 {
            engine.process(&mut l, &mut r);
        }
        for &s in l.iter().chain(r.iter()) {
            assert!(s.is_finite());
        }
    }

    // ----- note-off / gate close -----

    /// A one-shot slot ignores `release`: the default no-op is the correct
    /// behaviour for a drum voice, and pinning it here is what stops a later
    /// "make release universal" change from silently shortening every hit.
    #[test]
    fn one_shot_slot_ignores_release() {
        let mut engine =
            Engine::<DummySlot, NUM_MACROS, 2>::new_with_kit(&[DummyId(0), DummyId(1)]);
        engine.trigger(0, 1.0);
        assert!(engine.tracks[0].is_active());
        engine.release(0);
        assert!(
            engine.tracks[0].is_active(),
            "a one-shot hit must survive a note-off"
        );
    }

    #[test]
    fn release_closes_a_gated_voice() {
        let mut engine = Engine::<GatedSlot, NUM_MACROS, 2>::new_with_kit(&[GateId(0), GateId(1)]);
        engine.trigger(0, 1.0);
        engine.trigger(1, 1.0);
        assert!(engine.is_active());

        engine.release(1);
        assert!(!engine.tracks[1].is_active(), "gate should be closed");
        assert!(
            engine.tracks[0].is_active(),
            "releasing one track must not touch another"
        );
    }

    #[test]
    fn release_out_of_range_is_a_noop() {
        let mut engine = Engine::<GatedSlot, NUM_MACROS, 2>::new_with_kit(&[GateId(0), GateId(1)]);
        engine.trigger(0, 1.0);
        // Would panic on an unchecked index.
        engine.release(7);
        assert!(engine.tracks[0].is_active());
    }

    #[test]
    fn release_channel_maps_channel_to_track() {
        let mut engine = Engine::<GatedSlot, NUM_MACROS, 2>::new_with_kit(&[GateId(0), GateId(1)]);
        assert_eq!(engine.release_channel(1, 60), Some(1));
        assert_eq!(
            engine.release_channel(9, 60),
            None,
            "no track for channel 9"
        );
    }

    #[test]
    fn release_channel_routes_the_midi_stream() {
        // The end-to-end shape: bytes in, gate closes on the right track.
        let mut engine = Engine::<GatedSlot, NUM_MACROS, 2>::new_with_kit(&[GateId(0), GateId(1)]);
        let mut parser = crate::midi::MidiParser::new();
        for b in [0x91, 60, 127] {
            if let Some(ev) = parser.push(b) {
                crate::midi::handle_midi(&mut engine, ev);
            }
        }
        assert!(engine.tracks[1].is_active(), "note-on should sound track 1");

        for b in [0x81, 60, 0] {
            if let Some(ev) = parser.push(b) {
                crate::midi::handle_midi(&mut engine, ev);
            }
        }
        assert!(!engine.tracks[1].is_active(), "note-off should close it");
    }

    #[test]
    fn release_through_the_note_map() {
        let mut engine = Engine::<GatedSlot, NUM_MACROS, 2>::new_with_kit(&[GateId(0), GateId(1)]);
        engine.set_note(36, Some(1));
        engine.trigger(1, 1.0);
        assert_eq!(engine.release_note(36), Some(1));
        assert!(!engine.tracks[1].is_active());
        assert_eq!(engine.release_note(72), None, "unmapped note");
    }

    /// A held note has to last. This is the regression the whole note-off path
    /// exists for: a gate that closes on its own is indistinguishable from no
    /// gate at all.
    #[test]
    fn a_gated_voice_sustains_until_released() {
        let mut engine = Engine::<GatedSlot, NUM_MACROS, 2>::new_with_kit(&[GateId(0), GateId(1)]);
        let mut l = [0.0f32; BLOCK];
        let mut r = [0.0f32; BLOCK];
        engine.trigger(0, 1.0);

        // Five seconds of held note, well past any one-shot envelope.
        let held = (5.0 * crate::SAMPLE_RATE / BLOCK as f32) as usize;
        for _ in 0..held {
            engine.process(&mut l, &mut r);
            assert!(
                l.iter().any(|&s| s != 0.0),
                "the voice stopped sounding while the key was still down"
            );
        }
        assert!(engine.tracks[0].is_active());

        engine.release(0);
        for _ in 0..4 {
            engine.process(&mut l, &mut r);
        }
        assert!(
            l.iter().chain(r.iter()).all(|&s| s == 0.0),
            "the voice kept sounding after the gate closed"
        );
    }

    /// A release scheduled at an offset must land at that sample, exactly like
    /// a note-on does. The `timed_note_fires_at_its_offset` test in the drum
    /// crate pins the note-on half; this is the other half.
    #[test]
    fn timed_note_off_fires_at_its_offset() {
        let mut e = Engine::<GatedSlot, NUM_MACROS, 2>::new_with_kit(&[GateId(0), GateId(1)]);
        e.trigger(0, 1.0);
        e.schedule_timed(
            10,
            crate::EngineEvent::NoteOff {
                channel: 0,
                note: 60,
            },
        );

        let mut l = [0.0f32; BLOCK];
        let mut r = [0.0f32; BLOCK];
        e.process(&mut l, &mut r);

        assert!(
            l[..10].iter().any(|&s| s != 0.0),
            "should still be sounding before the release"
        );
        for i in 10..BLOCK {
            assert_eq!(l[i], 0.0, "gate stayed open past its offset ({i})");
            assert_eq!(r[i], 0.0, "gate stayed open past its offset ({i})");
        }
        assert!(e.timed.is_empty(), "process() must drain the queue");
    }

    /// The de-click crossfade must not survive a release. If it did, a
    /// sustained note would crossfade into its own pre-release level on every
    /// note-off and re-audibilise the level it was releasing from.
    #[test]
    fn release_does_not_arm_the_declick_crossfade() {
        let mut engine = Engine::<GatedSlot, NUM_MACROS, 2>::new_with_kit(&[GateId(0), GateId(1)]);
        let mut l = [0.0f32; BLOCK];
        let mut r = [0.0f32; BLOCK];
        engine.trigger(0, 1.0);
        for _ in 0..4 {
            engine.process(&mut l, &mut r);
        }
        engine.release(0);
        for _ in 0..8 {
            engine.process(&mut l, &mut r);
            for &s in l.iter().chain(r.iter()) {
                assert_eq!(s, 0.0, "release left a crossfade running");
            }
        }
    }

    #[test]
    fn panic_still_cuts_a_gated_voice() {
        let mut engine = Engine::<GatedSlot, NUM_MACROS, 2>::new_with_kit(&[GateId(0), GateId(1)]);
        engine.trigger(0, 1.0);
        engine.panic();
        assert!(!engine.is_active(), "panic must still be instant silence");
    }
}
