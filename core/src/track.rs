//! One channel / track of a device engine.
//!
//! A [`Track`] is generic over a [`Slot`](crate::slot::Slot) so the same
//! strip (filter, amp envelope, drive, pan, level, sends), modulation (LFOs +
//! velocity mods), CC-param smoothing, choke/layer logic and de-click/choke
//! fades can host drum machines, FM voices, or any other no-alloc voice.

use crate::dsp::{fast, AhdEnv, Lfo, ModDest, Svf};
use crate::params::{
    ParamRoute, SendBus, MAX_PARAMS, PARAM_MASK_WORDS, SLOT_LFO1_DEPTH, SLOT_LFO1_DEST,
    SLOT_LFO1_RATE, SLOT_LFO2_DEPTH, SLOT_LFO2_DEST, SLOT_LFO2_RATE, SLOT_MACHINE, SLOT_OUT,
    SLOT_PAN, SLOT_SEND_DELAY, SLOT_SEND_REVERB, SLOT_STRIP_ATK, SLOT_STRIP_CUT, SLOT_STRIP_DEC,
    SLOT_STRIP_HOLD, SLOT_STRIP_RESO,
};
use crate::slot::{DeviceModel, Slot, SlotId};
use crate::sound::Sound;
use crate::strip::StripParams;
use crate::OutPair;
use crate::BLOCK;
use crate::SAMPLE_RATE;

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
    /// effective params/strip for the duration of the hit.
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
pub const CHOKE_SAMPLES: usize = 240;

/// Which params have a smoothing ramp pending, one bit per param.
///
/// Word-wise, so the advance walks only the words a track can use and visits
/// only the bits that are set, and so a device can expose more than 32 params.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
struct ParamMask {
    words: [u32; PARAM_MASK_WORDS],
}

impl ParamMask {
    const EMPTY: Self = Self {
        words: [0; PARAM_MASK_WORDS],
    };

    fn set(&mut self, idx: usize) {
        self.words[idx / 32] |= 1 << (idx % 32);
    }

    fn clear(&mut self, idx: usize) {
        self.words[idx / 32] &= !(1 << (idx % 32));
    }

    fn is_set(&self, idx: usize) -> bool {
        self.words[idx / 32] & (1 << (idx % 32)) != 0
    }

    /// True if any bit is set in the first `words` words.
    fn any(&self, words: usize) -> bool {
        self.words[..words].iter().any(|&w| w != 0)
    }
}

/// One channel of a device.
pub struct Track<S, const N: usize>
where
    S: Slot<N>,
{
    /// Underlying synthesis model. Reassignable via [`Track::load_machine`].
    pub slot: S,
    /// Base param values (user-configured, pre-modulation).
    pub base_params: [f32; N],
    /// CC param targets: [`Track::control`] slews `base_params` toward these
    /// at block rate. Only params touched by [`Track::set_param_target`] are
    /// tracked (see `param_pending`); everything else is direct. A direct
    /// set updates the target as well as the base, so this always names the
    /// value the param is *heading to* — which is what an NRPN increment
    /// reads (see [`Track::param_target`]).
    param_targets: [f32; N],
    /// One-pole state used to slew a pending param toward its target.
    param_smooth: [f32; N],
    /// Bitmask: bit `i` set = param `i` is mid-ramp toward `param_targets[i]`.
    param_pending: ParamMask,
    /// Strip configuration (filter, amp env, drive, pan, level, choke, layer).
    pub strip: StripParams,
    /// LFOs + velocity modulation.
    pub mod_state: ModState,

    // ---- strip runtime state ----
    filter: Svf,
    amp_env: AhdEnv,
    // Equal-power pan gains, recomputed when pan changes.
    pan_l: f32,
    pan_r: f32,
    // Effective drive/level/pan after modulation, used in `tick`.
    eff_drive: f32,
    eff_level: f32,
    /// Effective delay send after modulation, used in `tick`. Post-fader.
    pub eff_send_delay: f32,
    /// Effective reverb send after modulation.
    pub eff_send_reverb: f32,

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
    /// Mono source samples collected for the current segment. Filled by the
    /// engine via [`Slot::tick`](crate::slot::Slot::tick) so that source voices
    /// with a shared random generator stay sample-interleaved across tracks;
    /// the strip then processes this buffer block-wise.
    pub(crate) source_segment: [f32; BLOCK],
    /// When true, skip the built-in filter/amp-env/drive strip. Devices such as
    /// mi-drum implement their own audio strip inside the slot (Warps →
    /// Ripples) and only need pan/level/sends/choke/de-click from Track.
    pub strip_bypass: bool,
}

impl<S, const N: usize> Track<S, N>
where
    S: Slot<N>,
{
    /// Words of the pending mask that can hold one of this track's `N` params.
    ///
    /// Evaluating this asserts `N <= MAX_PARAMS`, so a device with too many
    /// params fails to build the first time its track runs.
    const MASK_WORDS: usize = {
        assert!(
            N <= MAX_PARAMS,
            "a device exposes at most MAX_PARAMS params"
        );
        N.div_ceil(32)
    };

    /// Build a track loaded with the given slot and that slot's default params
    /// + the default strip.
    pub fn new(id: S::Id) -> Self {
        let base_params = S::Id::default_params(id);
        let strip = StripParams::default();
        let slot = S::new(id, &base_params);
        let mut filter = Svf::new(strip.f_mode);
        filter.recalc(strip.f_cutoff_hz, strip.f_reso_q, SAMPLE_RATE);
        let mut amp_env = AhdEnv::new();
        amp_env.set_params(strip.amp_attack_s, strip.amp_hold_s, strip.amp_decay_s);
        let (pan_l, pan_r) = pan_law(strip.pan);
        // The MACH slot always mirrors the loaded slot.
        let mut base_params = base_params;
        let divisor = S::Id::count().saturating_sub(1).max(1) as f32;
        base_params[SLOT_MACHINE] = id.index() as f32 / divisor;
        Self {
            slot,
            base_params,
            param_targets: base_params,
            param_smooth: base_params,
            param_pending: ParamMask::EMPTY,
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
            source_segment: [0.0f32; BLOCK],
            strip_bypass: false,
        }
    }

    /// Build a track directly in the memory pointed to by `ptr`.
    ///
    /// The slot is constructed in-place via [`Slot::new_in_place`], which is
    /// required for slots whose C++ state contains internal pointers.
    ///
    /// # Safety
    ///
    /// `ptr` must be valid for writes and properly aligned for `Track<S, N>`.
    #[allow(unsafe_code)]
    pub unsafe fn new_in_place(id: S::Id, ptr: *mut Track<S, N>) {
        let base_params = S::Id::default_params(id);
        let strip = StripParams::default();

        // Construct the slot in-place at its final address. This must happen
        // before any move of the surrounding Track, because some slots own
        // self-referential C++ state.
        let slot_ptr = core::ptr::addr_of_mut!((*ptr).slot);
        S::new_in_place(id, &base_params, slot_ptr);

        // Now write every other field of the track at its final address.
        let mut filter = Svf::new(strip.f_mode);
        filter.recalc(strip.f_cutoff_hz, strip.f_reso_q, SAMPLE_RATE);
        let mut amp_env = AhdEnv::new();
        amp_env.set_params(strip.amp_attack_s, strip.amp_hold_s, strip.amp_decay_s);
        let (pan_l, pan_r) = pan_law(strip.pan);

        let mut base_params = base_params;
        let divisor = S::Id::count().saturating_sub(1).max(1) as f32;
        base_params[SLOT_MACHINE] = id.index() as f32 / divisor;

        core::ptr::addr_of_mut!((*ptr).base_params).write(base_params);
        core::ptr::addr_of_mut!((*ptr).param_targets).write(base_params);
        core::ptr::addr_of_mut!((*ptr).param_smooth).write(base_params);
        core::ptr::addr_of_mut!((*ptr).param_pending).write(ParamMask::EMPTY);
        core::ptr::addr_of_mut!((*ptr).strip).write(strip);
        core::ptr::addr_of_mut!((*ptr).mod_state).write(ModState::new());
        core::ptr::addr_of_mut!((*ptr).filter).write(filter);
        core::ptr::addr_of_mut!((*ptr).amp_env).write(amp_env);
        core::ptr::addr_of_mut!((*ptr).pan_l).write(pan_l);
        core::ptr::addr_of_mut!((*ptr).pan_r).write(pan_r);
        core::ptr::addr_of_mut!((*ptr).eff_drive).write(strip.drive);
        core::ptr::addr_of_mut!((*ptr).eff_level).write(strip.level);
        core::ptr::addr_of_mut!((*ptr).eff_send_delay).write(strip.send_delay);
        core::ptr::addr_of_mut!((*ptr).eff_send_reverb).write(strip.send_reverb);
        core::ptr::addr_of_mut!((*ptr).declick_left).write(0);
        core::ptr::addr_of_mut!((*ptr).declick_t).write(0.0);
        core::ptr::addr_of_mut!((*ptr).declick_from_l).write(0.0);
        core::ptr::addr_of_mut!((*ptr).declick_from_r).write(0.0);
        core::ptr::addr_of_mut!((*ptr).declick_l).write(0.0);
        core::ptr::addr_of_mut!((*ptr).declick_r).write(0.0);
        core::ptr::addr_of_mut!((*ptr).choke_fade_left).write(0);
        core::ptr::addr_of_mut!((*ptr).source_segment).write([0.0f32; BLOCK]);
        core::ptr::addr_of_mut!((*ptr).strip_bypass).write(false);
    }

    /// Replace the slot on this track, resetting params to its defaults. Strip
    /// and modulation are untouched.
    #[allow(unsafe_code)]
    pub fn load_machine(&mut self, id: S::Id) {
        self.base_params = S::Id::default_params(id);
        // Sends are track-level, not slot-level: keep the strip's aux levels
        // and mirror them back into the param slots so the two stay consistent.
        self.base_params[SLOT_SEND_DELAY] = self.strip.send_delay;
        self.base_params[SLOT_SEND_REVERB] = self.strip.send_reverb;
        // Pan is track-level too: keep the strip's pan mirrored in the param.
        self.base_params[SLOT_PAN] = self.strip.pan * 0.5 + 0.5;
        // The MACH slot always mirrors the loaded slot (see `load_sound`).
        let divisor = S::Id::count().saturating_sub(1).max(1) as f32;
        self.base_params[SLOT_MACHINE] = id.index() as f32 / divisor;
        // Construct the new slot in-place at its existing address. Slots that
        // own self-referential state (e.g. C++ voices with internal buffer
        // pointers) cannot survive a stack-to-heap move.
        //
        // This goes through `load_in_place` rather than `new_in_place`
        // because the slot is already constructed here, so a slot holding
        // pooled or otherwise reclaimable resources gets the chance to hand
        // them back before it is overwritten. `new_in_place` also runs on
        // never-constructed memory, so it must not read the old contents.
        //
        // SAFETY: `&mut self.slot` is a valid, aligned pointer to the slot's
        // existing storage, and the slot was built by a previous
        // `new_in_place`, so `load_in_place`'s contract holds.
        unsafe { S::load_in_place(id, &self.base_params, &mut self.slot) };
        // Params reset wholesale: the CC smoother must follow, not ramp from
        // a value that no longer means anything on the new voice.
        self.sync_param_smoothing();
    }

    /// Load a complete sound (slot + params + strip) onto this track.
    pub fn load_sound(&mut self, sound: &Sound<S, N>) {
        self.load_machine(sound.id);
        self.set_strip(&sound.strip);
        // The sound's params are authoritative for its sends (they round-trip
        // verbatim between host and device); derive the strip aux levels from
        // them so the two stay consistent.
        self.base_params = sound.params;
        // The MACH slot always mirrors the loaded slot, whatever the sound's
        // param array carried.
        let divisor = S::Id::count().saturating_sub(1).max(1) as f32;
        self.base_params[SLOT_MACHINE] = sound.id.index() as f32 / divisor;
        self.strip.send_delay = self.base_params[SLOT_SEND_DELAY];
        self.strip.send_reverb = self.base_params[SLOT_SEND_REVERB];
        self.eff_send_delay = self.base_params[SLOT_SEND_DELAY];
        self.eff_send_reverb = self.base_params[SLOT_SEND_REVERB];
        if !self.mod_state.has_active_mod() {
            self.slot.set_params(&self.base_params);
        }
        // base_params was overwritten by hand after load_machine's sync;
        // re-sync the CC smoother to the final array.
        self.sync_param_smoothing();
    }

    /// Which slot this track holds.
    pub fn id(&self) -> S::Id {
        self.slot.id()
    }

    /// Set one base param. Coefficient recompute is included.
    ///
    /// Where the value goes is data: the machine's parameter table vends a
    /// [`ParamRoute`] per slot ([`DeviceModel::param_route`]), and the match
    /// below replaces what used to be a cascade of flat-index comparisons. A
    /// device decides per row whether a value drives the core strip, the
    /// modulation bus, the sends, or the voice — so mi-drum's strip/shaper
    /// rows route to the slot even at indices the drum device routes to the
    /// strip, and no `strip_bypass` special case is needed in the control
    /// path.
    ///
    /// A direct set is also an absolute statement of where the param is
    /// *heading*: the smoothing target follows the stored value, so
    /// `param_targets` never holds a destination the param has been set
    /// away from. The read side ([`param_target`](Self::param_target)) is
    /// where a relative write — an NRPN data increment — nudges from. The
    /// ramp itself writes through [`set_param_inner`](Self::set_param_inner):
    /// an intermediate value must not move the destination it is heading to.
    pub fn set_param(&mut self, idx: usize, value: f32) {
        self.set_param_inner(idx, value);
        if idx < N {
            self.param_targets[idx] = self.param_smooth[idx];
        }
    }

    /// [`set_param`](Self::set_param)'s route dispatch, without the target
    /// restatement. The CC smoother writes each ramp value through here
    /// (`advance_param_smoothing`) while the destination stays where
    /// [`set_param_target`](Self::set_param_target) put it.
    fn set_param_inner(&mut self, idx: usize, value: f32) {
        if idx >= N {
            return;
        }
        let v = value.clamp(0.0, 1.0);
        let route = self.slot.id().param_route(idx);
        match route {
            // Machine selector: quantise 0..1 onto the slot catalogue.
            // Track-routed, so `(TRACK, 0)` swaps engines without a
            // program change.
            // Loading a different slot resets params to its defaults — the
            // old voice's param values mean nothing on the new one.
            ParamRoute::Machine => {
                let count = S::Id::count();
                if count <= 1 {
                    return;
                }
                let i = (v * (count - 1) as f32) as usize;
                if let Some(id) = S::Id::from_index(i) {
                    if id != self.slot.id() {
                        self.load_machine(id);
                    }
                }
                return;
            }
            // Output routing: quantise 0..1 onto the 4 `OutPair` variants and
            // write `strip.out`. Same instant-jump discipline as the machine
            // selector — routing is a discrete choice, smoothing it across
            // blocks would route a track to a half-pair. Stores the quantised
            // param value back so MIDI feedback / round-trip reads return the
            // canonical centre of the variant.
            ParamRoute::Out => {
                let i = (v * (OutPair::COUNT - 1) as f32 + 0.5) as usize;
                self.strip.out = OutPair::ALL[i];
                let q = i as f32 / (OutPair::COUNT - 1) as f32;
                self.base_params[idx] = q;
                self.param_smooth[idx] = q;
                self.param_pending.clear(idx);
                return;
            }
            // Track-routed strip and modulation params: store, then apply the
            // derived strip coefficients / LFO state immediately. Not
            // forwarded on this set — the voice is not what consumes them.
            ParamRoute::Strip(_) => {
                self.base_params[idx] = v;
                self.param_smooth[idx] = v;
                self.param_pending.clear(idx);
                self.apply_strip_params();
                return;
            }
            ParamRoute::Lfo(_) => {
                self.base_params[idx] = v;
                self.param_smooth[idx] = v;
                self.param_pending.clear(idx);
                self.apply_lfo_params();
                return;
            }
            // Everything else stores first.
            ParamRoute::Slot | ParamRoute::Pan | ParamRoute::Send(_) | ParamRoute::Level => {}
        }
        self.base_params[idx] = v;
        // Keep the CC smoother in sync: a direct set is the new current
        // value, and any pending ramp to a stale target is cancelled.
        self.param_smooth[idx] = v;
        self.param_pending.clear(idx);
        match route {
            // The send params drive the strip's aux levels (and the effective
            // values read in the per-sample path), not the slot DSP.
            ParamRoute::Send(SendBus::Delay) => {
                self.strip.send_delay = v;
                self.eff_send_delay = v;
            }
            ParamRoute::Send(SendBus::Reverb) => {
                self.strip.send_reverb = v;
                self.eff_send_reverb = v;
            }
            ParamRoute::Pan => {
                // Param 0..1 maps onto the bipolar -1..+1 strip pan (0.5 =
                // centre). `tick` reads the cached pan gains, so recompute
                // them here or the change is inaudible.
                self.strip.pan = v * 2.0 - 1.0;
                let (l, r) = pan_law(self.strip.pan);
                self.pan_l = l;
                self.pan_r = r;
            }
            // `Slot` (reserved rows included) and `Level` have no track-side
            // work: the voice consumes them from the param array.
            ParamRoute::Slot | ParamRoute::Level => {}
            // Unreachable: every remaining variant returned above.
            ParamRoute::Strip(_) | ParamRoute::Lfo(_) | ParamRoute::Out | ParamRoute::Machine => {}
        }
        if !self.mod_state.has_active_mod() {
            self.slot.set_params(&self.base_params);
        }
    }

    /// True while param `idx` is mid-ramp toward its target.
    pub fn is_param_pending(&self, idx: usize) -> bool {
        idx < N && self.param_pending.is_set(idx)
    }

    /// The value param `idx` is heading to: the pending target if one is
    /// armed, otherwise the current value.
    ///
    /// The read side of [`set_param_target`](Self::set_param_target). It
    /// matters for *relative* writes — NRPN data increment/decrement
    /// (`CC 96/97`, see
    /// [`apply_nrpn_step`](crate::midi::apply_nrpn_step)): a burst of steps
    /// arriving inside one block must accumulate on the destination, not
    /// the unmoved base, or every step after the first would overwrite
    /// its predecessors and a fast turn would edit one LSB. Reading the
    /// base instead would be right only for a controller that sends at
    /// most one step per block.
    pub fn param_target(&self, idx: usize) -> f32 {
        if idx < N {
            self.param_targets[idx]
        } else {
            0.0
        }
    }

    /// Set the *target* for a CC-driven param.
    ///
    /// The value ramps to it at block rate via [`Track::control`] (see
    /// [`PARAM_SMOOTH_K`]), so a burst of CC messages never triggers a
    /// coefficient recompute per message — `set_param`, the recompute call,
    /// runs at most once per block per moving param. The slot selector jumps
    /// instantly: swapping engines must be immediate, and loading a slot
    /// resets params to its defaults anyway.
    ///
    /// Control rate, main loop only — never the audio interrupt.
    pub fn set_param_target(&mut self, idx: usize, value: f32) {
        if idx >= N {
            return;
        }
        // The two instant routes bypass the smoother: ramping a discrete
        // choice would walk a track through every output pair, or load
        // every machine between here and the target. Dispatched on the
        // route rather than on a flat index, so a device that puts them
        // elsewhere in its table still gets the jump.
        if matches!(
            self.slot.id().param_route(idx),
            ParamRoute::Machine | ParamRoute::Out
        ) {
            self.set_param(idx, value);
            return;
        }
        let v = value.clamp(0.0, 1.0);
        self.param_targets[idx] = v;
        // Seed the ramp from wherever the param actually is now, so a
        // retarget mid-ramp (a knob being turned) continues smoothly from
        // the current value rather than jumping back to the block base.
        self.param_smooth[idx] = self.base_params[idx];
        self.param_pending.set(idx);
    }

    /// Reset the CC smoother to match the current param array: pending ramps
    /// cancelled, targets and smoother seeded from base values.
    fn sync_param_smoothing(&mut self) {
        self.param_targets = self.base_params;
        self.param_smooth = self.base_params;
        self.param_pending = ParamMask::EMPTY;
    }

    /// Advance every pending param one block toward its target. At most one
    /// param dispatch per moving param per block, so a CC burst costs one
    /// coefficient recompute per param per block instead of one per message.
    fn advance_param_smoothing(&mut self) {
        for w in 0..Self::MASK_WORDS {
            // Walk a copy of the word: `set_param_inner` clears and re-arms
            // bits in the live mask, and each pending param steps exactly
            // once.
            let mut pending = self.param_pending.words[w];
            while pending != 0 {
                let i = w * 32 + pending.trailing_zeros() as usize;
                pending &= pending - 1;
                let target = self.param_targets[i];
                let next = self.param_smooth[i] + (target - self.param_smooth[i]) * PARAM_SMOOTH_K;
                if (target - next).abs() < PARAM_SMOOTH_EPS {
                    // Converged: snap; the inner set clears the pending bit.
                    self.set_param_inner(i, target);
                } else {
                    // Still moving: the inner set clears the pending bit as
                    // part of syncing the smoother, so re-arm it.
                    self.set_param_inner(i, next);
                    self.param_pending.set(i);
                }
            }
        }
    }

    /// Apply the strip-filter and AHD-envelope param slots to the strip.
    ///
    /// Cutoff is log-mapped 20 Hz..20 kHz; resonance is linear 0.5..20 Q;
    /// attack 0..1 s, hold 0..2 s, decay 0.01..3 s. Only the coefficients that
    /// changed are recomputed, via [`set_strip`]'s dirty-flag logic.
    fn apply_strip_params(&mut self) {
        let m = self.base_params;
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

    /// Apply the LFO param slots to the two LFOs.
    ///
    /// Rate is split: 0..0.5 = slow range (0.1..10 Hz), 0.5..1 = fast range
    /// (1..100 Hz), log-mapped within each half. Depth is 0..1. Destination is
    /// quantised over [`ModDest`] via [`ModDest::from_param`].
    fn apply_lfo_params(&mut self) {
        let m = self.base_params;
        apply_one_lfo_param(
            &mut self.mod_state.lfos[0],
            m[SLOT_LFO1_RATE],
            m[SLOT_LFO1_DEPTH],
            m[SLOT_LFO1_DEST],
        );
        apply_one_lfo_param(
            &mut self.mod_state.lfos[1],
            m[SLOT_LFO2_RATE],
            m[SLOT_LFO2_DEPTH],
            m[SLOT_LFO2_DEST],
        );
    }

    /// Replace all base params in one call.
    pub fn set_params(&mut self, all: &[f32; N]) {
        self.base_params = *all;
        // Sends are track-routed from the param array (see `set_param`).
        self.strip.send_delay = all[SLOT_SEND_DELAY];
        self.strip.send_reverb = all[SLOT_SEND_REVERB];
        self.eff_send_delay = all[SLOT_SEND_DELAY];
        self.eff_send_reverb = all[SLOT_SEND_REVERB];
        // Pan is track-routed too (see `set_param`): the param owns the value,
        // the strip pan is the derived -1..+1 form and the gains feed `tick`.
        self.strip.pan = all[SLOT_PAN] * 2.0 - 1.0;
        let (l, r) = pan_law(self.strip.pan);
        self.pan_l = l;
        self.pan_r = r;
        // Output routing is track-routed too (see `set_param`): quantise the
        // param value over the 4 `OutPair` variants and store the quantised
        // form back so round-trip reads return the canonical centre.
        let i = (all[SLOT_OUT] * (OutPair::COUNT - 1) as f32 + 0.5) as usize;
        self.strip.out = OutPair::ALL[i];
        self.base_params[SLOT_OUT] = i as f32 / (OutPair::COUNT - 1) as f32;
        // Strip filter / AHD env / LFO params are track-routed too (see
        // `set_param`): apply them to the strip and mod state.
        self.apply_strip_params();
        self.apply_lfo_params();
        if !self.mod_state.has_active_mod() {
            self.slot.set_params(&self.base_params);
        }
        // Bulk param replace resets the CC smoother (same rationale as
        // `load_machine`).
        self.sync_param_smoothing();
    }

    /// Transpose the slot by `semis` semitones relative to its param pitch.
    /// Absolute, not incremental — the whole voice (sweep, FM ratio, detune)
    /// moves. Survives later param recomputes via each slot's internal
    /// frequency scaling. Slots without pitch no-op. Control rate.
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
        // Mirror the aux levels into the send params so a strip edit keeps the
        // param view consistent. The send params are the
        // routing authority; the strip fields are derived storage.
        self.base_params[SLOT_SEND_DELAY] = params.send_delay;
        self.base_params[SLOT_SEND_REVERB] = params.send_reverb;
        // Mirror the strip pan into the PAN param slot so a strip edit keeps
        // the param view consistent, like the sends.
        self.base_params[SLOT_PAN] = params.pan * 0.5 + 0.5;
        // Mirror routing into the OUT param slot. Same shape as
        // `set_param(SLOT_OUT)`: the param view shows the canonical centre of
        // the variant, so MIDI feedback reports the value the user expects.
        self.base_params[SLOT_OUT] = params.out.index() as f32 / (OutPair::COUNT - 1) as f32;
        // Mirror the strip filter / AHD env params into their param slots so a
        // strip edit keeps the param view consistent. Inverse of
        // `apply_strip_params`.
        self.base_params[SLOT_STRIP_CUT] =
            libm::logf(params.f_cutoff_hz / 20.0) / libm::logf(1000.0);
        self.base_params[SLOT_STRIP_RESO] = (params.f_reso_q - 0.5) / 19.5;
        self.base_params[SLOT_STRIP_ATK] = params.amp_attack_s;
        self.base_params[SLOT_STRIP_HOLD] = params.amp_hold_s / 10.0;
        self.base_params[SLOT_STRIP_DEC] = (params.amp_decay_s - 0.01) / 9.99;
        if !self.mod_state.has_active_mod() {
            self.eff_drive = params.drive;
            self.eff_level = params.level;
            self.eff_send_delay = params.send_delay;
            self.eff_send_reverb = params.send_reverb;
        }
        // The strip edit just rewrote the send/pan param mirrors; resync the
        // CC smoother so a pending ramp doesn't yank them back.
        self.sync_param_smoothing();
    }

    /// Begin a hit at `velocity` (0..=1.0). Fires LFO triggers and stores
    /// velocity for the control pass.
    pub fn trigger(&mut self, velocity: f32) {
        let retrigger = self.slot.is_active();
        self.slot.trigger(velocity);
        self.amp_env.trigger(velocity);
        self.mod_state.trigger(velocity);
        // A hit landing on an already-sounding voice would step the output
        // from the old tail to the new attack; arm a short crossfade from the
        // current output instead. A fresh hit from silence cancels any stale
        // window and any in-progress choke fade.
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

    /// Close the gate on this track — the note-off path.
    ///
    /// Forwards to [`Slot::release`], which is a no-op for one-shot voices and
    /// ends the note for gated ones. Deliberately does *not* touch the strip's
    /// amp envelope, `choke_fade_left`, or the de-click window: the strip
    /// amp is a fixed A-H-D gesture that the voice sits inside, not a second
    /// gate, and layering a release on top of it would double-shape every
    /// note. Releasing also does not touch `declick_left`, because a release
    /// is not a retrigger — there is no new attack to crossfade into, and
    /// crossfading would re-introduce the very level the voice is releasing
    /// from.
    ///
    /// A no-op when the track is already silent.
    pub fn release(&mut self) {
        if self.is_active() {
            self.slot.release();
        }
    }

    /// Choke this voice: fade its output to zero over [`CHOKE_SAMPLES`]
    /// rather than cutting it. The slot keeps ringing under the fade and is
    /// hard-reset once the fade completes. A no-op when already silent or
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
    /// params + strip params, and recomputes coefficients. Called from the
    /// engine before the sample loop. If no modulation is active and no CC
    /// params are mid-ramp, this is a single-branch early return.
    pub fn control(&mut self) {
        // 0. CC param smoothing first, before the mod-state borrow: slew
        //    pending params toward their targets and recompute coefficients
        //    as they move, so a CC edit is audible even with no modulation.
        if self.param_pending.any(Self::MASK_WORDS) {
            self.advance_param_smoothing();
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
        let mut eff_params = self.base_params;
        let mut eff_cutoff = self.strip.f_cutoff_hz;
        let mut eff_reso = self.strip.f_reso_q;
        let mut eff_drive = self.strip.drive;
        let mut eff_pan = self.strip.pan;
        let mut eff_level = self.strip.level;
        let mut eff_amp_decay = self.strip.amp_decay_s;
        let mut eff_send_delay = self.strip.send_delay;
        let mut eff_send_reverb = self.strip.send_reverb;
        let mut param_dirty = false;
        let mut strip_dirty = false;

        // 3. Apply LFO modulations.
        for lfo in [lfo0, lfo1] {
            if !lfo.is_contributing() {
                continue;
            }
            let v = lfo.value() * lfo.depth();
            match lfo.dest() {
                ModDest::Param(i) if i < N => {
                    eff_params[i] = (eff_params[i] + v).clamp(0.0, 1.0);
                    param_dirty = true;
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
                ModDest::Param(i) if i < N => {
                    eff_params[i] = (eff_params[i] + v).clamp(0.0, 1.0);
                    param_dirty = true;
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
        if param_dirty {
            self.slot.set_params(&eff_params);
            // Sends are track-routed from the param array, so modulating
            // SEND.DLY / SEND.RVB moves the aux levels too.
            eff_send_delay = eff_params[SLOT_SEND_DELAY];
            eff_send_reverb = eff_params[SLOT_SEND_REVERB];
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
        let mixed = if self.strip_bypass {
            machine_sample * self.eff_level
        } else {
            let amp = self.amp_env.tick();
            let stage = machine_sample * amp;
            let driven = fast::soft_clip(stage * self.eff_drive);
            let filtered = self.filter.tick(driven);
            filtered * self.eff_level
        };
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

    /// Render one contiguous segment into the output and send buses.
    ///
    /// Reads source samples from [`Self::source_segment`], which the engine
    /// fills via [`Slot::tick`](crate::slot::Slot::tick) and then optionally
    /// processes through [`Slot::process_audio_strip`]. `start` is the offset
    /// into the block. The strip (amp env, drive, filter, level, pan, sends,
    /// choke, de-click) is applied sample-wise. This is the segment-based
    /// equivalent of calling [`tick`](Self::tick) `n` times.
    pub fn process_segment(
        &mut self,
        start: usize,
        n: usize,
        master_l: &mut [f32; BLOCK],
        master_r: &mut [f32; BLOCK],
        aux: &mut [[f32; BLOCK]; 6],
        send_dl: &mut [f32; BLOCK],
        send_dr: &mut [f32; BLOCK],
        send_rl: &mut [f32; BLOCK],
        send_rr: &mut [f32; BLOCK],
    ) {
        debug_assert!(start + n <= BLOCK);

        let sd = self.eff_send_delay;
        let sr = self.eff_send_reverb;
        let out = self.strip.out;

        for i in 0..n {
            let machine_sample = self.source_segment[i];
            let mixed = if self.strip_bypass {
                machine_sample * self.eff_level
            } else {
                let amp = self.amp_env.tick();
                let stage = machine_sample * amp;
                let driven = fast::soft_clip(stage * self.eff_drive);
                let filtered = self.filter.tick(driven);
                filtered * self.eff_level
            };
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

            let idx = start + i;
            send_dl[idx] += l * sd;
            send_dr[idx] += r * sd;
            send_rl[idx] += l * sr;
            send_rr[idx] += r * sr;
            match out {
                OutPair::Master => {
                    master_l[idx] += l;
                    master_r[idx] += r;
                }
                OutPair::Aux1 => {
                    aux[0][idx] += l;
                    aux[1][idx] += r;
                }
                OutPair::Aux2 => {
                    aux[2][idx] += l;
                    aux[3][idx] += r;
                }
                OutPair::Aux3 => {
                    aux[4][idx] += l;
                    aux[5][idx] += r;
                }
            }
        }
    }
}

/// One-pole coefficient for CC param smoothing, block rate.
///
/// Time constant 7.5 ms (mid-range of the 5–10 ms design guidance), block
/// rate 1500 Hz (SAMPLE_RATE / BLOCK). k = 1 − exp(−1 / (0.0075·1500)).
const PARAM_SMOOTH_K: f32 = 0.0851;

/// Convergence threshold for CC param smoothing. A pending param snaps to
/// its target once the residual is below this; ~0.01% of full scale is
/// comfortably inaudible.
const PARAM_SMOOTH_EPS: f32 = 1.0e-4;

/// Apply one LFO's param slots: rate (slow/fast split), depth, destination.
fn apply_one_lfo_param(lfo: &mut Lfo, rate_v: f32, depth_v: f32, dest_v: f32) {
    let (mode, norm) = if rate_v < 0.5 {
        (crate::dsp::LfoRateMode::Slow, rate_v * 2.0)
    } else {
        (crate::dsp::LfoRateMode::Fast, (rate_v - 0.5) * 2.0)
    };
    lfo.set_rate(norm, mode);
    lfo.set_params(
        lfo.speed_hz(),
        lfo.wave(),
        lfo.mode(),
        depth_v,
        ModDest::from_param(dest_v),
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
pub fn pan_law(pan: f32) -> (f32, f32) {
    let p = pan.clamp(-1.0, 1.0);
    let t = (p + 1.0) * 0.5; // 0 = hard L, 1 = hard R
    let turns = t * 0.25; // angle = t·π/2 in turns
    let l = fast::sin_turns(turns + 0.25); // cos
    let r = fast::sin_turns(turns); // sin
    (l, r)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn param_mask_tracks_bits_across_words() {
        let mut m = ParamMask::default();
        assert!(!m.any(PARAM_MASK_WORDS));
        for idx in [0, 31, 32, 63, 255] {
            m.set(idx);
        }
        assert_eq!(m.words[0], 1u32 | 1u32 << 31);
        assert_eq!(m.words[1], 1u32 | 1u32 << 31);
        assert_eq!(m.words[7], 1u32 << 31);
        // A 32-param device scans one word, so only word 0 can hold its bits.
        assert!(m.any(1));
        m.clear(0);
        m.clear(31);
        assert!(!m.any(1));
        assert!(m.any(2));
        for idx in [32, 63, 255] {
            m.clear(idx);
        }
        assert!(!m.any(PARAM_MASK_WORDS));
        assert_eq!(m, ParamMask::EMPTY);
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
}
