//! Shared core for synth devices.
//!
//! This crate is `#![no_std]`, never allocates, and contains only the parts
//! that are reusable across devices: DSP primitives, the macro/CC slot system,
//! a MIDI parser, and timing/event plumbing.
//!
//! Device-specific code — machine catalogs, engine structs, MIDI routing,
//! and the grid screens — lives in per-device engine crates.

#![no_std]
#![deny(unsafe_code)]
#![warn(missing_docs)]

pub mod dsp;
pub mod engine;
pub mod macros;
pub mod midi;
pub mod slot;
pub mod sound;
pub mod strip;
pub mod track;

#[cfg(feature = "grid")]
pub mod grid;

/// Sample rate the engine is built for.
///
/// Fixed rather than runtime-configurable on purpose: every filter and
/// envelope coefficient is derived from it, and making it dynamic means
/// either recomputing coefficients on the fly or carrying a divide into the
/// hot path. Change it here and rebuild.
pub const SAMPLE_RATE: f32 = 48_000.0;

/// Reciprocal of the sample rate, precomputed.
///
/// Multiply by this instead of dividing by [`SAMPLE_RATE`]. A single-precision
/// divide is multi-cycle on an M7 and there is no reason to pay for one per
/// sample.
pub const INV_SAMPLE_RATE: f32 = 1.0 / SAMPLE_RATE;

/// Frames per processing block.
///
/// 32 frames at 48kHz is 667µs of headroom per callback. Smaller means lower
/// latency and more per-callback overhead; larger means the opposite. This is
/// the number your cycle budget is measured against.
pub const BLOCK: usize = 32;

/// Anything below this magnitude is flushed to zero.
///
/// Long envelope and filter tails decay toward denormal floats, which on some
/// cores trap to microcode and cost orders of magnitude more than a normal
/// operation. The symptom is a synth that gets slower the longer it runs.
/// Cheaper to clamp than to debug.
pub const DENORMAL_FLOOR: f32 = 1.0e-9;

/// Output pair a track routes to.
///
/// A device has four stereo output pairs: the master mix (default) plus three
/// stereo auxes. A track routed to an aux pair does *not* contribute to the
/// master mix — its dry signal lands on that aux pair only. Sends still ride
/// the shared send buses; the wet FX return always lands on the master pair.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Default)]
pub enum OutPair {
    /// Master mix — pair 0 (channels 0/1). Default for every track.
    #[default]
    Master,
    /// Auxiliary output 1 — pair 1 (channels 2/3).
    Aux1,
    /// Auxiliary output 2 — pair 2 (channels 4/5).
    Aux2,
    /// Auxiliary output 3 — pair 3 (channels 6/7).
    Aux3,
}

impl OutPair {
    /// Number of output pairs.
    pub const COUNT: usize = 4;

    /// All pairs in index order.
    pub const ALL: [Self; Self::COUNT] = [Self::Master, Self::Aux1, Self::Aux2, Self::Aux3];

    /// Pair index, `0..=3`. Master = 0, Aux1 = 1, Aux2 = 2, Aux3 = 3.
    pub fn index(self) -> usize {
        match self {
            Self::Master => 0,
            Self::Aux1 => 1,
            Self::Aux2 => 2,
            Self::Aux3 => 3,
        }
    }
}

/// What a scheduled event does when it fires.
#[derive(Clone, Copy, PartialEq, Debug)]
pub enum EngineEvent {
    /// Play a track chromatically.
    NoteOn {
        /// MIDI channel, `0..=7` — selects the track in the device engine.
        channel: u8,
        /// MIDI note number — sets the pitch (60 = macro pitch).
        note: u8,
        /// Normalised velocity, `0.0..=1.0`.
        velocity: f32,
    },
    /// Close the gate on a track.
    ///
    /// The counterpart to [`NoteOn`](Self::NoteOn), and what makes a sustained
    /// voice possible. A one-shot voice ignores it and runs its own envelope
    /// out; a gated voice drops into its release.
    NoteOff {
        /// MIDI channel, `0..=7` — selects the track in the device engine.
        channel: u8,
        /// MIDI note number. Carried for symmetry with
        /// [`NoteOn`](Self::NoteOn) and for devices that later want
        /// per-note voice allocation; the channel is what routes today,
        /// because a track holds exactly one voice.
        note: u8,
    },
    /// Silence the whole engine.
    Panic,
}

/// An engine action scheduled for a sample offset within the next block.
#[derive(Clone, Copy, PartialEq, Debug)]
pub struct TimedEvent {
    /// Sample offset inside the next block, `0..=BLOCK-1`. Clamped on push.
    pub offset: usize,
    /// What fires at that offset.
    pub event: EngineEvent,
}

/// Maximum number of timed events a device can hold for the next block.
pub const MAX_TIMED_EVENTS: usize = 16;

/// Fixed-capacity schedule of engine events for the *next* block.
///
/// No allocation; lives inside the device engine. The main loop pushes events
/// with a sample offset and the audio callback drains it at the top of the
/// block, firing each event at its sample.
#[derive(Clone, Copy)]
pub struct TimedQueue {
    items: [Option<TimedEvent>; MAX_TIMED_EVENTS],
    len: usize,
}

impl Default for TimedQueue {
    fn default() -> Self {
        Self::new()
    }
}

impl TimedQueue {
    /// Empty queue.
    pub const fn new() -> Self {
        Self {
            items: [None; MAX_TIMED_EVENTS],
            len: 0,
        }
    }

    /// Number of events currently queued.
    pub fn len(&self) -> usize {
        self.len
    }

    /// True when the queue has no events.
    pub fn is_empty(&self) -> bool {
        self.len == 0
    }

    /// True when the queue is full.
    pub fn is_full(&self) -> bool {
        self.len == MAX_TIMED_EVENTS
    }

    /// Queue an event to fire `offset` samples into the next block.
    ///
    /// `offset` is clamped to `BLOCK - 1`. Returns false if the queue is full.
    pub fn push(&mut self, offset: usize, event: EngineEvent) -> bool {
        if self.is_full() {
            return false;
        }
        self.items[self.len] = Some(TimedEvent {
            offset: offset.min(BLOCK - 1),
            event,
        });
        self.len += 1;
        true
    }

    /// Move the queue into `out`, sorted by ascending offset, and clear self.
    /// Returns the number of events copied; only the first `n` entries of
    /// `out` are valid.
    pub fn drain_sorted(&mut self, out: &mut [Option<TimedEvent>; MAX_TIMED_EVENTS]) -> usize {
        let n = self.len;
        out[..n].copy_from_slice(&self.items[..n]);
        // Insertion sort; at most 16 items, typically 1..=3.
        let mut i = 1;
        while i < n {
            let mut j = i;
            while j > 0 && out[j - 1].unwrap().offset > out[j].unwrap().offset {
                out.swap(j - 1, j);
                j -= 1;
            }
            i += 1;
        }
        self.items = [None; MAX_TIMED_EVENTS];
        self.len = 0;
        n
    }
}
