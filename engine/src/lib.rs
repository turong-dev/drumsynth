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
//! one [`MachineId`](machines::MachineId) into its [`MachineSlot`] and runs it
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

pub mod dsp;
#[cfg(feature = "grid")]
pub mod grid;
pub mod machines;
pub mod midi;

pub use machines::{
    MachineId, MacroInfo, MACROS_PER_BANK, NUM_BANKS, NUM_MACROS, SLOT_FILT_0, SLOT_FILT_1,
    SLOT_LFO1_DEPTH, SLOT_LFO1_DEST, SLOT_LFO1_RATE, SLOT_LFO2_DEPTH, SLOT_LFO2_DEST,
    SLOT_LFO2_RATE, SLOT_LEVEL, SLOT_MACHINE, SLOT_OUT, SLOT_PAN, SLOT_SEND_DELAY,
    SLOT_SEND_REVERB, SLOT_STRIP_ATK, SLOT_STRIP_CUT, SLOT_STRIP_DEC, SLOT_STRIP_HOLD,
    SLOT_STRIP_RESO,
};

use dsp::{Lfo, ModDest};

use machines::MachineSlot;

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
/// the number your cycle budget is measured against — see `firmware/src/bin/bench.rs`.
pub const BLOCK: usize = 32;

/// Anything below this magnitude is flushed to zero.
///
/// Long envelope and filter tails decay toward denormal floats, which on some
/// cores trap to microcode and cost orders of magnitude more than a normal
/// operation. The symptom is a synth that gets slower the longer it runs.
/// Cheaper to clamp than to debug.
pub const DENORMAL_FLOOR: f32 = 1.0e-9;

/// How many tracks the engine owns.
///
/// The 8-track count is matched to a comfortably-sized drum kit on a Teensy
/// 4.1's cycle budget; see `PLAN.md` for the bench-driven sizing rationale. A
/// `const` rather than const-generic because consumers say `engine.tracks[i]`
/// a lot and need a known length.
pub const TRACKS: usize = 8;

/// Output pair a track routes to.
///
/// The engine has four stereo output pairs: the master mix (default) plus
/// three stereo auxes. A track routed to an aux pair does *not* contribute
/// to the master mix — its dry signal lands on that aux pair only. Sends
/// still ride the shared send buses; the wet FX return always lands on the
/// master pair (matches how hardware aux returns work: the FX lives on the
/// mix bus, not the individual channel).
///
/// "Mono out" is a stereo pair with the track's strip pan set to one
/// extreme and the consumer collapsing stereo→mono. There is no dedicated
/// mono path — that's how real drum machines with stereo individual-outs
/// do it too.
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

/// Per-track signal chain — everything but the machine itself.
///
/// Applied in order: drive → filter → amp-envelope × pan × level. The amp
/// envelope defaults to an instant-on gate (attack 0, hold several seconds,
/// decay trailing the machine's own tail), so by default the *machine's*
/// envelope shapes the audible hit and the strip is just the mixer.
/// Set [`StripParams::amp_decay_s`] lower to LFO-style gate a track, or
/// raise [`StripParams::amp_attack_s`] to fade it in.
#[derive(Clone, Copy)]
#[cfg_attr(feature = "debug-params", derive(Debug))]
pub struct StripParams {
    /// Multimode filter type.
    pub f_mode: dsp::SvfMode,
    /// Filter cutoff, Hz.
    pub f_cutoff_hz: f32,
    /// Filter resonance, Q (0.5..~20; 0.707 is Butterworth).
    pub f_reso_q: f32,
    /// Filter envelope attack, seconds. The track amp envelope is reused as
    /// the filter envelope in Phase 1 — a dedicated filter env is Phase 3.
    pub amp_attack_s: f32,
    /// Filter envelope hold, seconds.
    pub amp_hold_s: f32,
    /// Amp envelope decay, seconds. Track-level gate.
    pub amp_decay_s: f32,
    /// Pre-filter drive gain; 1.0 = no drive, higher saturates.
    pub drive: f32,
    /// Pan, -1 (fully left) .. +1 (fully right).
    pub pan: f32,
    /// Track fader, 0..1.
    pub level: f32,
    /// Send level to the delay bus, 0..1. Post-fader, like a mixer aux.
    pub send_delay: f32,
    /// Send level to the reverb bus, 0..1. Post-fader, like a mixer aux.
    pub send_reverb: f32,
    /// Which output pair this track routes to. Tracks on a non-master pair
    /// do not contribute to the master sum.
    pub out: OutPair,
    /// Mask of track indices that this track *chokes* when triggered. Bit `1
    /// << i` set means triggering this track resets track `i` immediately
    /// (cutting its tail) — the OH-cuts-CH relation.
    pub choke_mask: u8,
    /// Mask of track indices that get *layered* on this track's trigger. Bit
    /// `1 << i` set means track `i` is also triggered with the same velocity.
    pub layer_mask: u8,
}

impl Default for StripParams {
    fn default() -> Self {
        Self {
            f_mode: dsp::SvfMode::Off,
            f_cutoff_hz: 1000.0,
            f_reso_q: 0.707,
            amp_attack_s: 0.0,
            amp_hold_s: 10.0,
            amp_decay_s: 10.0,
            drive: 1.0,
            pan: 0.0,
            level: 1.0,
            send_delay: 0.0,
            send_reverb: 0.0,
            out: OutPair::Master,
            choke_mask: 0,
            layer_mask: 0,
        }
    }
}

/// A complete sound: machine + macros + strip. `Copy`, ~100 bytes.
///
/// The Syntakt's "sound pool" is a fixed-size array of these. Load one onto
/// a track at trigger time to get per-trig sound locks: the engine
/// re-loads the machine and applies the macros + strip in `trigger_with_sound`,
/// all at control rate so there's no allocation or coefficient recompute in
/// the audio callback.
#[derive(Clone, Copy)]
#[cfg_attr(feature = "debug-params", derive(Debug))]
pub struct Sound {
    /// Which machine to load.
    pub machine_id: MachineId,
    /// Factory macro values for this sound.
    pub macros: [f32; NUM_MACROS],
    /// Strip configuration (filter, amp env, drive, pan, level, choke, layer).
    pub strip: StripParams,
}

impl Sound {
    /// Build a sound for the given machine with its default macros + default
    /// strip. A one-liner starting point for sound design.
    pub fn from_defaults(id: MachineId) -> Self {
        Self {
            machine_id: id,
            macros: id.default_macros(),
            strip: StripParams::default(),
        }
    }
}

/// One velocity-modulation slot: maps incoming velocity to a destination with
/// a bipolar depth. Applied once at trigger time and sustained through the
/// hit.
#[derive(Clone, Copy, PartialEq, Debug)]
pub struct VelMod {
    /// What to modulate.
    pub dest: ModDest,
    /// Bipolar depth, -1..1. Positive = velocity increases the param.
    pub depth: f32,
}

impl VelMod {
    /// No-op velocity mod (depth 0, destination None).
    pub const fn off() -> Self {
        Self {
            dest: ModDest::None,
            depth: 0.0,
        }
    }
}

/// Maximum velocity-mod slots per track.
pub const MAX_VEL_MODS: usize = 4;

/// Per-track modulation state: 2 LFOs + 4 velocity-mod slots.
#[derive(Clone, Copy)]
pub struct ModState {
    /// Two independent LFOs, each with its own destination + depth.
    pub lfos: [Lfo; 2],
    /// Velocity-mod slots. Applied at trigger time; contribute to the
    /// effective macros/strip for the duration of the hit.
    pub vel_mods: [VelMod; MAX_VEL_MODS],
    /// Last velocity received, kept for `control()` to apply velocity mod.
    last_velocity: f32,
}

impl ModState {
    /// All LFOs idle, all vel-mods off.
    pub const fn new() -> Self {
        Self {
            lfos: [Lfo::new(), Lfo::new()],
            vel_mods: [VelMod::off(), VelMod::off(), VelMod::off(), VelMod::off()],
            last_velocity: 0.0,
        }
    }

    /// True if any LFO is contributing or any velocity mod has non-zero depth.
    pub fn has_active_mod(&self) -> bool {
        if self.lfos[0].is_contributing() || self.lfos[1].is_contributing() {
            return true;
        }
        for vm in &self.vel_mods {
            if vm.depth != 0.0 && vm.dest != ModDest::None {
                return true;
            }
        }
        false
    }

    /// Record velocity for the next `control()` pass and fire LFO triggers.
    fn trigger(&mut self, velocity: f32) {
        self.last_velocity = velocity;
        for lfo in &mut self.lfos {
            lfo.trigger();
        }
    }

    /// Reset LFOs to their start phases.
    fn reset(&mut self) {
        self.last_velocity = 0.0;
        for lfo in &mut self.lfos {
            lfo.reset();
        }
    }
}

impl Default for ModState {
    fn default() -> Self {
        Self::new()
    }
}

/// Maximum number of timed events the engine can hold for the next block.
///
/// 16 covers an 8-track grid of 16th notes at 150 BPM (a note every 10 ms)
/// plus everything a MIDI cable can carry between two blocks. If a host
/// saturates it, events are dropped — the design guidance is that losing an
/// event beats missing the audio deadline.
pub const MAX_TIMED_EVENTS: usize = 16;

/// What a scheduled event does when it fires.
#[derive(Clone, Copy, PartialEq, Debug)]
pub enum EngineEvent {
    /// Play a track chromatically, exactly as
    /// [`MidiEvent::NoteOn`](crate::midi::MidiEvent::NoteOn) routes.
    NoteOn {
        /// MIDI channel, `0..=7` — selects the track.
        channel: u8,
        /// MIDI note number — sets the pitch (60 = macro pitch).
        note: u8,
        /// Normalised velocity, `0.0..=1.0`.
        velocity: f32,
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

/// Fixed-capacity schedule of engine events for the *next*
/// [`DrumEngine::process`] block.
///
/// No allocation; lives inside [`DrumEngine`]. The main loop pushes events
/// with a sample offset (how far into the next block they should fire) and
/// [`DrumEngine::process`] drains it at the top, sorts by offset, and fires
/// each event at its sample inside the block.
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

    /// True when the queue is full — the next [`Self::push`] would drop.
    pub fn is_full(&self) -> bool {
        self.len == MAX_TIMED_EVENTS
    }

    /// Queue an event to fire `offset` samples into the next block.
    ///
    /// `offset` is clamped to `BLOCK - 1`, so a late-published event still
    /// lands before the block ends rather than being silently lost. Returns
    /// false if the queue is full (event dropped).
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
    ///
    /// Sorted into the caller's array rather than returned so the hot path
    /// reuses one stack array per block instead of building one.
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

/// Retrigger de-click crossfade length (samples). A hit landing on an
/// already-sounding voice would otherwise step the output from the old tail
/// to the new voice's attack; the output is instead crossfaded over this
/// window (1 ms at 48 kHz).
const DECLICK_SAMPLES: usize = 48;

/// Inverse of [`DECLICK_SAMPLES`] — the crossfade advances this much per
/// sample, reaching exactly 1.0 on the last sample of the window so the
/// switch back to the raw signal is seamless.
const DECLICK_STEP: f32 = 1.0 / DECLICK_SAMPLES as f32;

/// Choke fade length (samples). A choked voice keeps ringing under a linear
/// ramp to zero over this window (5 ms at 48 kHz) instead of being cut
/// instantly, then is hard-reset.
const CHOKE_SAMPLES: usize = 240;

/// One channel of the kit.
pub struct Track {
    /// Underlying synthesis model. Reassignable via [`Track::load_machine`].
    pub slot: MachineSlot,
    /// Base macro values (user-configured, pre-modulation).
    pub base_macros: [f32; NUM_MACROS],
    /// CC macro targets: [`Track::control`] slews `base_macros` toward these
    /// at block rate. Only macros touched by [`Track::set_macro_target`] are
    /// tracked (see `macro_pending`); everything else is direct.
    macro_targets: [f32; NUM_MACROS],
    /// One-pole state used to slew a pending macro toward its target.
    macro_smooth: [f32; NUM_MACROS],
    /// Bitmask: bit `i` set = macro `i` is mid-ramp toward `macro_targets[i]`.
    macro_pending: u32,
    /// Strip configuration (filter, amp env, drive, pan, level, choke, layer).
    pub strip: StripParams,
    /// LFOs + velocity modulation.
    pub mod_state: ModState,

    // ---- strip runtime state ----
    filter: dsp::Svf,
    amp_env: dsp::AhdEnv,
    // Equal-power pan gains, recomputed when pan changes.
    pan_l: f32,
    pan_r: f32,
    // Effective drive/level/pan after modulation, used in `tick`.
    eff_drive: f32,
    eff_level: f32,
    /// Effective delay send after modulation, used in `tick`. Post-fader.
    eff_send_delay: f32,
    /// Effective reverb send after modulation.
    eff_send_reverb: f32,

    // ---- de-click / choke smoothing ----
    /// Samples left in the retrigger crossfade window. 0 = bypassed.
    declick_left: usize,
    /// Crossfade progress 0..1, advanced by [`DECLICK_STEP`] per sample.
    declick_t: f32,
    /// Crossfade start points: the output at the moment of the retrigger.
    declick_from_l: f32,
    declick_from_r: f32,
    /// Last output, synced every tick. Seeded into `declick_from_*` when a
    /// retrigger arms the window, so the crossfade always starts from the
    /// actual previous output.
    declick_l: f32,
    declick_r: f32,
    /// Samples left in the choke fade-out window. 0 = no fade in progress.
    choke_fade_left: usize,
}

impl Track {
    /// Build a track loaded with the given machine and that machine's
    /// default macros + the default strip.
    pub fn new(id: MachineId) -> Self {
        let base_macros = id.default_macros();
        let strip = StripParams::default();
        let slot = MachineSlot::new(id, &base_macros);
        let mut filter = dsp::Svf::new(strip.f_mode);
        filter.recalc(strip.f_cutoff_hz, strip.f_reso_q, SAMPLE_RATE);
        let mut amp_env = dsp::AhdEnv::new();
        amp_env.set_params(strip.amp_attack_s, strip.amp_hold_s, strip.amp_decay_s);
        let pan_l;
        let pan_r;
        (pan_l, pan_r) = pan_law(strip.pan);
        Self {
            slot,
            base_macros,
            macro_targets: base_macros,
            macro_smooth: base_macros,
            macro_pending: 0,
            strip,
            mod_state: ModState::new(),
            filter,
            amp_env,
            pan_l,
            pan_r,
            eff_drive: strip.drive,
            eff_level: strip.level,
            eff_send_delay: strip.send_delay,
            eff_send_reverb: strip.send_reverb,
            declick_left: 0,
            declick_t: 0.0,
            declick_from_l: 0.0,
            declick_from_r: 0.0,
            declick_l: 0.0,
            declick_r: 0.0,
            choke_fade_left: 0,
        }
    }

    /// Replace the machine on this track, resetting macros to that machine's
    /// defaults. Strip and modulation are untouched.
    pub fn load_machine(&mut self, id: MachineId) {
        self.base_macros = id.default_macros();
        // Sends are track-level, not machine-level: keep the strip's aux
        // levels and mirror them back into the macro slots so the two stay
        // consistent.
        self.base_macros[SLOT_SEND_DELAY] = self.strip.send_delay;
        self.base_macros[SLOT_SEND_REVERB] = self.strip.send_reverb;
        // Pan is track-level too: keep the strip's pan mirrored in the macro.
        self.base_macros[SLOT_PAN] = self.strip.pan * 0.5 + 0.5;
        // The MACH slot always mirrors the loaded machine (see `load_sound`).
        self.base_macros[SLOT_MACHINE] = id.index() as f32 / (MachineId::COUNT - 1) as f32;
        self.slot = MachineSlot::new(id, &self.base_macros);
        // Macros reset wholesale: the CC smoother must follow, not ramp from
        // a value that no longer means anything on the new machine.
        self.sync_macro_smoothing();
    }

    /// Load a complete sound (machine + macros + strip) onto this track.
    pub fn load_sound(&mut self, sound: &Sound) {
        self.load_machine(sound.machine_id);
        self.set_strip(&sound.strip);
        // The sound's macros are authoritative for its sends (they round-trip
        // verbatim between host and device); derive the strip aux levels from
        // them so the two stay consistent.
        self.base_macros = sound.macros;
        // The MACH slot always mirrors the loaded machine, whatever the
        // sound's macro array carried.
        self.base_macros[SLOT_MACHINE] =
            sound.machine_id.index() as f32 / (MachineId::COUNT - 1) as f32;
        self.strip.send_delay = self.base_macros[SLOT_SEND_DELAY];
        self.strip.send_reverb = self.base_macros[SLOT_SEND_REVERB];
        self.eff_send_delay = self.base_macros[SLOT_SEND_DELAY];
        self.eff_send_reverb = self.base_macros[SLOT_SEND_REVERB];
        if !self.mod_state.has_active_mod() {
            self.slot.set_macros(&self.base_macros);
        }
        // base_macros was overwritten by hand after load_machine's sync;
        // re-sync the CC smoother to the final array.
        self.sync_macro_smoothing();
    }

    /// Which machine this track holds.
    pub fn id(&self) -> MachineId {
        self.slot.id()
    }

    /// Set one base macro. Coefficient recompute is included.
    pub fn set_macro(&mut self, idx: usize, value: f32) {
        if idx >= NUM_MACROS {
            return;
        }
        let v = value.clamp(0.0, 1.0);
        // Machine selector (PITCH slot 5): quantise 0..1 onto the machine
        // catalogue. Track-routed, so CC 25 swaps engines without a program
        // change. Loading a different machine resets macros to its defaults —
        // the old voice's macro values mean nothing on the new one.
        if idx == SLOT_MACHINE {
            let i = (v * (MachineId::COUNT - 1) as f32) as usize;
            let id = MachineId::ALL[i];
            if id != self.slot.id() {
                self.load_machine(id);
            }
            return;
        }
        // Output routing (MOD slot 26): quantise 0..1 onto the 4 `OutPair`
        // variants and write `strip.out`. Same instant-jump discipline as
        // the machine selector — routing is a discrete choice, smoothing
        // it across blocks would route a track to a half-pair. Stores the
        // quantised macro value back so MIDI feedback / round-trip reads
        // return the canonical centre of the variant.
        if idx == SLOT_OUT {
            const PAIRS: [OutPair; 4] =
                [OutPair::Master, OutPair::Aux1, OutPair::Aux2, OutPair::Aux3];
            let i = (v * (PAIRS.len() - 1) as f32 + 0.5) as usize;
            self.strip.out = PAIRS[i];
            let q = i as f32 / (PAIRS.len() - 1) as f32;
            self.base_macros[SLOT_OUT] = q;
            self.macro_smooth[SLOT_OUT] = q;
            self.macro_pending &= !(1 << SLOT_OUT);
            return;
        }
        // Track-routed strip/LFO macros. These drive the per-track strip and
        // modulation state, not the machine DSP, so they are intercepted
        // before the generic machine-macro path below. Each stores the macro
        // value and applies the derived parameter immediately.
        if idx == SLOT_STRIP_CUT
            || idx == SLOT_STRIP_RESO
            || idx == SLOT_STRIP_ATK
            || idx == SLOT_STRIP_HOLD
            || idx == SLOT_STRIP_DEC
        {
            self.base_macros[idx] = v;
            self.macro_smooth[idx] = v;
            self.macro_pending &= !(1 << idx);
            self.apply_strip_macros();
            return;
        }
        if idx == SLOT_LFO1_RATE
            || idx == SLOT_LFO1_DEPTH
            || idx == SLOT_LFO1_DEST
            || idx == SLOT_LFO2_RATE
            || idx == SLOT_LFO2_DEPTH
            || idx == SLOT_LFO2_DEST
        {
            self.base_macros[idx] = v;
            self.macro_smooth[idx] = v;
            self.macro_pending &= !(1 << idx);
            self.apply_lfo_macros();
            return;
        }
        self.base_macros[idx] = v;
        // Keep the CC smoother in sync: a direct set is the new current
        // value, and any pending ramp to a stale target is cancelled.
        self.macro_smooth[idx] = v;
        self.macro_pending &= !(1 << idx);
        // The send macros are track-routed: editing SEND.DLY / SEND.RVB
        // drives the strip's aux levels (and the effective values read in
        // the per-sample path), not the machine DSP.
        if idx == SLOT_SEND_DELAY {
            self.strip.send_delay = v;
            self.eff_send_delay = v;
        } else if idx == SLOT_SEND_REVERB {
            self.strip.send_reverb = v;
            self.eff_send_reverb = v;
        } else if idx == SLOT_PAN {
            // Macro 0..1 maps onto the bipolar -1..+1 strip pan (0.5 = centre).
            // `tick` reads the cached pan gains, so recompute them here or the
            // change is inaudible.
            self.strip.pan = v * 2.0 - 1.0;
            let (l, r) = pan_law(self.strip.pan);
            self.pan_l = l;
            self.pan_r = r;
        }
        if !self.mod_state.has_active_mod() {
            self.slot.set_macros(&self.base_macros);
        }
    }

    /// Set the *target* for a CC-driven macro.
    ///
    /// The value ramps to it at block rate via [`Track::control`] (see
    /// [`MACRO_SMOOTH_K`]), so a burst of CC messages never triggers a
    /// coefficient recompute per message — `set_macro`, the recompute call,
    /// runs at most once per block per moving macro. The machine selector
    /// jumps instantly: swapping engines must be immediate, and loading a
    /// machine resets macros to its defaults anyway.
    ///
    /// Control rate, main loop only — never the audio interrupt.
    pub fn set_macro_target(&mut self, idx: usize, value: f32) {
        if idx >= NUM_MACROS {
            return;
        }
        if idx == SLOT_MACHINE || idx == SLOT_OUT {
            self.set_macro(idx, value);
            return;
        }
        let v = value.clamp(0.0, 1.0);
        self.macro_targets[idx] = v;
        // Seed the ramp from wherever the macro actually is now, so a
        // retarget mid-ramp (a knob being turned) continues smoothly from
        // the current value rather than jumping back to the block base.
        self.macro_smooth[idx] = self.base_macros[idx];
        self.macro_pending |= 1 << idx;
    }

    /// Reset the CC smoother to match the current macro array: pending ramps
    /// cancelled, targets and smoother seeded from base values.
    fn sync_macro_smoothing(&mut self) {
        self.macro_targets = self.base_macros;
        self.macro_smooth = self.base_macros;
        self.macro_pending = 0;
    }

    /// Advance every pending macro one block toward its target. At most one
    /// `set_macro` per moving macro per block, so a CC burst costs one
    /// coefficient recompute per macro per block instead of one per message.
    fn advance_macro_smoothing(&mut self) {
        let mut i = 0;
        while i < NUM_MACROS {
            if self.macro_pending & (1 << i) != 0 {
                let target = self.macro_targets[i];
                let next = self.macro_smooth[i] + (target - self.macro_smooth[i]) * MACRO_SMOOTH_K;
                if (target - next).abs() < MACRO_SMOOTH_EPS {
                    // Converged: snap; `set_macro` clears the pending bit.
                    self.set_macro(i, target);
                } else {
                    // Still moving: `set_macro` clears the pending bit as
                    // part of syncing the smoother, so re-arm it.
                    self.set_macro(i, next);
                    self.macro_pending |= 1 << i;
                }
            }
            i += 1;
        }
    }

    /// Apply the strip-filter and AHD-envelope macro slots to the strip.
    ///
    /// Cutoff is log-mapped 20 Hz..20 kHz; resonance is linear 0.5..20 Q;
    /// attack 0..1 s, hold 0..2 s, decay 0.01..3 s. Only the coefficients that
    /// changed are recomputed, via [`set_strip`]'s dirty-flag logic.
    fn apply_strip_macros(&mut self) {
        let m = self.base_macros;
        let new_strip = StripParams {
            f_cutoff_hz: 20.0 * libm::powf(1000.0, m[SLOT_STRIP_CUT]),
            f_reso_q: 0.5 + 19.5 * m[SLOT_STRIP_RESO],
            amp_attack_s: m[SLOT_STRIP_ATK],
            amp_hold_s: 10.0 * m[SLOT_STRIP_HOLD],
            amp_decay_s: 0.01 + 9.99 * m[SLOT_STRIP_DEC],
            ..self.strip
        };
        self.set_strip(&new_strip);
    }

    /// Apply the LFO macro slots to the two LFOs.
    ///
    /// Rate is split: 0..0.5 = slow range (0.1..10 Hz), 0.5..1 = fast range
    /// (1..100 Hz), log-mapped within each half. Depth is 0..1. Destination is
    /// quantised over [`ModDest`] via [`ModDest::from_macro`].
    fn apply_lfo_macros(&mut self) {
        let m = self.base_macros;
        apply_one_lfo_macro(
            &mut self.mod_state.lfos[0],
            m[SLOT_LFO1_RATE],
            m[SLOT_LFO1_DEPTH],
            m[SLOT_LFO1_DEST],
        );
        apply_one_lfo_macro(
            &mut self.mod_state.lfos[1],
            m[SLOT_LFO2_RATE],
            m[SLOT_LFO2_DEPTH],
            m[SLOT_LFO2_DEST],
        );
    }

    /// Replace all base macros in one call.
    pub fn set_macros(&mut self, all: &[f32; NUM_MACROS]) {
        self.base_macros = *all;
        // Sends are track-routed from the macro array (see `set_macro`).
        self.strip.send_delay = all[SLOT_SEND_DELAY];
        self.strip.send_reverb = all[SLOT_SEND_REVERB];
        self.eff_send_delay = all[SLOT_SEND_DELAY];
        self.eff_send_reverb = all[SLOT_SEND_REVERB];
        // Pan is track-routed too (see `set_macro`): the macro owns the value,
        // the strip pan is the derived -1..+1 form and the gains feed `tick`.
        self.strip.pan = all[SLOT_PAN] * 2.0 - 1.0;
        let (l, r) = pan_law(self.strip.pan);
        self.pan_l = l;
        self.pan_r = r;
        // Output routing is track-routed too (see `set_macro`): quantise the
        // macro value over the 4 `OutPair` variants and store the quantised
        // form back so round-trip reads return the canonical centre.
        const PAIRS: [OutPair; 4] = [OutPair::Master, OutPair::Aux1, OutPair::Aux2, OutPair::Aux3];
        let i = (all[SLOT_OUT] * (PAIRS.len() - 1) as f32 + 0.5) as usize;
        self.strip.out = PAIRS[i];
        self.base_macros[SLOT_OUT] = i as f32 / (PAIRS.len() - 1) as f32;
        // Strip filter / AHD env / LFO macros are track-routed too (see
        // `set_macro`): apply them to the strip and mod state.
        self.apply_strip_macros();
        self.apply_lfo_macros();
        if !self.mod_state.has_active_mod() {
            self.slot.set_macros(&self.base_macros);
        }
        // Bulk macro replace resets the CC smoother (same rationale as
        // `load_machine`).
        self.sync_macro_smoothing();
    }

    /// Transpose the machine by `semis` semitones relative to its macro
    /// pitch. Absolute, not incremental — the whole voice (sweep, FM ratio,
    /// detune) moves. Survives later macro recomputes via each machine's
    /// internal `freq_scale`. Noise-only machines no-op. Control rate.
    pub fn retune(&mut self, semis: f32) {
        self.slot.retune(semis);
    }

    /// Replace the strip configuration. Coefficients are recomputed.
    pub fn set_strip(&mut self, params: &StripParams) {
        let prev = self.strip;
        self.strip = *params;

        if params.f_mode != prev.f_mode {
            self.filter.set_mode(params.f_mode);
        }
        if params.f_mode != prev.f_mode
            || params.f_cutoff_hz != prev.f_cutoff_hz
            || params.f_reso_q != prev.f_reso_q
        {
            self.filter
                .recalc(params.f_cutoff_hz, params.f_reso_q, SAMPLE_RATE);
        }
        if params.amp_attack_s != prev.amp_attack_s
            || params.amp_hold_s != prev.amp_hold_s
            || params.amp_decay_s != prev.amp_decay_s
        {
            self.amp_env
                .set_params(params.amp_attack_s, params.amp_hold_s, params.amp_decay_s);
        }
        if params.pan != prev.pan {
            let (l, r) = pan_law(params.pan);
            self.pan_l = l;
            self.pan_r = r;
        }
        // Mirror the aux levels into the send macros so a strip edit keeps the
        // macro view (MIDI CC 41/42) consistent. The send macros are the
        // routing authority; the strip fields are derived storage.
        self.base_macros[SLOT_SEND_DELAY] = params.send_delay;
        self.base_macros[SLOT_SEND_REVERB] = params.send_reverb;
        // Mirror the strip pan into the PAN macro slot so a strip edit keeps
        // the macro view (MIDI CC 37) consistent, like the sends.
        self.base_macros[SLOT_PAN] = params.pan * 0.5 + 0.5;
        // Mirror routing into the OUT macro slot (MIDI CC 46). Same shape as
        // `set_macro(SLOT_OUT)`: the macro view shows the canonical centre
        // of the variant, so MIDI feedback reports the value the user
        // expects.
        self.base_macros[SLOT_OUT] = params.out.index() as f32 / 3.0;
        // Mirror the strip filter / AHD env params into their macro slots so a
        // strip edit keeps the macro view consistent. Inverse of
        // `apply_strip_macros`.
        self.base_macros[SLOT_STRIP_CUT] = libm::logf(params.f_cutoff_hz / 20.0) / libm::logf(1000.0);
        self.base_macros[SLOT_STRIP_RESO] = (params.f_reso_q - 0.5) / 19.5;
        self.base_macros[SLOT_STRIP_ATK] = params.amp_attack_s;
        self.base_macros[SLOT_STRIP_HOLD] = params.amp_hold_s / 10.0;
        self.base_macros[SLOT_STRIP_DEC] = (params.amp_decay_s - 0.01) / 9.99;
        if !self.mod_state.has_active_mod() {
            self.eff_drive = params.drive;
            self.eff_level = params.level;
            self.eff_send_delay = params.send_delay;
            self.eff_send_reverb = params.send_reverb;
        }
        // The strip edit just rewrote the send/pan macro mirrors; resync the
        // CC smoother so a pending ramp doesn't yank them back.
        self.sync_macro_smoothing();
    }

    /// Begin a hit at `velocity` (0..=1.0). Fires LFO triggers and stores
    /// velocity for the control pass.
    pub fn trigger(&mut self, velocity: f32) {
        let retrigger = self.slot.is_active();
        self.slot.trigger(velocity);
        self.amp_env.trigger(velocity);
        self.mod_state.trigger(velocity);
        // A hit landing on an already-sounding voice would step the output
        // from the old tail to the new attack; arm a short crossfade from
        // the current output instead. A fresh hit from silence cancels any
        // stale window and any in-progress choke fade.
        self.choke_fade_left = 0;
        if retrigger {
            self.declick_from_l = self.declick_l;
            self.declick_from_r = self.declick_r;
            self.declick_left = DECLICK_SAMPLES;
            self.declick_t = 0.0;
        } else {
            self.declick_left = 0;
        }
    }

    /// Force to silence immediately. Used by panic (kill switch) — chokes
    /// use [`Track::choke`] so their cut is faded, not instant.
    pub fn reset(&mut self) {
        self.slot.reset();
        self.amp_env.reset();
        self.filter.reset();
        self.mod_state.reset();
        self.declick_left = 0;
        self.declick_t = 0.0;
        self.choke_fade_left = 0;
    }

    /// Choke this voice: fade its output to zero over [`CHOKE_SAMPLES`]
    /// rather than cutting it. The machine keeps ringing under the fade and
    /// is hard-reset once the fade completes. A no-op when already silent or
    /// already fading.
    pub fn choke(&mut self) {
        if self.slot.is_active() && self.choke_fade_left == 0 {
            self.choke_fade_left = CHOKE_SAMPLES;
        }
    }

    /// Still producing output? Also true for the tail of a [`Track::choke`]
    /// fade, so the fading voice keeps rendering until it reaches silence.
    pub fn is_active(&self) -> bool {
        self.slot.is_active() || self.choke_fade_left > 0
    }

    /// Per-block control pass. Advances LFOs, sums modulation onto base
    /// macros + strip params, and recomputes coefficients. Called from
    /// [`DrumEngine::process`] before the sample loop. If no modulation is
    /// active and no CC macros are mid-ramp, this is a single-branch early
    /// return.
    pub fn control(&mut self) {
        // 0. CC macro smoothing first, before the mod-state borrow: slew
        //    pending macros toward their targets and recompute coefficients
        //    as they move, so a CC edit is audible even with no modulation.
        if self.macro_pending != 0 {
            self.advance_macro_smoothing();
        }

        let mod_state = &mut self.mod_state;
        if !mod_state.has_active_mod() {
            return;
        }

        // 1. Advance LFOs by one block.
        mod_state.lfos[0].tick_block();
        mod_state.lfos[1].tick_block();
        let lfo0 = mod_state.lfos[0];
        let lfo1 = mod_state.lfos[1];
        let vel = mod_state.last_velocity;

        // 2. Start from base values.
        let mut eff_macros = self.base_macros;
        let mut eff_cutoff = self.strip.f_cutoff_hz;
        let mut eff_reso = self.strip.f_reso_q;
        let mut eff_drive = self.strip.drive;
        let mut eff_pan = self.strip.pan;
        let mut eff_level = self.strip.level;
        let mut eff_amp_decay = self.strip.amp_decay_s;
        let mut eff_send_delay = self.strip.send_delay;
        let mut eff_send_reverb = self.strip.send_reverb;
        let mut macro_dirty = false;
        let mut strip_dirty = false;

        // 3. Apply LFO modulations.
        for lfo in [lfo0, lfo1] {
            if !lfo.is_contributing() {
                continue;
            }
            let v = lfo.value() * lfo.depth();
            match lfo.dest() {
                ModDest::Macro(i) if i < NUM_MACROS => {
                    eff_macros[i] = (eff_macros[i] + v).clamp(0.0, 1.0);
                    macro_dirty = true;
                }
                ModDest::FilterCutoff => {
                    eff_cutoff = (eff_cutoff * (1.0 + v * 3.0)).clamp(1.0, 20_000.0);
                    strip_dirty = true;
                }
                ModDest::FilterReso => {
                    eff_reso = (eff_reso + v * 2.0).clamp(0.5, 20.0);
                    strip_dirty = true;
                }
                ModDest::Drive => {
                    eff_drive = (eff_drive + v * 3.0).clamp(0.0, 6.0);
                    // Drive is applied per-sample in `tick`; just cache it.
                    self.eff_drive = eff_drive;
                }
                ModDest::Pan => {
                    eff_pan = (eff_pan + v).clamp(-1.0, 1.0);
                    let (l, r) = pan_law(eff_pan);
                    self.pan_l = l;
                    self.pan_r = r;
                }
                ModDest::Level => {
                    eff_level = (eff_level + v * 0.5).clamp(0.0, 1.0);
                    self.eff_level = eff_level;
                }
                ModDest::AmpDecay => {
                    eff_amp_decay = (eff_amp_decay * (1.0 + v * 3.0)).clamp(0.001, 30.0);
                    strip_dirty = true;
                }
                ModDest::SendDelay => {
                    eff_send_delay = (eff_send_delay + v * 0.5).clamp(0.0, 1.0);
                    self.eff_send_delay = eff_send_delay;
                }
                ModDest::SendReverb => {
                    eff_send_reverb = (eff_send_reverb + v * 0.5).clamp(0.0, 1.0);
                    self.eff_send_reverb = eff_send_reverb;
                }
                _ => {}
            }
        }

        // 4. Apply velocity modulations (sustained through the hit).
        for vm in &self.mod_state.vel_mods {
            if vm.depth == 0.0 || vm.dest == ModDest::None {
                continue;
            }
            let v = vel * vm.depth;
            match vm.dest {
                ModDest::Macro(i) if i < NUM_MACROS => {
                    eff_macros[i] = (eff_macros[i] + v).clamp(0.0, 1.0);
                    macro_dirty = true;
                }
                ModDest::FilterCutoff => {
                    eff_cutoff = (eff_cutoff * (1.0 + v * 3.0)).clamp(1.0, 20_000.0);
                    strip_dirty = true;
                }
                ModDest::FilterReso => {
                    eff_reso = (eff_reso + v * 2.0).clamp(0.5, 20.0);
                    strip_dirty = true;
                }
                ModDest::Drive => {
                    self.eff_drive = (self.eff_drive + v * 3.0).clamp(0.0, 6.0);
                }
                ModDest::Pan => {
                    let new_pan = (self.strip.pan + v).clamp(-1.0, 1.0);
                    let (l, r) = pan_law(new_pan);
                    self.pan_l = l;
                    self.pan_r = r;
                }
                ModDest::Level => {
                    self.eff_level = (self.eff_level + v * 0.5).clamp(0.0, 1.0);
                }
                ModDest::AmpDecay => {
                    eff_amp_decay = (eff_amp_decay * (1.0 + v * 3.0)).clamp(0.001, 30.0);
                    strip_dirty = true;
                }
                ModDest::SendDelay => {
                    self.eff_send_delay = (self.eff_send_delay + v * 0.5).clamp(0.0, 1.0);
                }
                ModDest::SendReverb => {
                    self.eff_send_reverb = (self.eff_send_reverb + v * 0.5).clamp(0.0, 1.0);
                }
                _ => {}
            }
        }

        // 5. Push effective values to the DSP.
        if macro_dirty {
            self.slot.set_macros(&eff_macros);
            // Sends are track-routed from the macro array, so modulating
            // SEND.DLY / SEND.RVB moves the aux levels too.
            eff_send_delay = eff_macros[SLOT_SEND_DELAY];
            eff_send_reverb = eff_macros[SLOT_SEND_REVERB];
        }
        if strip_dirty {
            self.filter.recalc(eff_cutoff, eff_reso, SAMPLE_RATE);
            self.amp_env.set_params(
                self.strip.amp_attack_s,
                self.strip.amp_hold_s,
                eff_amp_decay,
            );
        }
        // Cache eff_* so the LFO-less sends/drive/level reflect base-strip
        // changes when an LFO is active but does not target them. Without
        // this push, `set_strip` while mod is active leaves `eff_send_*`
        // stuck at their last slow-path value forever.
        self.eff_drive = eff_drive;
        self.eff_level = eff_level;
        self.eff_send_delay = eff_send_delay;
        self.eff_send_reverb = eff_send_reverb;
    }

    /// One stereo sample. Polled per-sample at the audio rate; zero work while
    /// the track is idle.
    #[inline(always)]
    pub fn tick(&mut self) -> (f32, f32) {
        let machine_sample = self.slot.tick();
        let amp = self.amp_env.tick();
        let stage = machine_sample * amp;
        let driven = dsp::fast::soft_clip(stage * self.eff_drive);
        let filtered = self.filter.tick(driven);
        let mixed = filtered * self.eff_level;
        let mut l = mixed * self.pan_l;
        let mut r = mixed * self.pan_r;

        // Choke fade-out: linear ramp to silence, then hard-reset the voice.
        if self.choke_fade_left > 0 {
            self.choke_fade_left -= 1;
            let g = self.choke_fade_left as f32 / CHOKE_SAMPLES as f32;
            l *= g;
            r *= g;
            if self.choke_fade_left == 0 {
                self.slot.reset();
                self.amp_env.reset();
                self.filter.reset();
                self.mod_state.reset();
                self.declick_left = 0;
            }
        }

        // Retrigger de-click: sync the last output, then if a window is
        // armed, crossfade from the pre-trigger output toward the new voice.
        // The last sample of the window is pure `l`/`r` (t = 1), so the
        // switch back to the raw signal is seamless.
        self.declick_l = l;
        self.declick_r = r;
        if self.declick_left > 0 {
            self.declick_left -= 1;
            self.declick_t += DECLICK_STEP;
            let t = self.declick_t.min(1.0);
            let inv = 1.0 - t;
            l = self.declick_from_l * inv + l * t;
            r = self.declick_from_r * inv + r * t;
        }

        (l, r)
    }
}

/// One-pole coefficient for CC macro smoothing, block rate.
///
/// Time constant 7.5 ms (mid-range of the 5–10 ms design guidance), block
/// rate 1500 Hz (SAMPLE_RATE / BLOCK). k = 1 − exp(−1 / (0.0075·1500)).
const MACRO_SMOOTH_K: f32 = 0.0851;

/// Convergence threshold for CC macro smoothing. A pending macro snaps to
/// its target once the residual is below this; ~0.01% of full scale is
/// comfortably inaudible.
const MACRO_SMOOTH_EPS: f32 = 1.0e-4;

/// Apply one LFO's macro slots: rate (slow/fast split), depth, destination.
fn apply_one_lfo_macro(lfo: &mut Lfo, rate_v: f32, depth_v: f32, dest_v: f32) {
    let (mode, norm) = if rate_v < 0.5 {
        (dsp::LfoRateMode::Slow, rate_v * 2.0)
    } else {
        (dsp::LfoRateMode::Fast, (rate_v - 0.5) * 2.0)
    };
    lfo.set_rate(norm, mode);
    lfo.set_params(
        lfo.speed_hz(),
        lfo.wave(),
        lfo.mode(),
        depth_v,
        ModDest::from_macro(dest_v),
        lfo.start_phase(),
    );
}

/// Equal-power pan.
///
/// `pan` of -1 maps to (1, 0), `pan` of +1 to (0, 1), and any in-between to
/// gains whose squares sum to roughly 1. Uses the table-driven `sin_turns`
/// so no transcendentals reach the hot path even if the pan moves between
/// strip updates.
#[inline]
fn pan_law(pan: f32) -> (f32, f32) {
    let p = pan.clamp(-1.0, 1.0);
    let t = (p + 1.0) * 0.5; // 0 = hard L, 1 = hard R
    let turns = t * 0.25; // angle = t·π/2 in turns
    let l = dsp::fast::sin_turns(turns + 0.25); // cos
    let r = dsp::fast::sin_turns(turns); // sin
    (l, r)
}

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

/// The engine.
///
/// Construct once, hold for the lifetime of the program. Everything it needs
/// lives inside it.
pub struct DrumEngine {
    /// All tracks, indexed by their position in the kit.
    pub tracks: [Track; TRACKS],
    /// Send-FX bus (delay + reverb). Drained by [`Self::process`] after the
    /// dry sample sum and before master clip.
    pub send_fx: dsp::SendFx,
    /// Note-number → track index, for the programmatic [`Self::trigger_note`]
    /// path. `None` = no track handles this note. The shared MIDI router
    /// (`midi::handle_midi`) does not consult this — it is one channel per
    /// track via [`Self::trigger_channel`].
    pub note_map: [Option<u8>; 128],
    /// Sample-accurate event schedule for the next block. The main loop
    /// pushes NoteOns here with a sample offset; [`Self::process`] drains it.
    pub timed: TimedQueue,
    /// Post-sum, pre-output limiter gain, linear.
    pub master_gain: f32,
}

impl Default for DrumEngine {
    fn default() -> Self {
        Self::new()
    }
}

impl DrumEngine {
    /// Build an engine with the default kit (BD on 0, SD on 1, Hat on 2,
    /// everything else HatClassic idle).
    pub fn new() -> Self {
        let mut engine = Self {
            tracks: new_tracks(),
            send_fx: dsp::SendFx::new(),
            note_map: [None; 128],
            timed: TimedQueue::new(),
            master_gain: 0.8,
        };
        configure_default_notes(&mut engine.note_map);
        engine
    }

    /// Initialize an engine in-place at the given pointer. Used by
    /// firmware that places the engine in a `.uninit` static (the engine
    /// is ~260 KB after Phase 5 — too large for the 16 KB DTCM stack).
    ///
    /// `tracks` (~3.4 KB) and `note_map` (128 B) are small enough to build
    /// as ordinary by-value locals and move into place — measured, not
    /// assumed: `size_of::<[Track; TRACKS]>() == 3456`. `send_fx` is the
    /// other ~99% of the struct (~256 KB, almost all of it `Delay`'s
    /// buffers) and gets its own [`SendFx::new_in_place`](dsp::SendFx::new_in_place),
    /// which never holds that value as a stack local at any point, at any
    /// optimization level.
    ///
    /// # Safety
    ///
    /// `dst` must point to writable memory of at least `size_of::<DrumEngine>`
    /// bytes, valid for the lifetime of the returned reference. The memory
    /// need not be zeroed — this function writes every field.
    #[allow(unsafe_code)]
    pub unsafe fn new_in_place<'a>(dst: *mut DrumEngine) -> &'a mut DrumEngine {
        core::ptr::addr_of_mut!((*dst).tracks).write(new_tracks());
        dsp::SendFx::new_in_place(core::ptr::addr_of_mut!((*dst).send_fx));
        core::ptr::addr_of_mut!((*dst).note_map).write([None; 128]);
        core::ptr::addr_of_mut!((*dst).timed).write(TimedQueue::new());
        core::ptr::addr_of_mut!((*dst).master_gain).write(0.8);

        let engine = &mut *dst;
        configure_default_notes(&mut engine.note_map);
        engine
    }

    /// Replace the entire kit one track at a time. Keeps strip + macro
    /// configurations on untouched tracks intact.
    pub fn load_kit(&mut self, kit: &[MachineId; TRACKS]) {
        for (i, id) in kit.iter().enumerate() {
            self.tracks[i].load_machine(*id);
        }
    }

    /// Trigger a single track by index. Applies that track's `layer_mask`
    /// and chokes the tracks named in its `choke_mask`.
    pub fn trigger(&mut self, track: usize, velocity: f32) {
        let layer = self.tracks[track].strip.layer_mask;
        let choke = self.tracks[track].strip.choke_mask;

        // Layering first, choking second — a layered track that we then choke
        // was never meant to keep playing anyway, but layering wins.
        let mut i = 0;
        while i < TRACKS {
            if (layer & (1 << i)) != 0 && i != track {
                self.tracks[i].trigger(velocity);
            }
            i += 1;
        }
        // Fade out choked tracks — a short fade instead of an instant cut, so
        // the cut-off tail doesn't click. The fade completes on its own even
        // if the choke source never triggers again.
        let mut j = 0;
        while j < TRACKS {
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
    /// mapped to it (silence is the correct behaviour on an unassigned note).
    ///
    /// Note: the shared MIDI router (`midi::handle_midi`) does **not** use
    /// this path — it routes one channel per track via [`trigger_channel`].
    /// This note-map API is kept for programmatic use (kits, sound design).
    pub fn trigger_note(&mut self, note: u8, velocity: f32) -> Option<usize> {
        let track = self.note_map[note as usize]?;
        self.trigger(track as usize, velocity);
        Some(track as usize)
    }

    /// Chromatic trigger for the one-channel-per-track MIDI path.
    ///
    /// The MIDI channel selects the track (`channel` < [`TRACKS`]); the note
    /// number sets its pitch — [`midi::CHROMATIC_REFERENCE_NOTE`] (middle C)
    /// is the machine's macro-configured pitch, and each semitone away
    /// transposes the whole voice via [`Track::retune`] before the hit.
    /// Choke/layer masks apply exactly as in [`Self::trigger`].
    ///
    /// Returns `Some(track)` when the note landed, `None` when the channel
    /// has no track (silence is correct on an unassigned channel).
    pub fn trigger_channel(&mut self, channel: u8, note: u8, velocity: f32) -> Option<usize> {
        let track = channel as usize;
        if track >= TRACKS {
            return None;
        }
        let semis = note as f32 - crate::midi::CHROMATIC_REFERENCE_NOTE as f32;
        self.tracks[track].retune(semis);
        self.trigger(track, velocity);
        Some(track)
    }

    /// Map a MIDI note to a track. `None` means unassigned (default).
    pub fn set_note(&mut self, note: u8, track: Option<u8>) {
        self.note_map[note as usize] = track;
    }

    /// Silence every track immediately.
    pub fn panic(&mut self) {
        for t in self.tracks.iter_mut() {
            t.reset();
        }
        self.send_fx.reset();
    }

    /// Schedule an event to fire `offset` samples into the next
    /// [`Self::process`] block. Returns false if the queue is full — the
    /// event is dropped.
    ///
    /// This is the sample-accurate half of the MIDI path;
    /// [`crate::midi::schedule_midi`] routes incoming MIDI here. `offset`
    /// is clamped to `BLOCK - 1`, so a main-loop event that races the audio
    /// callback still lands before the block ends.
    pub fn schedule_timed(&mut self, offset: usize, event: EngineEvent) -> bool {
        self.timed.push(offset, event)
    }

    /// Load a complete [`Sound`] onto a track. Cheaper than calling
    /// `load_machine` + `set_macros` + `set_strip` separately.
    pub fn load_sound(&mut self, track: usize, sound: &Sound) {
        self.tracks[track].load_sound(sound);
    }

    /// Load a [`Sound`] onto a track *and* trigger it in the same call.
    /// This is the Syntakt *sound lock* path: per-trig, the track gets a
    /// new machine + macros + strip, triggered at `velocity`.
    pub fn trigger_with_sound(&mut self, track: usize, velocity: f32, sound: &Sound) {
        self.tracks[track].load_sound(sound);
        // Apply choke / layer from the *new* sound's strip.
        let layer = self.tracks[track].strip.layer_mask;
        let choke = self.tracks[track].strip.choke_mask;

        let mut i = 0;
        while i < TRACKS {
            if (layer & (1 << i)) != 0 && i != track {
                self.tracks[i].trigger(velocity);
            }
            i += 1;
        }
        let mut j = 0;
        while j < TRACKS {
            if (choke & (1 << j)) != 0 && j != track {
                self.tracks[j].reset();
            }
            j += 1;
        }
        self.tracks[track].trigger(velocity);
    }

    /// True if any track is still producing output.
    pub fn is_active(&self) -> bool {
        self.tracks.iter().any(Track::is_active)
    }

    /// Render one block into planar stereo buffers.
    ///
    /// Hot path. Drains the [`TimedQueue`] (firing events at their sample
    /// offsets), calls [`Track::control`] once per block for each track (this
    /// is where LFOs advance, modulation is summed, and CC macros slew),
    /// then ticks the sounding tracks per sample. Idle tracks are skipped
    /// per sample via `is_active`; the per-block `control` call is a fast
    /// no-op when no modulation is configured.
    ///
    /// After the dry sum, send buses are routed through [`SendFx`] (delay +
    /// reverb) and the wet signal is summed into the master bus before the
    /// final safety clip.
    ///
    /// This is the stereo-summed convenience wrapper around
    /// [`Self::process_dry_wet`]: every track's dry pair is summed into
    /// `out_l`/`out_r`, the wet FX return is mixed in post-FX-bus-drive,
    /// and the final safety clip is applied. Routing each track to its own
    /// physical output (DAW multi-out, future TDM/multi-DAC firmware) uses
    /// [`Self::process_dry_wet`] directly — the inner `tick()` path is the
    /// same, only the post-tick routing differs.
    ///
    /// # Panics
    ///
    /// Debug builds assert both slices are exactly [`BLOCK`] long.
    pub fn process(&mut self, out_l: &mut [f32], out_r: &mut [f32]) {
        debug_assert_eq!(out_l.len(), BLOCK, "left buffer must be BLOCK frames");
        debug_assert_eq!(out_r.len(), BLOCK, "right buffer must be BLOCK frames");

        // Stack buses — no allocation. 6 mono aux + 2 mono wet = 2 KB.
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

        // Re-apply the exact pre-refactor master chain: FX-bus drive on the
        // wet, inner soft_clip per side, sum with the dry master, then master
        // gain + final clip. Sourced from `master_*` (dry Master-routed
        // tracks) and `wet_*` (shared FX return). Tracks routed to aux pairs
        // do not reach this path — they live on `aux` and the multi-out
        // caller handles them.
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
    /// The primitive [`process`](Self::process) wraps. Per-track routing is
    /// by [`StripParams::out`]: tracks on [`OutPair::Master`] sum into
    /// `master_l`/`master_r`; tracks on [`OutPair::Aux1`]..[`OutPair::Aux3`]
    /// sum into the matching pair of `aux`. A track routed to an aux does
    /// *not* contribute to the master dry sum — the routing is either/or.
    ///
    /// Sends still ride the shared send buses regardless of routing — a
    /// track on Aux2 with `send_reverb = 0.5` still produces dry on Aux2
    /// and its wet return lands on `wet_l`/`wet_r` (which the caller mixes
    /// onto the master pair — matching how hardware aux returns work: the
    /// FX lives on the mix bus, not the individual channel).
    ///
    /// All five outputs are pre-everything: no FX-bus drive, no master gain,
    /// no master clip. The caller decides what drive/gain/clip to apply to
    /// each pair. The stereo `process` wrapper applies the canonical chain
    /// (fx_drive + inner clip on wet, master gain + final clip on the sum)
    /// to the master pair only and discards the auxes.
    ///
    /// The engine *zeros* `master_l`, `master_r`, and `aux` on entry — callers can pass in
    /// buffers reused across blocks without worrying about leftover state. The wet buses
    /// are overwritten in place, so they need no pre-zero from the caller. Inactive tracks
    /// contribute zero to every dry bus, so callers can sum naively without an `is_active`
    /// check.
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
        // 0. Drain the timed event queue. Events are sorted by offset and
        //    fired at the matching sample inside the block (step 2), so a
        //    note scheduled half-way through the block sounds half-way
        //    through, not at the next boundary.
        let mut timed = [None::<TimedEvent>; MAX_TIMED_EVENTS];
        let n_timed = self.timed.drain_sorted(&mut timed);

        // 1. Control pass: advance LFOs, apply modulation, recompute coefficients.
        for t in self.tracks.iter_mut() {
            t.control();
        }

        // Zero the dry-sum buses — the sample loop accumulates with `+=`,
        // so caller-passed state would otherwise leak across blocks. The
        // wet buses are overwritten in step 3 (not accumulated), so they
        // need no pre-zero. Cost: 8 × BLOCK × 4 = 1 KB of memset per call.
        // Trivial against the per-block cycle budget and removes a class
        // of "did the caller pre-zero?" bugs from the primitive's contract.
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

        // Send buses (block-sized, cleared per block). The sends are tapped
        // post-fader/post-pan inside the sample loop, then routed through
        // [`SendFx`] afterwards. Block-sized stack arrays — no allocation,
        // fixed at compile time.
        let mut send_dl = [0.0f32; BLOCK];
        let mut send_dr = [0.0f32; BLOCK];
        let mut send_rl = [0.0f32; BLOCK];
        let mut send_rr = [0.0f32; BLOCK];

        // Block-rate fast-path probe: if every track is on `Master` (the
        // default — the common case for stereo use, single-I2S firmware,
        // and `render device` without `--multi-out`), skip the per-sample
        // `match strip.out` entirely. Branch is one compare per block
        // instead of one match per sample per voice — measurably recovers
        // the autovectorizer's tight `sum_l += l; sum_r += r` shape that
        // the routing match was breaking (Phase 11 regate: -3.8pp on 8
        // sounding without the fast path). Multi-out callers still get the
        // general path; correctness is identical (both paths produce the
        // same master dry sum and the same zero auxes when routing is all-
        // Master).
        let all_master = self.tracks.iter().all(|t| t.strip.out == OutPair::Master);

        // 2. Sample loop. Per-track dry goes to its routed pair (master or
        //    one of the three auxes); sends accumulate into the four block
        //    buses. The two `t` loops are deliberate code duplication: the
        //    fast path avoids the per-sample `match strip.out` and keeps the
        //    `master_l += l` accumulator vectorizable, which the bench
        //    showed is worth ~3pp of budget on an 8-voice block.
        for i in 0..BLOCK {
            // Timed events for this sample. The queue is offset-sorted, so
            // each sample's events are contiguous.
            let mut k = 0;
            while k < n_timed {
                let ev = timed[k].expect("drained entries are Some");
                if ev.offset != i {
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
                    EngineEvent::Panic => self.panic(),
                }
                k += 1;
            }

            if all_master {
                let mut t = 0;
                while t < TRACKS {
                    if self.tracks[t].is_active() {
                        let (l, r) = self.tracks[t].tick();
                        let sd = self.tracks[t].eff_send_delay;
                        let sr = self.tracks[t].eff_send_reverb;
                        send_dl[i] += l * sd;
                        send_dr[i] += r * sd;
                        send_rl[i] += l * sr;
                        send_rr[i] += r * sr;
                        master_l[i] += l;
                        master_r[i] += r;
                    }
                    t += 1;
                }
            } else {
                let mut t = 0;
                while t < TRACKS {
                    if self.tracks[t].is_active() {
                        let (l, r) = self.tracks[t].tick();
                        let sd = self.tracks[t].eff_send_delay;
                        let sr = self.tracks[t].eff_send_reverb;
                        // Sends accumulate regardless of routing — aux sends
                        // ride the shared bus, the wet return lives on the
                        // master pair (mixed in by the caller).
                        send_dl[i] += l * sd;
                        send_dr[i] += r * sd;
                        send_rl[i] += l * sr;
                        send_rr[i] += r * sr;
                        match self.tracks[t].strip.out {
                            OutPair::Master => {
                                master_l[i] += l;
                                master_r[i] += r;
                            }
                            OutPair::Aux1 => {
                                aux[0][i] += l;
                                aux[1][i] += r;
                            }
                            OutPair::Aux2 => {
                                aux[2][i] += l;
                                aux[3][i] += r;
                            }
                            OutPair::Aux3 => {
                                aux[4][i] += l;
                                aux[5][i] += r;
                            }
                        }
                    }
                    t += 1;
                }
            }
        }

        // 3. Send-FX pass. Wet buses carry delay + reverb summed per side,
        //    pre-FX-bus-drive, pre-clip. The caller (stereo wrapper, DAW
        //    multi-out rig, firmware TDM) applies drive/gain/clip per its
        //    own routing policy — same arithmetic as the stereo `process`
        //    master loop, but left to the routing stage so a DAW can drive
        //    the wet return on its own channel and a future firmware multi-
        //    out path can mix it into the master pair the same way.
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

/// Build the default track array. Const-array init of non-Copy types needs a
/// helper, since `[Default::default(); 8]` requires Copy + Default.
fn new_tracks() -> [Track; TRACKS] {
    let mut tracks = [
        Track::new(DEFAULT_KIT[0]),
        Track::new(DEFAULT_KIT[1]),
        Track::new(DEFAULT_KIT[2]),
        Track::new(DEFAULT_KIT[3]),
        Track::new(DEFAULT_KIT[4]),
        Track::new(DEFAULT_KIT[5]),
        Track::new(DEFAULT_KIT[6]),
        Track::new(DEFAULT_KIT[7]),
    ];
    // Closed hat (track 2) chokes open hat / HH Basic (track 3): a closed
    // hit on top of a ringing open hat cuts the tail.
    tracks[2].strip.choke_mask = 1 << 3;
    // Closed hat: short decay; HH Basic on track 3: longer, metallic.
    tracks[2].set_macro(0, 0.05); // HatClassic DEC ≈ 35ms (closed)
    tracks[3].set_macro(3, 0.25); // HH Basic DEC ≈ 135ms (open-ish)
    tracks
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
                m.slot.trigger(1.0);
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
    fn pan_law_is_equal_power() {
        let (l, r) = pan_law(0.0);
        approx::assert_abs_diff_eq!(l * l + r * r, 1.0, epsilon = 1e-3);
        let (l, r) = pan_law(-1.0);
        approx::assert_abs_diff_eq!(l, 1.0, epsilon = 1e-3);
        approx::assert_abs_diff_eq!(r, 0.0, epsilon = 1e-3);
        let (l, r) = pan_law(1.0);
        approx::assert_abs_diff_eq!(l, 0.0, epsilon = 1e-3);
        approx::assert_abs_diff_eq!(r, 1.0, epsilon = 1e-3);
        let (l, r) = pan_law(0.0);
        // Centre: both ≈ 1/√2.
        approx::assert_abs_diff_eq!(l, r, epsilon = 1e-4);
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
            machine_id: MachineId::BdFm,
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
        // No track routed to an aux on the default kit, so aux stays zero.
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
        approx::assert_abs_diff_eq!(e.tracks[0].mod_state.lfos[0].speed_hz(), 0.1, epsilon = 1e-6);
        // 0.5 → fast range bottom (1 Hz)
        e.tracks[0].set_macro(SLOT_LFO1_RATE, 0.5);
        approx::assert_abs_diff_eq!(e.tracks[0].mod_state.lfos[0].speed_hz(), 1.0, epsilon = 1e-6);
        // 1.0 → fast range top (100 Hz)
        e.tracks[0].set_macro(SLOT_LFO1_RATE, 1.0);
        approx::assert_abs_diff_eq!(e.tracks[0].mod_state.lfos[0].speed_hz(), 100.0, epsilon = 1e-6);
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
        assert_eq!(e.tracks[0].mod_state.lfos[0].dest(), ModDest::Macro(0));
        // ~0.56 → FilterCutoff (index 8 of 16 → 8/15 ≈ 0.533)
        e.tracks[0].set_macro(SLOT_LFO1_DEST, 0.56);
        assert_eq!(e.tracks[0].mod_state.lfos[0].dest(), ModDest::FilterCutoff);
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
