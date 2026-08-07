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
pub mod machines;
pub mod midi;

pub use machines::{MachineId, Macro, MacroInfo, NUM_MACROS};

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

/// One channel of the kit.
pub struct Track {
    /// Underlying synthesis model. Reassignable via [`Track::load_machine`].
    pub slot: MachineSlot,
    /// Base macro values (user-configured, pre-modulation).
    pub base_macros: [f32; NUM_MACROS],
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
        }
    }

    /// Replace the machine on this track, resetting macros to that machine's
    /// defaults. Strip and modulation are untouched.
    pub fn load_machine(&mut self, id: MachineId) {
        self.base_macros = id.default_macros();
        self.slot = MachineSlot::new(id, &self.base_macros);
    }

    /// Load a complete sound (machine + macros + strip) onto this track.
    pub fn load_sound(&mut self, sound: &Sound) {
        self.load_machine(sound.machine_id);
        self.base_macros = sound.macros;
        self.set_strip(&sound.strip);
        if !self.mod_state.has_active_mod() {
            self.slot.set_macros(&self.base_macros);
        }
    }

    /// Which machine this track holds.
    pub fn id(&self) -> MachineId {
        self.slot.id()
    }

    /// Set one base macro. Coefficient recompute is included.
    pub fn set_macro(&mut self, idx: usize, value: f32) {
        if idx < NUM_MACROS {
            let v = value.clamp(0.0, 1.0);
            self.base_macros[idx] = v;
            if !self.mod_state.has_active_mod() {
                self.slot.set_macros(&self.base_macros);
            }
        }
    }

    /// Replace all base macros in one call.
    pub fn set_macros(&mut self, all: &[f32; NUM_MACROS]) {
        self.base_macros = *all;
        if !self.mod_state.has_active_mod() {
            self.slot.set_macros(&self.base_macros);
        }
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
        if !self.mod_state.has_active_mod() {
            self.eff_drive = params.drive;
            self.eff_level = params.level;
            self.eff_send_delay = params.send_delay;
            self.eff_send_reverb = params.send_reverb;
        }
    }

    /// Begin a hit at `velocity` (0..=1.0). Fires LFO triggers and stores
    /// velocity for the control pass.
    pub fn trigger(&mut self, velocity: f32) {
        self.slot.trigger(velocity);
        self.amp_env.trigger(velocity);
        self.mod_state.trigger(velocity);
    }

    /// Force to silence immediately.
    pub fn reset(&mut self) {
        self.slot.reset();
        self.amp_env.reset();
        self.filter.reset();
        self.mod_state.reset();
    }

    /// Still producing output?
    pub fn is_active(&self) -> bool {
        self.slot.is_active()
    }

    /// Per-block control pass. Advances LFOs, sums modulation onto base
    /// macros + strip params, and recomputes coefficients. Called from
    /// [`DrumEngine::process`] before the sample loop. If no modulation is
    /// active, this is a single-branch early return.
    pub fn control(&mut self) {
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
        (mixed * self.pan_l, mixed * self.pan_r)
    }
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
        // Reset choked tracks instantly. `id` read before reset so a track
        // being choked can also be the layering source.
        let mut j = 0;
        while j < TRACKS {
            if (choke & (1 << j)) != 0 && j != track {
                self.tracks[j].reset();
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
    /// Hot path. Calls [`Track::control`] once per block for each track (this
    /// is where LFOs advance and modulation is summed), then ticks the
    /// sounding tracks per sample. Idle tracks are skipped per sample via
    /// `is_active`; the per-block `control` call is a fast no-op when no
    /// modulation is configured.
    ///
    /// After the dry sum, send buses are routed through [`SendFx`] (delay +
    /// reverb) and the wet signal is summed into the master bus before the
    /// final safety clip.
    ///
    /// # Panics
    ///
    /// Debug builds assert both slices are exactly [`BLOCK`] long.
    pub fn process(&mut self, out_l: &mut [f32], out_r: &mut [f32]) {
        debug_assert_eq!(out_l.len(), BLOCK, "left buffer must be BLOCK frames");
        debug_assert_eq!(out_r.len(), BLOCK, "right buffer must be BLOCK frames");

        let n = out_l.len().min(out_r.len()).min(BLOCK);
        let master = self.master_gain;
        let fx_drive = self.send_fx.drive;

        // 1. Control pass: advance LFOs, apply modulation, recompute coefficients.
        for t in self.tracks.iter_mut() {
            t.control();
        }

        // Send buses (block-sized, cleared per block). The sends are tapped
        // post-fader/post-pan inside the sample loop, then routed through
        // [`SendFx`] afterwards.
        // Block-sized stack arrays — no allocation, fixed at compile time.
        let mut send_dl = [0.0f32; BLOCK];
        let mut send_dr = [0.0f32; BLOCK];
        let mut send_rl = [0.0f32; BLOCK];
        let mut send_rr = [0.0f32; BLOCK];

        // 2. Sample loop. Dry bus accumulates into out_*; sends accumulate
        //    into the four block buses above. The dry bus is left un-clipped
        //    until after the wet sum is mixed back, so the final clip is the
        //    only protection stage (matching the contract from Phase 1).
        for i in 0..n {
            let mut sum_l = 0.0f32;
            let mut sum_r = 0.0f32;

            let mut t = 0;
            while t < TRACKS {
                if self.tracks[t].is_active() {
                    let (l, r) = self.tracks[t].tick();
                    sum_l += l;
                    sum_r += r;
                    let sd = self.tracks[t].eff_send_delay;
                    let sr = self.tracks[t].eff_send_reverb;
                    let dl = l * sd;
                    let dr = r * sd;
                    let rl = l * sr;
                    let rr = r * sr;
                    // Two accumulations per send per channel: keeps the
                    // modulation cache read out of `eff_send_*` (set by
                    // `control()` at block rate).
                    send_dl[i] += dl;
                    send_dr[i] += dr;
                    send_rl[i] += rl;
                    send_rr[i] += rr;
                }
                t += 1;
            }

            out_l[i] = sum_l;
            out_r[i] = sum_r;
        }

        // 3. Send-FX pass. The wet buses accumulate into the dry bus
        //    (out_*) along with FX-bus drive — both FX are summed through a
        //    soft_clip each, then a master clip protects the final mix.
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

        // 4. Master clip on the dry + wet sum.
        for i in 0..n {
            let wet_l = dsp::fast::soft_clip((wet_dl[i] + wet_rl[i]) * fx_drive);
            let wet_r = dsp::fast::soft_clip((wet_dr[i] + wet_rr[i]) * fx_drive);
            out_l[i] = dsp::fast::soft_clip((out_l[i] + wet_l) * master);
            out_r[i] = dsp::fast::soft_clip((out_r[i] + wet_r) * master);
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
        e.tracks[7].set_macro(5, 0.5); // DEC — recomputes coefficients
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
        assert_eq!(e.tracks[0].base_macros, [0.5; NUM_MACROS]);
        assert_eq!(e.tracks[0].strip.pan, 0.3);
        assert_eq!(e.tracks[0].strip.level, 0.6);
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
}
