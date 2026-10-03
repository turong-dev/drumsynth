//! Mutable-Instruments-based drum device.
//!
//! Hosts Plaits macro-oscillator engines (and eventually Peaks drum models) as
//! per-track voices inside the generic [`device_core::Engine`] framework.
//! The C++ Plaits voice is block-rate, while `device_core::Slot` is
//! per-sample, so [`MiSlot`] buffers one rendered block and yields samples one
//! at a time.
//!
//! # The gate
//!
//! [`MiSlot`] holds the gate high from note-on to note-off, and
//! [`Slot::release`] closes it. Plaits reads the gate as a *level* as well as
//! an edge, so the pulse this replaced starved the three `SixOp` engines into
//! digital silence; holding the level fixes them and gives every
//! level-reading engine a note to sustain, while the edge still fires once.
//! See `DESIGN.md` for the measurements and `PLAN.md` for what the gate still
//! does not do.
//!
//! Release is the engine's own, not a cut: dropping the gate starts Plaits'
//! LPG decay and Peaks' gate-processor release. A gate also ends when its
//! voice does, so a one-shot drum model triggered from a grid that never sends
//! a note-off still comes to rest.

#![no_std]
#![deny(unsafe_code)]
#![warn(missing_docs)]

#[cfg(test)]
extern crate std;

pub use device_core::dsp;
pub use device_core::engine::{DeviceEngine, Engine};
pub use device_core::slot::{DeviceModel, Slot, SlotId};
pub use device_core::strip::StripParams;
pub use device_core::track::{pan_law, ModState, VelMod, CHOKE_SAMPLES};
pub use device_core::{
    EngineEvent, OutPair, TimedEvent, TimedQueue, BLOCK, DENORMAL_FLOOR, INV_SAMPLE_RATE,
    MAX_TIMED_EVENTS, SAMPLE_RATE,
};

pub use device_core::macros::{
    SLOT_FILT_0, SLOT_FILT_1, SLOT_STRIP_ATK, SLOT_STRIP_CUT, SLOT_STRIP_DEC, SLOT_STRIP_HOLD,
    SLOT_STRIP_RESO,
};

use device_core::dsp::svf::stability_ceiling_hz;
use device_core::dsp::Svf;
use device_core::macros::{
    macro_index, mi, MacroInfo, BANK_FILT, BANK_MOD, BANK_TRACK, MACH_INFO, NUM_MACROS, OUT_INFO,
    PAN_INFO, SEND_DLY_INFO, SEND_RVB_INFO, SLOT_LEVEL, SLOT_MACHINE, SLOT_MACH_0, SLOT_MACH_1,
    SLOT_MACH_2, SLOT_MACH_3, SLOT_MACH_4, SLOT_MACH_5, SLOT_MACH_6, SLOT_MACH_7, SLOT_OUT,
    SLOT_PAN, SLOT_SEND_DELAY, SLOT_SEND_REVERB,
};
use mi_dsp::peaks::{PeaksModel, PeaksVoice};
use mi_dsp::plaits::{MiPlaitsModulations, MiPlaitsPatch, PlaitsVoice};
use mi_dsp::stages::{
    Stages as ModStages, GATE_FALLING, GATE_HIGH, GATE_LOW, GATE_RISING, SEGMENT_ALT,
};
use mi_dsp::warps::{Carrier, OscShape, Warps, WarpsOscillator, MAX_BLOCK as WARPS_MAX_BLOCK};

/// What feeds Warps' modulator input. See [`SLOT_WARPS_MOD_SRC`].
#[derive(Clone, Copy, PartialEq, Eq, Debug, Default)]
pub enum ModSource {
    /// The voice's own output, in both of Warps' inputs.
    #[default]
    Self_,
    /// Plaits' aux output. Falls back to `Self_` on a Peaks voice.
    Aux,
    /// The strip's own oscillator, shape from `WARP.OSC`.
    Oscillator,
}

/// Seed the noise generator shared by every Plaits engine on this device.
///
/// Re-exported from `mi-dsp` because callers that want a reproducible render
/// (the host renderer, tests) already depend on this crate and should not have
/// to reach past it. See [`mi_dsp::seed_random`] for why one call is not
/// enough to make *concurrent* renders reproducible.
pub use mi_dsp::{seed_random, DEFAULT_RANDOM_SEED};

/// The MI processing stages, re-exported so callers (firmware benches, the
/// host renderer) can construct one without depending on `mi-dsp` directly.
pub use mi_dsp::spike_stages;
/// Backward-compatible alias: firmware benches reach these as `stages::Resonator`.
pub use mi_dsp::spike_stages as stages;

/// How many tracks the engine owns.
pub const TRACKS: usize = 6;

/// Render block size used internally by [`MiSlot`]. Matches Plaits
/// `kMaxBlockSize` so every render call is one native block.
const VOICE_BLOCK: usize = 24;

/// Consider a Plaits block silent when every sample is below this magnitude.
/// Plaits reaches true zero, so this can be tight.
const SILENCE_THRESHOLD: f32 = 1.0e-6;

/// The same gate for a Peaks block.
///
/// Peaks is 16-bit fixed point with a saturating `CLIP` at the end of every
/// model, so its tail settles onto a limit cycle of roughly 60-100 int16
/// (about -70 dBFS) instead of reaching zero. Measured, not assumed — see
/// docs/peaks-vendoring.md. -50 dBFS clears that floor and is inaudible.
const PEAKS_SILENCE_THRESHOLD: f32 = mi_dsp::peaks::SILENCE_F32;
/// Number of consecutive silent blocks before the slot declares itself idle.
const SILENCE_BLOCKS: u8 = 4;

/// Watchdog on a held gate, in seconds.
///
/// A note-off that never arrives leaves the gate high, and a high gate is what
/// makes a track render at full cost — `is_active` is the engine's per-track
/// early-out, so a stuck key would keep one voice's full DSP in the cycle
/// budget indefinitely. Ten seconds is far longer than a held note and short
/// enough to bound the damage.
///
/// This is a backstop, not the note length. Raise it if you hold a key longer
/// than ten seconds and hear the voice drop out.
pub const GATED_MAX_HOLD_S: f32 = 10.0;

/// [`GATED_MAX_HOLD_S`] in rendered voice blocks.
///
/// The watchdog is decremented once per `render_if_needed`, i.e. once per
/// 24-sample `VOICE_BLOCK`, so it has to be counted in blocks. Counting in
/// samples would make the gate last `VOICE_BLOCK` times too long — ten seconds
/// of watchdog would be four minutes of held gate.
/// One-pole release coefficient for the oscillator's gate, ~30 ms at 48 kHz.
///
/// Attack is instant; only the release is smoothed. A fast release would
/// chatter on the voice's own zero crossings and amplitude-modulate the
/// modulator at the voice's pitch.
const OSC_ENV_RELEASE: f32 = 1.0 / (0.030 * SAMPLE_RATE);

const GATED_MAX_HOLD_BLOCKS: u32 = (GATED_MAX_HOLD_S * SAMPLE_RATE) as u32 / VOICE_BLOCK as u32;

/// A named Plaits engine model.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum MiMachineId {
    /// Virtual-analog VCF oscillator.
    VirtualAnalogVcf,
    /// Phase-distortion oscillator.
    PhaseDistortion,
    /// Six-operator FM voice 1.
    SixOp1,
    /// Six-operator FM voice 2.
    SixOp2,
    /// Six-operator FM voice 3.
    SixOp3,
    /// Wave-terrain oscillator.
    WaveTerrain,
    /// String-machine ensemble.
    StringMachine,
    /// Chiptune / 8-bit oscillator.
    Chiptune,
    /// Virtual-analog oscillator.
    VirtualAnalog,
    /// Waveshaping oscillator.
    Waveshaping,
    /// FM oscillator.
    Fm,
    /// Granular texture.
    Grain,
    /// Additive oscillator.
    Additive,
    /// Wavetable oscillator.
    Wavetable,
    /// Chord / supersaw.
    Chord,
    /// Speech synthesis.
    Speech,
    /// Swarm oscillator.
    Swarm,
    /// Noise oscillator.
    Noise,
    /// Particle noise texture.
    Particle,
    /// Physical string model.
    String,
    /// Modal / membrane model.
    Modal,
    /// Plaits bass drum.
    BassDrum,
    /// Plaits snare drum.
    SnareDrum,
    /// Plaits hi-hat.
    HiHat,
    /// Peaks bass drum. Phase 14.4.
    PeaksBassDrum,
    /// Peaks snare drum. Phase 14.4.
    PeaksSnareDrum,
    /// Peaks high hat. Phase 14.4.
    PeaksHighHat,
    /// Peaks FM drum. Phase 14.4.
    PeaksFmDrum,
}

impl MiMachineId {
    /// Number of machines currently catalogued.
    pub const COUNT: usize = 28;

    /// Number of Plaits engines, i.e. the machine indices below
    /// [`PLAITS_COUNT`](Self::PLAITS_COUNT). Peaks models are appended after
    /// them so no existing catalogue index moves.
    pub const PLAITS_COUNT: usize = 24;

    /// All machines, in catalogue order. This order is stable ABI: the
    /// `SLOT_MACHINE` macro quantises over it.
    pub const ALL: [Self; Self::COUNT] = [
        Self::VirtualAnalogVcf,
        Self::PhaseDistortion,
        Self::SixOp1,
        Self::SixOp2,
        Self::SixOp3,
        Self::WaveTerrain,
        Self::StringMachine,
        Self::Chiptune,
        Self::VirtualAnalog,
        Self::Waveshaping,
        Self::Fm,
        Self::Grain,
        Self::Additive,
        Self::Wavetable,
        Self::Chord,
        Self::Speech,
        Self::Swarm,
        Self::Noise,
        Self::Particle,
        Self::String,
        Self::Modal,
        Self::BassDrum,
        Self::SnareDrum,
        Self::HiHat,
        Self::PeaksBassDrum,
        Self::PeaksSnareDrum,
        Self::PeaksHighHat,
        Self::PeaksFmDrum,
    ];

    /// Catalogue index of this machine.
    pub const fn index(self) -> usize {
        self as usize
    }

    /// Short canonical name used in CLI args and logs.
    pub fn name(self) -> &'static str {
        match self {
            Self::VirtualAnalogVcf => "mi-va-vcf",
            Self::PhaseDistortion => "mi-phase",
            Self::SixOp1 => "mi-sixop-1",
            Self::SixOp2 => "mi-sixop-2",
            Self::SixOp3 => "mi-sixop-3",
            Self::WaveTerrain => "mi-terrain",
            Self::StringMachine => "mi-string-mach",
            Self::Chiptune => "mi-chiptune",
            Self::VirtualAnalog => "mi-va",
            Self::Waveshaping => "mi-waveshape",
            Self::Fm => "mi-fm",
            Self::Grain => "mi-grain",
            Self::Additive => "mi-additive",
            Self::Wavetable => "mi-wavetable",
            Self::Chord => "mi-chord",
            Self::Speech => "mi-speech",
            Self::Swarm => "mi-swarm",
            Self::Noise => "mi-noise",
            Self::Particle => "mi-particle",
            Self::String => "mi-string",
            Self::Modal => "mi-modal",
            Self::BassDrum => "mi-bd",
            Self::SnareDrum => "mi-sd",
            Self::HiHat => "mi-hh",
            Self::PeaksBassDrum => "pk-bd",
            Self::PeaksSnareDrum => "pk-sd",
            Self::PeaksHighHat => "pk-hh",
            Self::PeaksFmDrum => "pk-fm",
        }
    }

    /// Whether this machine is a Peaks drum rather than a Plaits engine.
    pub const fn is_peaks(self) -> bool {
        self.index() >= Self::PLAITS_COUNT
    }

    /// The Peaks model this machine maps to. `None` for Plaits engines.
    pub const fn peaks_model(self) -> Option<PeaksModel> {
        match self {
            Self::PeaksBassDrum => Some(PeaksModel::BassDrum),
            Self::PeaksSnareDrum => Some(PeaksModel::SnareDrum),
            Self::PeaksHighHat => Some(PeaksModel::HighHat),
            Self::PeaksFmDrum => Some(PeaksModel::FmDrum),
            _ => None,
        }
    }

    /// The Peaks machine for a model, the inverse of [`Self::peaks_model`].
    pub const fn from_peaks_model(model: PeaksModel) -> Self {
        match model {
            PeaksModel::BassDrum => Self::PeaksBassDrum,
            PeaksModel::SnareDrum => Self::PeaksSnareDrum,
            PeaksModel::HighHat => Self::PeaksHighHat,
            PeaksModel::FmDrum => Self::PeaksFmDrum,
        }
    }

    /// The Plaits engines, as a sub-catalogue.
    pub const PLAITS: [Self; Self::PLAITS_COUNT] = [
        Self::VirtualAnalogVcf,
        Self::PhaseDistortion,
        Self::SixOp1,
        Self::SixOp2,
        Self::SixOp3,
        Self::WaveTerrain,
        Self::StringMachine,
        Self::Chiptune,
        Self::VirtualAnalog,
        Self::Waveshaping,
        Self::Fm,
        Self::Grain,
        Self::Additive,
        Self::Wavetable,
        Self::Chord,
        Self::Speech,
        Self::Swarm,
        Self::Noise,
        Self::Particle,
        Self::String,
        Self::Modal,
        Self::BassDrum,
        Self::SnareDrum,
        Self::HiHat,
    ];

    /// The Peaks drums, as a sub-catalogue.
    pub const PEAKS: [Self; 4] = [
        Self::PeaksBassDrum,
        Self::PeaksSnareDrum,
        Self::PeaksHighHat,
        Self::PeaksFmDrum,
    ];

    /// Human-readable label for display.
    pub fn label(self) -> &'static str {
        match self {
            Self::VirtualAnalogVcf => "MI VA-VCF",
            Self::PhaseDistortion => "MI Phase",
            Self::SixOp1 => "MI SixOp 1",
            Self::SixOp2 => "MI SixOp 2",
            Self::SixOp3 => "MI SixOp 3",
            Self::WaveTerrain => "MI Terrain",
            Self::StringMachine => "MI StringMach",
            Self::Chiptune => "MI Chiptune",
            Self::VirtualAnalog => "MI VA",
            Self::Waveshaping => "MI Waveshape",
            Self::Fm => "MI FM",
            Self::Grain => "MI Grain",
            Self::Additive => "MI Additive",
            Self::Wavetable => "MI Wavetable",
            Self::Chord => "MI Chord",
            Self::Speech => "MI Speech",
            Self::Swarm => "MI Swarm",
            Self::Noise => "MI Noise",
            Self::Particle => "MI Particle",
            Self::String => "MI String",
            Self::Modal => "MI Modal",
            Self::BassDrum => "MI BassDrum",
            Self::SnareDrum => "MI Snare",
            Self::HiHat => "MI HiHat",
            Self::PeaksBassDrum => "PK BassDrum",
            Self::PeaksSnareDrum => "PK Snare",
            Self::PeaksHighHat => "PK HiHat",
            Self::PeaksFmDrum => "PK FmDrum",
        }
    }

    /// The Plaits engine index this machine maps to.
    const fn plaits_engine(self) -> i32 {
        self.index() as i32
    }

    /// Machine-specific default macro values.
    /// Whether this machine's aux output carries a *different* signal from
    /// its main output.
    ///
    /// Not the same question as whether it carries signal at all. Measured
    /// through the strip, 21 of the 24 Plaits engines render an aux that
    /// differs from their main output; the three `SixOp` engines write the
    /// same samples to both, so selecting aux on one of them is
    /// indistinguishable from self-modulation. A Peaks voice has no aux at
    /// all.
    ///
    /// This is what decides a machine's `WARP.IN` default: aux where there is
    /// a real second signal, the strip's oscillator where there is not.
    /// `every_machine_has_a_real_modulator_by_default` is the guard, and it
    /// is what caught this — the first version of that default was a single
    /// shared value and it left the `SixOp` engines cross-modulating against
    /// themselves.
    pub fn has_distinct_aux(self) -> bool {
        !self.is_peaks() && !matches!(self, Self::SixOp1 | Self::SixOp2 | Self::SixOp3)
    }

    /// Machine-specific default macro values.
    pub fn default_macros(self) -> [f32; NUM_MACROS] {
        // Default TUNE: drums sit lower, melodic models near middle C.
        let tune = match self {
            Self::BassDrum | Self::SnareDrum | Self::HiHat => 0.30,
            Self::Modal | Self::String | Self::Particle => 0.40,
            _ => 0.45,
        };
        let decay = match self {
            Self::BassDrum | Self::SnareDrum | Self::HiHat | Self::Modal | Self::String => 0.45,
            _ => 0.50,
        };
        let mut m = [0.0f32; NUM_MACROS];
        if self.is_peaks() {
            // A Peaks model's MACH 1..4 are its own parameters, not Plaits
            // patch fields, and the TUNE slot is unused because pitch comes
            // from the MIDI note. Defaults are per model:
            //   bass  [punch, tone, decay]
            //   snare [tone, snap, decay]
            //   fm    [fm amount, decay, noise]
            m[SLOT_MACH_1] = 0.30; // punch / tone / FM amount
            m[SLOT_MACH_2] = 0.50; // tone / snap / decay
            m[SLOT_MACH_3] = 0.30; // decay / decay / noise
            m[SLOT_MACH_4] = 0.0;
        } else {
            m[SLOT_MACH_0] = tune;
            m[SLOT_MACH_1] = 0.5;
            m[SLOT_MACH_2] = 0.5;
            m[SLOT_MACH_3] = 0.5;
            m[SLOT_MACH_4] = 0.0;
            m[SLOT_MACH_5] = 0.0;
            m[SLOT_MACH_6] = 0.0;
        }
        m[SLOT_MACH_7] = decay;

        // Track-routed defaults.
        m[SLOT_MACHINE] = self.index() as f32 / ((Self::COUNT.saturating_sub(1).max(1)) as f32);
        m[SLOT_OUT] = 0.0;
        m[SLOT_PAN] = 0.5;
        m[SLOT_LEVEL] = 0.85;
        m[SLOT_SEND_DELAY] = 0.0;
        m[SLOT_SEND_REVERB] = 0.0;

        // Phase 14 fixed strip defaults.
        m[SLOT_WARPS_ALGO] = 0.0;
        m[SLOT_WARPS_TIMBRE] = 0.5;
        m[SLOT_WARPS_DRIVE] = 0.2; // light colour; 0.0 is a true bypass

        m[SLOT_RIPPLES_CUTOFF] = 1.0; // fully open — see RIPPLES_CUTOFF_INFO
        m[SLOT_RIPPLES_RESONANCE] = 0.01; // gentle Q
        m[SLOT_RIPPLES_FM] = 0.0;
        // Warps as a colour rather than a transform: a third wet by default,
        // its modulator fed from Plaits' aux, taking the main output.
        m[SLOT_WARPS_MIX] = 0.35;
        // Per machine, because a single shared default leaves some voices
        // cross-modulating against themselves — the degenerate case this
        // whole arrangement exists to avoid. Aux where the machine has a
        // distinct one, the strip's oscillator where it does not. See
        // `has_distinct_aux`.
        m[SLOT_WARPS_MOD_SRC] = if self.has_distinct_aux() { 0.5 } else { 1.0 };
        m[SLOT_WARPS_OSC_SHAPE] = 0.0; // sine
        m[SLOT_WARPS_OUT_TAP] = 0.0;

        // Modulation defaults. The per-target depths are 0 so the sound is
        // unchanged until a macro is moved; the LFO master is 1 so turning up
        // a per-target depth alone is enough to hear the route.
        m[SLOT_LFO_RATE] = 0.5;
        m[SLOT_LFO_DEPTH] = 1.0;
        m[SLOT_AD_ATTACK] = 0.05;
        m[SLOT_AD_DECAY] = 0.3;
        m[SLOT_LFO_FILTER_DEPTH] = 0.0;
        m[SLOT_LFO_WARPS_DEPTH] = 0.0;
        m[SLOT_AD_FILTER_DEPTH] = 0.0;
        m[SLOT_AD_WARPS_DEPTH] = 0.0;

        m
    }

    /// Per-macro metadata. All MI machines share the same layout.
    pub fn macros(self) -> [MacroInfo; NUM_MACROS] {
        let _ = self;
        MACROS
    }

    /// Look up a macro's metadata by name. Case-insensitive, ASCII only.
    pub fn macro_by_name(self, name: &str) -> Option<(usize, MacroInfo)> {
        for (i, m) in self.macros().into_iter().enumerate() {
            if m.name.eq_ignore_ascii_case(name.trim()) {
                return Some((i, m));
            }
        }
        None
    }
}

impl SlotId<NUM_MACROS> for MiMachineId {
    fn index(self) -> usize {
        self.index()
    }
    fn from_index(i: usize) -> Option<Self> {
        Self::ALL.get(i).copied()
    }
    fn count() -> usize {
        Self::COUNT
    }
    fn default_macros(self) -> [f32; NUM_MACROS] {
        self.default_macros()
    }
}

impl DeviceModel<NUM_MACROS> for MiMachineId {
    fn macro_info(self) -> [MacroInfo; NUM_MACROS] {
        self.macros()
    }
    fn label(self) -> &'static str {
        self.label()
    }
}

// Macro slot aliases for the Phase 14 fixed strip.
const SLOT_WARPS_ALGO: usize = SLOT_FILT_0;
const SLOT_WARPS_TIMBRE: usize = SLOT_FILT_1;
/// FILT 6: shape of the strip's own oscillator, when `WARP.IN` selects it as
/// Warps' modulator. Sine / triangle / saw / pulse / noise, pitched from the
/// voice's note.
///
/// This slot used to be `WARP.CAR`, which put the same oscillator on Warps'
/// *carrier* input. That was the wrong side: with an internal carrier the
/// voice is demoted to a modulation index, which is why all 28 machines
/// sounded like one sawtooth through it. On the modulator side the voice
/// stays the carrier and keeps its identity, and the strip no longer needs
/// Warps' `carrier_shape` at all — `Carrier::External` is now permanent.
pub const SLOT_WARPS_OSC_SHAPE: usize = macro_index(BANK_FILT, 6);
const SLOT_WARPS_DRIVE: usize = SLOT_STRIP_HOLD;
const SLOT_RIPPLES_CUTOFF: usize = SLOT_STRIP_CUT;
const SLOT_RIPPLES_RESONANCE: usize = SLOT_STRIP_RESO;
const SLOT_RIPPLES_FM: usize = SLOT_STRIP_ATK;

/// FILT 7: dry/wet between the voice and the Warps stage.
///
/// Warps has no mix control of its own — `Modulator::Process` is 100% wet,
/// and `channel_drive` only feeds the input saturators — so the strip
/// supplies one. 0 is the untouched voice, 1 is Warps alone. This is the
/// control that makes Warps usable as a colour; `WARP.DRV` is then purely how
/// hard the inputs are driven.
pub const SLOT_WARPS_MIX: usize = macro_index(BANK_FILT, 7);

/// TRACK 6: what feeds Warps' modulator input (input 2). Three positions.
///
/// - **0 — self.** The voice's own output in both inputs. Degenerate, and
///   kept only because it is what the strip did before: a comparator fed two
///   identical signals has nothing to compare, and `ALGORITHM_XFADE` reduces
///   to a gain, which makes `WARP.TIM` a trim rather than a timbre control.
/// - **1 — aux.** Plaits' second output, which the slot renders every block
///   and used to discard. Free, and the patch Warps is designed for. A Peaks
///   voice has no aux, so a Peaks track falls back to self.
/// - **2 — oscillator.** The strip's own [`WarpsOscillator`], shape from
///   `WARP.OSC`, pitched from the voice's note. The only source available to
///   every voice type, which matters because four of the six default tracks
///   are Peaks and would otherwise be stuck on self.
///
/// Stepped rather than a crossfade. The three are different *kinds* of
/// signal, not points on an axis, and blending a pitched oscillator into an
/// aux output is not a position anybody reaches for.
pub const SLOT_WARPS_MOD_SRC: usize = macro_index(BANK_TRACK, 6);

/// TRACK 7: which Warps output the strip takes.
///
/// 0 is **main**, the cross-modulation result. 1 is **aux**, which in the
/// cross-modulation path is the sum of the two *saturated inputs* rather than
/// a second cross-modulated voice (`modulator.cc:209`-`:222`) — so it is a
/// drive-only tap, Warps as a saturator with the ring modulation taken out.
/// Values in between crossfade.
pub const SLOT_WARPS_OUT_TAP: usize = macro_index(BANK_TRACK, 7);

/// MOD 0: Stages LFO period, 0..1 = slow..fast. Shared by LFO 1 and LFO 2.
pub const SLOT_LFO_RATE: usize = macro_index(BANK_MOD, 0);
/// MOD 1: master LFO depth scalar applied before the per-target depths.
pub const SLOT_LFO_DEPTH: usize = macro_index(BANK_MOD, 1);
/// MOD 2: Stages AD envelope attack time, 0..1 mapped to 1 ms..1 s.
pub const SLOT_AD_ATTACK: usize = macro_index(BANK_MOD, 2);
/// MOD 3: Stages AD envelope decay time, 0..1 mapped to 10 ms..5 s.
pub const SLOT_AD_DECAY: usize = macro_index(BANK_MOD, 3);
/// MOD 4: LFO depth into the Ripples cutoff, in octaves at full scale.
pub const SLOT_LFO_FILTER_DEPTH: usize = macro_index(BANK_MOD, 4);
/// MOD 5: LFO 2 depth into the Warps timbre parameter.
pub const SLOT_LFO_WARPS_DEPTH: usize = macro_index(BANK_MOD, 5);
/// MOD 6: AD env 1 depth into the Ripples cutoff, in octaves at full scale.
///
/// The AD envelopes are **unipolar**, so this route only ever *opens* the
/// filter. With the default `RIP.CUT` now fully open there is nowhere to go,
/// and the depth macro does nothing until `RIP.CUT` is pulled down. That is
/// the same failure mode as a dead modulation route and it is worth knowing
/// about before reaching for the knob: close the filter, then set the depth.
/// `AD.FIL` is not bipolar and is not meant to be — a filter envelope that
/// darkens the voice before it opens it is not what a drum machine wants.
pub const SLOT_AD_FILTER_DEPTH: usize = macro_index(BANK_MOD, 6);
/// MOD 7: AD env 2 depth into the Warps timbre parameter.
pub const SLOT_AD_WARPS_DEPTH: usize = macro_index(BANK_MOD, 7);

const WARPS_ALGO_INFO: MacroInfo = mi("WARP.ALG", "WAL", 0.0);
const WARPS_TIMBRE_INFO: MacroInfo = mi("WARP.TIM", "WTM", 0.5);
const WARPS_OSC_SHAPE_INFO: MacroInfo = mi("WARP.OSC", "WOS", 0.0);
/// Warps input drive, and the clean end of the strip.
///
/// The macro is **not** Warps' `drive` directly. Warps'
/// `SaturatingAmplifier` computes pre-gain as `0.5·drive` blended towards
/// `24·drive⁵` (`modulator.h:88`-`:91`), with post-gain normalising it back.
/// That makes the raw knob's travel:
///
/// | `WARP.DRV` | Warps `drive` | pre-gain | net gain | character |
/// |---|---|---|---|---|
/// | 0.00 | — | — | — | **bypass**: bit-transparent, no colour |
/// | 0.01 | 0.51 | 0.39 | 1.08 | unity, gentle |
/// | 0.25 | 0.63 | 1.05 | 1.19 | unity, cleanest saturation point |
/// | 0.50 | 0.75 | 3.37 | 3.37 | 3× overdriven |
/// | 0.75 | 0.88 | 9.72 | 9.72 | hard |
/// | 1.00 | 1.00 | 24.0 | 24.0 | destroyed |
///
/// Two things fall out of that table, and both are why the remap exists:
///
/// - **`drive = 0` is silence, not clean.** Passing the macro straight through
///   would make "no drive" mute the track. The bottom of the knob is a real
///   `Modulator::set_bypass`, which is what makes a clean section possible.
/// - **The top half of Warps' own knob covers 48× of gain.** The `drive⁵` term
///   is nearly linear below 0.5 and explodes above it, which is where the
///   "gets crazy past halfway" reputation comes from. Rescaling onto
///   `0.50..1.00` keeps that character but spends the whole knob on it, so
///   there is a usable clean-ish region below halfway and the destructive
///   region is something you choose rather than something you inherit.
///
/// `0.0` is a discrete detent onto the bypass and the rest of the knob is
/// strictly monotonic, so there is no dead zone between them.
const WARPS_DRIVE_INFO: MacroInfo = mi("WARP.DRV", "WDR", 0.2);

/// Map the `WARP.DRV` macro onto Warps' own `drive` parameter.
///
/// Returns `(bypassed, warps_drive)`.
///
/// `0.0` is a discrete detent onto the bypass. Anything above it is rescaled
/// `0.0..1.0` onto Warps' `0.50..1.00`, so the knob is strictly monotonic and
/// spends its whole travel on the part of Warps' curve that is usable.
/// Resolving this at control rate keeps the per-chunk strip path to two FFI
/// calls with no arithmetic. See [`WARPS_DRIVE_INFO`].
fn warps_drive_from_macro(macro_value: f32) -> (bool, f32) {
    let m = macro_value.clamp(0.0, 1.0);
    if m <= 0.0 {
        return (true, 0.0);
    }
    (false, 0.50 + 0.50 * m)
}
/// Ripples cutoff. 1.0 = fully open, matching `core`'s own `STRIP_CUT_INFO`.
///
/// This was 0.5, which the `20 * 1000^macro` mapping turns into a **632 Hz**
/// low-pass on every track. Measured on the rendered baseline, that put
/// everything above 1 kHz at least 45 dB down — a telephone band, not a drum
/// machine filter — and the comment here used to claim "~1 kHz", which was
/// wrong by 1.6x. `core` already defaults the equivalent strip slot wide open,
/// so the mi-drum strip is now neutral by default and the voice is heard
/// unfiltered.
///
/// The range is the filter's own: `20 Hz` at 0 to the Chamberlin SVF's
/// stability ceiling at 1.0 — ~6.4 kHz at the default `Q = 0.695`, and higher
/// as resonance comes down. Mapping to a nominal 20 kHz instead left the top
/// sixth of the knob flat and put modulation out of reach of the live region;
/// see `MiSlot::ripples_cutoff_from_macro` and `core::dsp::svf`.
const RIPPLES_CUTOFF_INFO: MacroInfo = mi("RIP.CUT", "RCT", 1.0);
const RIPPLES_RESONANCE_INFO: MacroInfo = mi("RIP.RES", "RRS", 0.5);
/// Ripples FM: audio-rate modulation of the filter cutoff by the signal
/// entering the filter.
///
/// This macro existed, had a label and a default, and was never read by
/// anything — `SLOT_RIPPLES_FM` appeared only in the table and in
/// `default_macros`. It is wired now. The source is the post-Warps signal, so
/// it is self-FM: the classic growling filter rather than a tremolo, and
/// distinct from the `LFO.FIL` and `AD.FIL` routes, which are control-rate.
/// Full scale is +/-2 octaves of cutoff swing per unit of input.
const RIPPLES_FM_INFO: MacroInfo = mi("RIP.FM", "RFM", 0.0);
const WARPS_MIX_INFO: MacroInfo = mi("WARP.MIX", "WMX", 0.35);
const WARPS_MOD_SRC_INFO: MacroInfo = mi("WARP.IN", "WIN", 0.5);
const WARPS_OUT_TAP_INFO: MacroInfo = mi("WARP.OUT", "WOU", 0.0);
const LFO_RATE_INFO: MacroInfo = mi("LFO.RATE", "LRT", 0.5);
const LFO_DEPTH_INFO: MacroInfo = mi("LFO.DEPTH", "LDPT", 1.0);
const AD_ATTACK_INFO: MacroInfo = mi("AD.ATK", "AAT", 0.05);
const AD_DECAY_INFO: MacroInfo = mi("AD.DEC", "ADEC", 0.3);
const LFO_FILTER_DEPTH_INFO: MacroInfo = mi("LFO.FIL", "LFI", 0.0);
const LFO_WARPS_DEPTH_INFO: MacroInfo = mi("LFO.WRP", "LWR", 0.0);
const AD_FILTER_DEPTH_INFO: MacroInfo = mi("AD.FIL", "AFI", 0.0);
const AD_WARPS_DEPTH_INFO: MacroInfo = mi("AD.WRP", "AWR", 0.0);

/// Shared macro metadata table for all MI machines.
static MACROS: [MacroInfo; NUM_MACROS] = [
    mi("TUNE", "TUN", 0.45),  // MACH 0
    mi("HARM", "HRM", 0.5),   // MACH 1
    mi("TIMBRE", "TIM", 0.5), // MACH 2
    mi("MORPH", "MRP", 0.5),  // MACH 3
    mi("FM.AMT", "FMA", 0.0), // MACH 4
    mi("TM.MOD", "TMM", 0.0), // MACH 5
    mi("MM.MOD", "MMM", 0.0), // MACH 6
    mi("DECAY", "DEC", 0.5),  // MACH 7
    WARPS_ALGO_INFO,          // FILT 0 — Warps algorithm
    WARPS_TIMBRE_INFO,        // FILT 1 — Warps timbre
    RIPPLES_CUTOFF_INFO,      // FILT 2 — Ripples cutoff
    RIPPLES_RESONANCE_INFO,   // FILT 3 — Ripples resonance
    RIPPLES_FM_INFO,          // FILT 4 — Ripples FM amount
    WARPS_DRIVE_INFO,         // FILT 5 — Warps input drive / VCA
    WARPS_OSC_SHAPE_INFO,     // FILT 6 — strip oscillator shape
    WARPS_MIX_INFO,           // FILT 7 — Warps dry/wet
    MACH_INFO,                // TRACK 0
    OUT_INFO,                 // TRACK 1
    PAN_INFO,                 // TRACK 2
    mi("LEVEL", "LVL", 0.85), // TRACK 3
    SEND_DLY_INFO,            // TRACK 4
    SEND_RVB_INFO,            // TRACK 5
    WARPS_MOD_SRC_INFO,       // TRACK 6 — Warps modulator source
    WARPS_OUT_TAP_INFO,       // TRACK 7 — Warps output tap
    LFO_RATE_INFO,            // MOD 0
    LFO_DEPTH_INFO,           // MOD 1
    AD_ATTACK_INFO,           // MOD 2
    AD_DECAY_INFO,            // MOD 3
    LFO_FILTER_DEPTH_INFO,    // MOD 4
    LFO_WARPS_DEPTH_INFO,     // MOD 5
    AD_FILTER_DEPTH_INFO,     // MOD 6
    AD_WARPS_DEPTH_INFO,      // MOD 7
];

/// A block-buffered Plaits voice implementing the per-sample `Slot` trait.
///
/// `PlaitsVoice` renders 24-sample blocks; this slot feeds the core engine's
/// per-sample `tick()` by buffering one block and stepping through it.
/// Which voice a track is currently sounding.
///
/// Both voices are held and `is_peaks` picks between them, rather than an enum
/// of the two. Two reasons, both practical:
///
/// - `PlaitsVoice` owns self-referential C++ state and must be placement-new'd
///   into its final address, which an enum gives no way to express for a
///   variant that does not exist yet. `PeaksVoice` has no such state — the
///   models are PODs with inline arrays and no internal pointers — so it is
///   freely movable and needs no `new_in_place`.
/// - The crate is `#![deny(unsafe_code)]`. Owning both as plain fields keeps
///   the single unavoidable `unsafe` confined to `new_in_place`, where the
///   Plaits voice is already constructed that way.
///
/// The cost is the unused partner voice: a Peaks track carries a dormant
/// 12,304 B PlaitsVoice it never sounds, and a Plaits track a 196 B PeaksVoice.
/// That is the price of not touching `core`, and it is small — 196 B per track
/// over the 12,304 B we already spend.
///
/// (196, not the 193 this used to say: `PeaksVoice`'s FFI storage was
/// alignment-1, which is what made it HardFault on the M7. See the note on
/// `Storage` in `mi-dsp/src/peaks.rs`.)
pub struct MiSlot {
    /// Sounded only on a Plaits track.
    plaits: PlaitsVoice,
    /// Sounded only on a Peaks track.
    peaks: PeaksVoice,
    /// Which of the two this track is currently using.
    is_peaks: bool,
    block_out: [f32; VOICE_BLOCK],
    block_aux: [f32; VOICE_BLOCK],
    block_pos: usize,
    patch: MiPlaitsPatch,
    modulations: MiPlaitsModulations,
    /// A note-on is waiting to be rendered. Consumed by the next
    /// [`Self::render_if_needed`], which is where the gate edges are placed.
    trigger_pending: bool,
    /// The gate is held high, from note-on until note-off.
    ///
    /// This is the whole difference between a pulsing gate and a real one.
    /// Plaits reads the trigger as a *level* as well as an edge
    /// (`voice.cc:143`-`:150` derives `TRIGGER_HIGH` and `TRIGGER_RISING_EDGE`
    /// from the same value), so holding it high gives an engine that reads the
    /// level a note to sustain, while the edge still fires exactly once. The
    /// previous one-block pulse starved the three `SixOp` engines, whose FM
    /// operator envelopes rest at zero and only rise while the gate is high —
    /// they emitted digital silence for the whole hit.
    gate_open: bool,
    /// A note-off is waiting to be rendered, so the next block can carry the
    /// falling edge Peaks' gate processor needs.
    gate_release_pending: bool,
    active: bool,
    silence_counter: u8,
    tune_macro: f32,
    retune_semitones: f32,
    // Phase 14 fixed-strip modules.
    warps: Warps,
    /// Shape of the strip's own oscillator, from `WARP.OSC`.
    warps_osc_shape: OscShape,
    /// The strip's modulator oscillator. Not Warps' internal carrier: this
    /// one feeds input 2, so the voice keeps input 1 and stays the subject.
    warps_osc: WarpsOscillator,
    /// Peak follower on the voice, used to gate the oscillator.
    ///
    /// The oscillator free-runs, and the cross-modulation algorithms do not
    /// all suppress it when the carrier goes quiet: a ring modulator
    /// multiplies, so a silent voice gives silence, but `ALGORITHM_XFADE`
    /// — the default — *crossfades*, so it would pass the oscillator through
    /// an empty track as a continuous tone. Scaling the oscillator by the
    /// voice's own level is what keeps it a modulator rather than a second
    /// voice droning under the kit.
    osc_env: f32,
    ripples: Svf,
    lfo1: ModStages,
    lfo2: ModStages,
    env1: ModStages,
    env2: ModStages,
    segment: [f32; BLOCK],
    lfo1_out: [f32; BLOCK],
    lfo2_out: [f32; BLOCK],
    env1_out: [f32; BLOCK],
    env2_out: [f32; BLOCK],
    // Cached strip parameters, derived from macros each block.
    warps_algorithm: f32,
    warps_timbre: f32,
    /// Resolved Warps input drive, 0.5..=1.0. Already remapped from the macro
    /// by [`warps_drive_from_macro`] at control rate.
    warps_drive: f32,
    /// Whether Warps is bypassed, i.e. `WARP.DRV` at 0.0.
    warps_bypassed: bool,
    ripples_cutoff_hz: f32,
    ripples_reso_q: f32,
    /// `RIP.FM`: audio-rate cutoff modulation depth, in octaves per unit of
    /// input at full scale.
    ripples_fm: f32,
    /// `WARP.MIX`: 0 = dry voice, 1 = Warps alone.
    warps_mix: f32,
    /// `WARP.IN`: which signal feeds Warps' modulator input.
    warps_mod_src: ModSource,
    /// `WARP.OUT`: crossfade between Warps' main (0) and aux (1) outputs.
    warps_out_tap: f32,
    /// Plaits' aux output for the current segment, captured by `tick` in the
    /// same order the main output is consumed so the two stay aligned.
    ///
    /// Filled sample by sample because that is how the engine collects source
    /// samples — interleaved across tracks, to keep the shared `stmlib::Random`
    /// draw order stable — so the strip cannot simply read `block_aux`: by the
    /// time it runs, `block_pos` has moved on and may have crossed a voice
    /// block boundary.
    aux_segment: [f32; BLOCK],
    /// How many samples of `aux_segment` the current segment has filled.
    aux_filled: usize,
    lfo_rate: f32,
    lfo_depth: f32,
    ad_attack: f32,
    ad_decay: f32,
    lfo_filter_depth: f32,
    lfo_warps_depth: f32,
    ad_filter_depth: f32,
    ad_warps_depth: f32,
    env_trigger_pending: bool,
    warps_initialized: bool,
    // Peaks-only state. The 16-bit parameter block the model was configured
    // with, kept so a macro edit can reconfigure without a voice restart.
    peaks_params: [f32; 4],
    /// Gate flag handed to the Peaks model on the sample a note starts. Plaits
    /// carries its trigger inside the patch; Peaks needs it per sample.
    peaks_gate: u8,
    /// Watchdog on [`Self::gate_open`], in rendered voice blocks. Counted
    /// down only while the gate is held.
    ///
    /// A held gate is what makes a track cost full price, because
    /// `is_active` is the engine's per-track early-out. A note-off that never
    /// arrives — a stuck key, a dropped cable — would otherwise keep the
    /// track rendering for the life of the device. See [`GATED_MAX_HOLD_S`].
    gate_watchdog: u32,
}

impl MiSlot {
    /// Map the TUNE macro (0..1) to a MIDI note number.
    const fn tune_to_note(tune: f32) -> f32 {
        24.0 + tune * 72.0
    }

    /// Recompute `patch.note` from stored tune macro and retune offset.
    fn update_note(&mut self) {
        self.patch.note = Self::tune_to_note(self.tune_macro) + self.retune_semitones;
    }

    /// Quantise the `WARP.CAR` macro onto the six Warps carrier sources.
    ///
    /// Six positions across 0..1, with 0.0 selecting [`Carrier::External`] so an
    /// untouched macro keeps the cross-modulator behaviour the strip had before
    /// this control existed. The remaining five select Warps' internal
    /// oscillators, which turn the same block into a small FM voice.
    /// `WARP.OSC` to a shape for the strip's modulator oscillator.
    fn osc_shape_from_macro(v: f32) -> OscShape {
        match (v * 5.0) as u32 {
            0 => OscShape::Sine,
            1 => OscShape::Triangle,
            2 => OscShape::Saw,
            3 => OscShape::Pulse,
            _ => OscShape::NoiseLp,
        }
    }

    /// `WARP.IN` to one of three modulator sources. See
    /// [`SLOT_WARPS_MOD_SRC`].
    fn mod_src_from_macro(v: f32) -> ModSource {
        match (v * 3.0) as u32 {
            0 => ModSource::Self_,
            1 => ModSource::Aux,
            _ => ModSource::Oscillator,
        }
    }

    /// Peak absolute sample of a rendered block.
    fn peak_of(buf: &[f32]) -> f32 {
        buf.iter().fold(0.0f32, |a, &s| a.max(libm::fabsf(s)))
    }

    /// The level below which this voice counts as finished.
    ///
    /// Per voice type, and not a detail: Plaits reaches true zero so it can use
    /// a tight gate, while Peaks parks on a fixed-point limit cycle. Using
    /// Plaits' threshold for Peaks would keep the track active indefinitely.
    const fn silence_floor(&self) -> f32 {
        if self.is_peaks {
            PEAKS_SILENCE_THRESHOLD
        } else {
            SILENCE_THRESHOLD
        }
    }

    /// Set up the four Stages segment generators as 2 LFOs + 2 AD envelopes.
    fn configure_stages(&mut self) {
        // Two looping LFOs. SEGMENT_ALT is an alternating oscillator; rate and
        // shape are updated from macros in set_macros.
        self.lfo1
            .configure_single(SEGMENT_ALT, true, false, 0.5, 0.5);
        self.lfo2
            .configure_single(SEGMENT_ALT, true, false, 0.5, 0.5);
        // Two triggered AD envelopes.
        self.env1.configure_ad(self.ad_attack, self.ad_decay);
        self.env2.configure_ad(self.ad_attack, self.ad_decay);
    }

    /// Map the `RIP.CUT` macro onto a cutoff in Hz, `20 Hz` at 0 to the
    /// filter's stability ceiling at 1.0.
    ///
    /// The obvious mapping is `20 * 1000^macro`, i.e. 20 Hz to 20 kHz. It is
    /// wrong here, and silently: the Chamberlin SVF cannot run at 20 kHz at
    /// any usable Q, so `core::dsp::svf` clamps it — at the default
    /// `RIP.RESO` the ceiling is ~6.4 kHz, which is **1.6 octaves** below
    /// where the knob claims to be. Everything above `macro ≈ 0.84` was one
    /// setting, and the default of 1.0 sat in the middle of that dead zone.
    ///
    /// That is not just a cosmetic knob problem. Modulation is applied to the
    /// cutoff in octaves, so a route whose full depth is ±1.5 octaves could
    /// never bring the cutoff back down into the live region — `LFO.FIL` at
    /// full depth was *exactly* inaudible, not merely subtle, and the test
    /// that is supposed to catch a dead route measured a delta of zero.
    ///
    /// Mapping the top of the macro onto the ceiling instead makes the knob
    /// monotonic over its whole travel and puts the default at the edge of the
    /// live region, where a downward modulation is immediately audible. The
    /// ceiling moves with Q, so lowering resonance really does buy top end —
    /// the same interaction the analog circuit has.
    ///
    /// Note this does not change what the *default* sounds like: at
    /// `RIP.CUT = 1.0` the realised cutoff was already the ceiling, because
    /// the filter clamped it there. It changes what the rest of the knob does,
    /// and it makes the cutoff reachable by modulation.
    fn ripples_cutoff_from_macro(macro_value: f32, q: f32) -> f32 {
        let ceiling = stability_ceiling_hz(q, SAMPLE_RATE);
        // 20 Hz at macro 0, `ceiling` at macro 1, exponential in between.
        20.0 * libm::powf(ceiling / 20.0, macro_value.clamp(0.0, 1.0))
    }

    /// Render the next block if the buffer is exhausted, honouring any pending
    /// trigger or release, and update the active/silence state.
    fn render_if_needed(&mut self) {
        if self.block_pos < VOICE_BLOCK {
            return;
        }

        // Consume the pending edges once. Leaving a trigger set would keep
        // `is_active` true forever — the silence counter would climb past
        // `SILENCE_BLOCKS`, `active` would go false, and the slot would still
        // never be allowed to rest.
        let triggered = self.trigger_pending;
        self.trigger_pending = false;
        let released = self.gate_release_pending;
        self.gate_release_pending = false;

        // A held gate is the expensive state, so the watchdog runs here. It
        // counts rendered samples, which is what the cycle budget is spent on.
        if self.gate_open {
            if self.gate_watchdog == 0 {
                self.gate_open = false;
            } else {
                self.gate_watchdog -= 1;
            }
        }

        let peak = if self.is_peaks {
            // Peaks is sample-driven and wants real gate flags. The flags are
            // per *sample*, not per block, so a held gate has to be spelled
            // out across the whole block or the model sees a pulse again and
            // retriggers nothing it should not.
            let sustained = if self.gate_open { GATE_HIGH } else { GATE_LOW };
            let mut gate = [sustained; VOICE_BLOCK];
            // One edge sample carries the transition: rising on note-on,
            // falling on note-off, steady otherwise. The shim translates
            // these to Peaks' own bit values, which are NOT stmlib's.
            if triggered {
                gate[0] = GATE_RISING;
            } else if released {
                gate[0] = GATE_FALLING;
            }
            self.peaks_gate = GATE_LOW;
            self.peaks
                .process(&gate[..VOICE_BLOCK], &mut self.block_out[..VOICE_BLOCK]);
            self.block_aux = [0.0f32; VOICE_BLOCK];
            self.block_pos = 0;
            Self::peak_of(&self.block_out[..VOICE_BLOCK])
        } else {
            // Plaits derives both the level and the rising edge from this one
            // value, with hysteresis and a 1 ms trigger delay
            // (`voice.cc:97`-`:150`). Holding it high for the note is
            // therefore all it takes: the edge still fires once, on the first
            // block where the delayed value crosses 0.3.
            self.modulations.trigger = if self.gate_open { 1.0 } else { 0.0 };
            self.plaits.render_f32(
                &self.patch,
                &self.modulations,
                &mut self.block_out,
                &mut self.block_aux,
                VOICE_BLOCK,
            );
            self.block_pos = 0;
            Self::peak_of(&self.block_out[..VOICE_BLOCK])
        };

        if peak < self.silence_floor() {
            self.silence_counter += 1;
            if self.silence_counter >= SILENCE_BLOCKS {
                self.active = false;
                // The voice has gone quiet, so the note is over. Close the
                // gate with it, whatever the key is doing.
                //
                // This is what keeps a one-shot drum machine working: a
                // drum-grid trigger sends no note-off, and without this a held
                // gate would keep the track rendering for the full watchdog
                // after every kick. A drum model's own envelope ends the
                // note, and the gate has to end with it.
                self.gate_open = false;
                self.gate_watchdog = 0;
            }
        } else {
            self.silence_counter = 0;
        }
    }
}

impl Slot<NUM_MACROS> for MiSlot {
    type Id = MiMachineId;

    fn new(id: Self::Id, macros: &[f32; NUM_MACROS]) -> Self {
        let mut slot = Self {
            plaits: PlaitsVoice::new(),
            peaks: PeaksVoice::new(id.peaks_model().unwrap_or(PeaksModel::FmDrum)),
            is_peaks: id.is_peaks(),
            peaks_params: [0.5, 0.3, 0.5, 0.3],
            peaks_gate: GATE_LOW,
            gate_watchdog: 0,
            block_out: [0.0f32; VOICE_BLOCK],
            block_aux: [0.0f32; VOICE_BLOCK],
            block_pos: VOICE_BLOCK, // force a render on the first tick
            patch: MiPlaitsPatch {
                note: 60.0,
                harmonics: 0.5,
                timbre: 0.5,
                morph: 0.5,
                frequency_modulation_amount: 0.0,
                timbre_modulation_amount: 0.0,
                morph_modulation_amount: 0.0,
                engine: id.plaits_engine(),
                decay: 0.5,
                lpg_colour: 0.5,
            },
            modulations: MiPlaitsModulations {
                engine: 0.0,
                note: 0.0,
                frequency: 0.0,
                harmonics: 0.0,
                timbre: 0.0,
                morph: 0.0,
                trigger: 0.0,
                level: 0.0,
                frequency_patched: 0,
                timbre_patched: 0,
                morph_patched: 0,
                trigger_patched: 1,
                level_patched: 0,
            },
            trigger_pending: false,
            gate_open: false,
            gate_release_pending: false,
            active: false,
            silence_counter: 0,
            tune_macro: 0.45,
            retune_semitones: 0.0,
            warps: Warps::new(SAMPLE_RATE),
            warps_osc_shape: OscShape::Sine,
            warps_osc: WarpsOscillator::new(SAMPLE_RATE),
            osc_env: 0.0,
            ripples: Svf::new(device_core::dsp::SvfMode::Lp),
            lfo1: ModStages::new(),
            lfo2: ModStages::new(),
            env1: ModStages::new(),
            env2: ModStages::new(),
            segment: [0.0f32; BLOCK],
            lfo1_out: [0.0f32; BLOCK],
            lfo2_out: [0.0f32; BLOCK],
            env1_out: [0.0f32; BLOCK],
            env2_out: [0.0f32; BLOCK],
            warps_algorithm: 0.0,
            warps_timbre: 0.0,
            warps_drive: 0.0,
            warps_bypassed: true,
            ripples_cutoff_hz: 1000.0,
            ripples_reso_q: 0.707,
            ripples_fm: 0.0,
            warps_mix: 1.0,
            warps_mod_src: ModSource::Self_,
            warps_out_tap: 0.0,
            aux_segment: [0.0f32; BLOCK],
            aux_filled: 0,
            lfo_rate: 0.5,
            lfo_depth: 1.0,
            ad_attack: 0.01,
            ad_decay: 0.3,
            lfo_filter_depth: 0.0,
            lfo_warps_depth: 0.0,
            ad_filter_depth: 0.0,
            ad_warps_depth: 0.0,
            env_trigger_pending: false,
            warps_initialized: false,
        };
        slot.configure_stages();
        slot.set_macros(macros);
        slot.ripples
            .recalc(slot.ripples_cutoff_hz, slot.ripples_reso_q, SAMPLE_RATE);
        slot
    }

    /// Hand the outgoing Plaits voice's pool buffer back before rebuilding.
    ///
    /// `Track::load_machine` only reaches this on an already-constructed
    /// slot, which is what makes the free safe: `new_in_place` on its own also
    /// runs on a `.uninit` static, where the old `buffer_index` would be
    /// garbage. Without the free, every machine reload leaks one of the
    /// pool's eight (firmware) buffers and a reload-heavy render panics with
    /// `Plaits buffer pool exhausted`.
    ///
    /// # Safety
    ///
    /// `ptr` must point to a slot previously built by `new_in_place`.
    #[allow(unsafe_code)]
    unsafe fn load_in_place(id: Self::Id, macros: &[f32; NUM_MACROS], ptr: *mut Self) {
        unsafe {
            PlaitsVoice::free_in_place(core::ptr::addr_of_mut!((*ptr).plaits));
            Self::new_in_place(id, macros, ptr);
        }
    }

    #[allow(unsafe_code)]
    unsafe fn new_in_place(id: Self::Id, macros: &[f32; NUM_MACROS], ptr: *mut Self) {
        unsafe {
            PlaitsVoice::new_in_place(core::ptr::addr_of_mut!((*ptr).plaits));
            core::ptr::addr_of_mut!((*ptr).peaks).write(PeaksVoice::new(
                id.peaks_model().unwrap_or(PeaksModel::FmDrum),
            ));
            core::ptr::addr_of_mut!((*ptr).is_peaks).write(id.is_peaks());
            core::ptr::addr_of_mut!((*ptr).peaks_params).write([0.5, 0.3, 0.5, 0.3]);
            core::ptr::addr_of_mut!((*ptr).peaks_gate).write(GATE_LOW);
            core::ptr::addr_of_mut!((*ptr).gate_watchdog).write(0);
            core::ptr::addr_of_mut!((*ptr).block_out).write([0.0f32; VOICE_BLOCK]);
            core::ptr::addr_of_mut!((*ptr).block_aux).write([0.0f32; VOICE_BLOCK]);
            core::ptr::addr_of_mut!((*ptr).block_pos).write(VOICE_BLOCK);
            core::ptr::addr_of_mut!((*ptr).patch).write(MiPlaitsPatch {
                note: 60.0,
                harmonics: 0.5,
                timbre: 0.5,
                morph: 0.5,
                frequency_modulation_amount: 0.0,
                timbre_modulation_amount: 0.0,
                morph_modulation_amount: 0.0,
                engine: id.plaits_engine(),
                decay: 0.5,
                lpg_colour: 0.5,
            });
            core::ptr::addr_of_mut!((*ptr).modulations).write(MiPlaitsModulations {
                engine: 0.0,
                note: 0.0,
                frequency: 0.0,
                harmonics: 0.0,
                timbre: 0.0,
                morph: 0.0,
                trigger: 0.0,
                level: 0.0,
                frequency_patched: 0,
                timbre_patched: 0,
                morph_patched: 0,
                trigger_patched: 1,
                level_patched: 0,
            });
            core::ptr::addr_of_mut!((*ptr).trigger_pending).write(false);
            core::ptr::addr_of_mut!((*ptr).gate_open).write(false);
            core::ptr::addr_of_mut!((*ptr).gate_release_pending).write(false);
            core::ptr::addr_of_mut!((*ptr).active).write(false);
            core::ptr::addr_of_mut!((*ptr).silence_counter).write(0);
            core::ptr::addr_of_mut!((*ptr).tune_macro).write(0.45);
            core::ptr::addr_of_mut!((*ptr).retune_semitones).write(0.0);
            core::ptr::addr_of_mut!((*ptr).warps).write(Warps::new(SAMPLE_RATE));
            core::ptr::addr_of_mut!((*ptr).warps_osc_shape).write(OscShape::Sine);
            core::ptr::addr_of_mut!((*ptr).warps_osc).write(WarpsOscillator::new(SAMPLE_RATE));
            core::ptr::addr_of_mut!((*ptr).osc_env).write(0.0);
            core::ptr::addr_of_mut!((*ptr).ripples).write(Svf::new(device_core::dsp::SvfMode::Lp));
            core::ptr::addr_of_mut!((*ptr).lfo1).write(ModStages::new());
            core::ptr::addr_of_mut!((*ptr).lfo2).write(ModStages::new());
            core::ptr::addr_of_mut!((*ptr).env1).write(ModStages::new());
            core::ptr::addr_of_mut!((*ptr).env2).write(ModStages::new());
            core::ptr::addr_of_mut!((*ptr).segment).write([0.0f32; BLOCK]);
            core::ptr::addr_of_mut!((*ptr).lfo1_out).write([0.0f32; BLOCK]);
            core::ptr::addr_of_mut!((*ptr).lfo2_out).write([0.0f32; BLOCK]);
            core::ptr::addr_of_mut!((*ptr).env1_out).write([0.0f32; BLOCK]);
            core::ptr::addr_of_mut!((*ptr).env2_out).write([0.0f32; BLOCK]);
            core::ptr::addr_of_mut!((*ptr).warps_algorithm).write(0.0);
            core::ptr::addr_of_mut!((*ptr).warps_timbre).write(0.0);
            core::ptr::addr_of_mut!((*ptr).warps_drive).write(0.0);
            core::ptr::addr_of_mut!((*ptr).warps_bypassed).write(true);
            core::ptr::addr_of_mut!((*ptr).ripples_cutoff_hz).write(1000.0);
            core::ptr::addr_of_mut!((*ptr).ripples_reso_q).write(0.707);
            core::ptr::addr_of_mut!((*ptr).ripples_fm).write(0.0);
            core::ptr::addr_of_mut!((*ptr).warps_mix).write(1.0);
            core::ptr::addr_of_mut!((*ptr).warps_mod_src).write(ModSource::Self_);
            core::ptr::addr_of_mut!((*ptr).warps_out_tap).write(0.0);
            core::ptr::addr_of_mut!((*ptr).aux_segment).write([0.0f32; BLOCK]);
            core::ptr::addr_of_mut!((*ptr).aux_filled).write(0);
            core::ptr::addr_of_mut!((*ptr).lfo_rate).write(0.5);
            core::ptr::addr_of_mut!((*ptr).lfo_depth).write(1.0);
            core::ptr::addr_of_mut!((*ptr).ad_attack).write(0.01);
            core::ptr::addr_of_mut!((*ptr).ad_decay).write(0.3);
            core::ptr::addr_of_mut!((*ptr).lfo_filter_depth).write(0.0);
            core::ptr::addr_of_mut!((*ptr).lfo_warps_depth).write(0.0);
            core::ptr::addr_of_mut!((*ptr).ad_filter_depth).write(0.0);
            core::ptr::addr_of_mut!((*ptr).ad_warps_depth).write(0.0);
            core::ptr::addr_of_mut!((*ptr).env_trigger_pending).write(false);
            core::ptr::addr_of_mut!((*ptr).warps_initialized).write(false);
            (*ptr).configure_stages();
            (*ptr).set_macros(macros);
            (*ptr)
                .ripples
                .recalc((*ptr).ripples_cutoff_hz, (*ptr).ripples_reso_q, SAMPLE_RATE);
        }
    }

    fn id(&self) -> Self::Id {
        if self.is_peaks {
            MiMachineId::from_peaks_model(self.peaks.model())
        } else {
            MiMachineId::from_index(self.patch.engine as usize)
                .unwrap_or(MiMachineId::VirtualAnalog)
        }
    }

    fn set_macros(&mut self, macros: &[f32; NUM_MACROS]) {
        if self.is_peaks {
            // A Peaks track reads the MACH bank as its own four parameters
            // rather than as Plaits patch fields. MACH 0 is the machine
            // selector (handled by `Track::load_machine`, not here), so the
            // model takes MACH 1..4 and TUNE is unused -- a Peaks model's
            // pitch comes from the MIDI note, not a macro.
            self.peaks_params = [
                macros[SLOT_MACH_1],
                macros[SLOT_MACH_2],
                macros[SLOT_MACH_3],
                macros[SLOT_MACH_4],
            ];
            self.peaks.configure(self.peaks_params);
        } else {
            self.tune_macro = macros[SLOT_MACH_0];
            self.update_note();
            self.patch.harmonics = macros[SLOT_MACH_1];
            self.patch.timbre = macros[SLOT_MACH_2];
            self.patch.morph = macros[SLOT_MACH_3];
            self.patch.frequency_modulation_amount = macros[SLOT_MACH_4];
            self.patch.timbre_modulation_amount = macros[SLOT_MACH_5];
            self.patch.morph_modulation_amount = macros[SLOT_MACH_6];
            self.patch.decay = macros[SLOT_MACH_7];
        }

        self.warps_algorithm = macros[SLOT_WARPS_ALGO];
        self.warps_timbre = macros[SLOT_WARPS_TIMBRE];
        // Resolve the drive remap once, at control rate, so the per-chunk path
        // stays two FFI calls with no arithmetic. See `warps_drive_from_macro`.
        let (bypass, drive) = warps_drive_from_macro(macros[SLOT_WARPS_DRIVE]);
        self.warps_bypassed = bypass;
        self.warps_drive = drive;
        self.warps_osc_shape = Self::osc_shape_from_macro(macros[SLOT_WARPS_OSC_SHAPE]);
        // Resonance first: it sets the filter's stability ceiling, and the
        // cutoff macro is mapped onto that ceiling rather than onto a fixed
        // 20 kHz. See `ripples_cutoff_from_macro`.
        self.ripples_reso_q = 0.5 + 19.5 * macros[SLOT_RIPPLES_RESONANCE];
        self.ripples_cutoff_hz =
            Self::ripples_cutoff_from_macro(macros[SLOT_RIPPLES_CUTOFF], self.ripples_reso_q);
        self.ripples
            .recalc(self.ripples_cutoff_hz, self.ripples_reso_q, SAMPLE_RATE);
        self.ripples_fm = macros[SLOT_RIPPLES_FM];
        self.warps_mix = macros[SLOT_WARPS_MIX].clamp(0.0, 1.0);
        self.warps_mod_src = Self::mod_src_from_macro(macros[SLOT_WARPS_MOD_SRC]);
        self.warps_out_tap = macros[SLOT_WARPS_OUT_TAP].clamp(0.0, 1.0);

        self.lfo_rate = macros[SLOT_LFO_RATE];
        self.lfo_depth = macros[SLOT_LFO_DEPTH];
        self.ad_attack = 0.001 + 0.999 * macros[SLOT_AD_ATTACK];
        self.ad_decay = 0.01 + 4.99 * macros[SLOT_AD_DECAY];
        self.lfo_filter_depth = macros[SLOT_LFO_FILTER_DEPTH];
        self.lfo_warps_depth = macros[SLOT_LFO_WARPS_DEPTH];
        self.ad_filter_depth = macros[SLOT_AD_FILTER_DEPTH];
        self.ad_warps_depth = macros[SLOT_AD_WARPS_DEPTH];

        // Update Stages parameters from macros. A Stages segment's `primary`
        // is a normalised rate: the free-running path maps it as
        // `SemitonesToRatio(96 * (primary - 0.5)) * 2.044 Hz`, so the useful
        // LFO band is roughly -0.05..0.9 rather than the 0..1 the parameter
        // nominally takes. This maps the knob onto ~0.1 Hz .. ~19 Hz, with
        // centre (0.5) at ~1.4 Hz. LFO 2 runs a fifth above LFO 1.
        let lfo_primary = -0.05 + 0.95 * self.lfo_rate;
        self.lfo1.set_parameters(lfo_primary, 0.5);
        self.lfo2.set_parameters(lfo_primary + 0.12, 0.5);
        self.env1.configure_ad(self.ad_attack, self.ad_decay);
        self.env2
            .configure_ad(self.ad_attack * 0.8, self.ad_decay * 1.2);
    }

    fn trigger(&mut self, _velocity: f32) {
        self.trigger_pending = true;
        self.env_trigger_pending = true;
        // Open the gate and (re)arm the watchdog. Velocity is intentionally
        // ignored -- see docs/peaks-vendoring.md.
        self.gate_open = true;
        self.gate_release_pending = false;
        self.gate_watchdog = GATED_MAX_HOLD_BLOCKS;
        // A Peaks track also needs the edge flag on the sample the note
        // starts, so a held gate still has to begin with a rising edge.
        if self.is_peaks {
            self.peaks_gate = GATE_RISING;
        }
        self.active = true;
        self.silence_counter = 0;
    }

    /// Close the gate — the note-off path.
    ///
    /// Plaits and Peaks both release natively once the gate is low: Plaits'
    /// outer LPG decays, and Peaks' gate processor runs its own release on the
    /// falling edge. Nothing else is needed, and nothing is forced — a
    /// one-shot drum model finishes its own decay whether the gate came down
    /// or not, which is why this does not cut.
    ///
    /// A no-op for a note that already ended, or a second release for the same
    /// note.
    fn release(&mut self) {
        if !self.gate_open {
            return;
        }
        self.gate_open = false;
        self.gate_release_pending = true;
        self.gate_watchdog = 0;
    }

    fn retune(&mut self, semis: f32) {
        self.retune_semitones = semis;
        self.update_note();
    }

    fn reset(&mut self) {
        self.osc_env = 0.0;
        self.trigger_pending = false;
        self.gate_open = false;
        self.gate_release_pending = false;
        self.gate_watchdog = 0;
        self.peaks_gate = GATE_LOW;
        self.active = false;
        self.silence_counter = 0;
        self.block_pos = VOICE_BLOCK;
    }

    fn is_active(&self) -> bool {
        // A held gate counts as active even if the voice has not produced
        // output yet: the note has started and the engine still owes it
        // samples. Without this, a gate opened on a voice that takes a few
        // blocks to charge would be declared idle first.
        self.active || self.trigger_pending || self.gate_open
    }

    fn tick(&mut self) -> f32 {
        if !self.is_active() {
            return 0.0;
        }
        self.render_if_needed();
        let s = self.block_out[self.block_pos];
        // Capture the matching aux sample for the strip. Peaks zeroes
        // `block_aux`, so a Peaks track records silence here and
        // `process_audio_strip` falls back to self-modulation.
        if self.aux_filled < BLOCK {
            self.aux_segment[self.aux_filled] = self.block_aux[self.block_pos];
            self.aux_filled += 1;
        }
        self.block_pos += 1;
        s
    }

    fn process_audio_strip(&mut self, buf: &mut [f32], _start: usize) {
        let n = buf.len();

        // Warps' C++ state contains a self-referential pointer. It must be
        // initialised at its final memory location, which for `new()` happens
        // to be after the struct has been moved into the engine. Lazy-init on
        // first audio strip call covers both construction paths.
        if !self.warps_initialized {
            self.warps.init(SAMPLE_RATE);
            self.warps_initialized = true;
        }

        // Render the four Stages modulators into per-segment buffers. The LFOs
        // must free-run: a gate array would clock them from the gates and hold
        // them at a constant. `SEGMENT_ALT` already emits bipolar output, so
        // the LFO buffers need no recentring; the AD envelopes are unipolar.
        let mut gate = [GATE_LOW; BLOCK];
        if self.env_trigger_pending {
            gate[0] = GATE_RISING;
            self.env_trigger_pending = false;
        }
        self.lfo1.process_free_running(&mut self.lfo1_out[..n]);
        self.lfo2.process_free_running(&mut self.lfo2_out[..n]);
        self.env1.process(&gate[..n], &mut self.env1_out[..n]);
        self.env2.process(&gate[..n], &mut self.env2_out[..n]);

        // Modulate Warps timbre once per Warps-sized chunk (Warps interpolates
        // parameters internally over the chunk). Ripples cutoff is updated per
        // sample because tick() is cheap. `lfo_depth` is the master LFO scalar
        // applied on top of each per-target depth, so the whole LFO bus can be
        // scaled without touching four separate macros.
        let lfo_master = self.lfo_depth;
        let lfo_warps_scale = self.lfo_warps_depth * lfo_master;
        let ad_warps_scale = self.ad_warps_depth;
        let base_warps_timbre = self.warps_timbre;

        for (chunk_idx, chunk) in buf.chunks_mut(WARPS_MAX_BLOCK).enumerate() {
            let chunk_start = chunk_idx * WARPS_MAX_BLOCK;

            // Point-sample the modulators at the chunk's first sample rather
            // than averaging across it. Warps interpolates its own parameters
            // between calls, so this is already smooth, and averaging a 32-sample
            // window against a ~1.4 Hz LFO would cancel most of the sweep and
            // leave the route silent.
            let modulated_timbre = (base_warps_timbre
                + self.lfo2_out[chunk_start] * lfo_warps_scale
                + self.env2_out[chunk_start] * ad_warps_scale)
                .clamp(0.0, 1.0);

            // Drive 0 is a real bypass, not silence — see `WARPS_DRIVE_INFO` for
            // why the knob is remapped rather than passed straight through.
            // Both flags were resolved at control rate in `set_macros`.
            self.warps.set_bypass(self.warps_bypassed);
            // `Carrier::External` always: the strip supplies its own
            // oscillator on the modulator side, so Warps' internal-carrier
            // path — which replaces the voice rather than modulating it — is
            // never used, and `note` goes unread as a result.
            self.warps.set_parameters(
                self.warps_algorithm,
                modulated_timbre,
                self.warps_drive,
                Carrier::External,
                self.patch.note,
            );

            let len = chunk.len();
            // The dry signal, kept so `WARP.MIX` has something to blend back
            // towards. Warps itself is always 100% wet.
            let mut dry = [0.0f32; WARPS_MAX_BLOCK];
            dry[..len].copy_from_slice(chunk);

            // Warps' modulator input. Feeding it the same signal as the
            // carrier is the degenerate case the strip used to be stuck in —
            // a comparator with nothing to compare — so this is where the
            // second signal comes from.
            let mut modulator = [0.0f32; WARPS_MAX_BLOCK];
            let aux_available = !self.is_peaks && self.aux_filled >= chunk_start + len;
            match self.warps_mod_src {
                ModSource::Aux if aux_available => {
                    modulator[..len]
                        .copy_from_slice(&self.aux_segment[chunk_start..chunk_start + len]);
                }
                ModSource::Oscillator => {
                    // Silent modulation input: as a modulator the oscillator
                    // wants to be a defined tone rather than something the
                    // voice smears. `Duck` passes the noise shape through
                    // untouched when its external input is silent.
                    let silence = [0.0f32; WARPS_MAX_BLOCK];
                    self.warps_osc.render(
                        self.warps_osc_shape,
                        self.patch.note,
                        &silence[..len],
                        &mut modulator[..len],
                    );
                    // Two gains. The 0.5 is the one Warps applies to its own
                    // internal carrier (`kXmodCarrierGain`); the shapes are
                    // not level-matched, triangle peaking near 2.0 where sine
                    // reaches 1.0, and this is the module's own compensation.
                    //
                    // The envelope is ours, and it is what stops the
                    // oscillator being a drone. Instant attack so a hit is
                    // modulated from its first sample, slow release so the
                    // gate does not chatter on a waveform's own zero
                    // crossings. See `osc_env`.
                    for i in 0..len {
                        let level = if dry[i] < 0.0 { -dry[i] } else { dry[i] };
                        if level > self.osc_env {
                            self.osc_env = level;
                        } else {
                            self.osc_env += (level - self.osc_env) * OSC_ENV_RELEASE;
                        }
                        modulator[i] *= 0.5 * self.osc_env;
                    }
                }
                // Self, and Aux on a voice that has no aux to offer.
                _ => modulator[..len].copy_from_slice(&dry[..len]),
            }

            let mut warps_aux = [0.0f32; WARPS_MAX_BLOCK];
            self.warps
                .process_dual(chunk, &modulator[..len], &mut warps_aux[..len]);

            // `WARP.OUT` picks the tap, then `WARP.MIX` decides how much of
            // the result replaces the voice.
            let tap = self.warps_out_tap;
            let mix = self.warps_mix;
            for i in 0..len {
                let wet = chunk[i] + tap * (warps_aux[i] - chunk[i]);
                chunk[i] = dry[i] + mix * (wet - dry[i]);
            }
        }

        // Ripples multimode SVF after Warps, with per-sample cutoff modulation
        // from LFO 1 and AD envelope 1.
        let lfo_filter_scale = self.lfo_filter_depth * lfo_master * 3.0;
        let ad_filter_scale = self.ad_filter_depth * 3.0;
        // `RIP.FM` is audio rate and self-sourced: the sample about to enter
        // the filter also displaces the cutoff, which is what gives a
        // state-variable filter its growl. It shares the exponent with the two
        // control-rate routes, so it costs an add rather than a second `powf`.
        let fm_scale = self.ripples_fm * 2.0;
        let base_cutoff = self.ripples_cutoff_hz;

        for i in 0..n {
            let cutoff = (base_cutoff
                * libm::powf(
                    2.0,
                    self.lfo1_out[i] * lfo_filter_scale
                        + self.env1_out[i] * ad_filter_scale
                        + buf[i] * fm_scale,
                ))
            .clamp(20.0, 20000.0);
            self.ripples.set_cutoff(cutoff, SAMPLE_RATE);
            buf[i] = self.ripples.tick(buf[i]);
        }

        // The segment is done with; the next one refills from zero.
        self.aux_filled = 0;
    }
}

/// One channel of the mi-drum kit.
pub type Track = device_core::track::Track<MiSlot, NUM_MACROS>;
/// A complete mi-drum sound: machine + macros + strip.
pub type Sound = device_core::sound::Sound<MiSlot, NUM_MACROS>;

/// Default kit: drum-focused Plaits engines on the first tracks, melodic on
/// the later tracks. Limited to six tracks so the engine still fits in the
/// Teensy 4.1 OCRAM budget alongside the shared send effects.
///
/// Public so the host renderer can restore it after auditioning other
/// machines on a track.
/// Tracks 0-3 are Peaks drums and 4-5 are Plaits oscillators.
///
/// The split is a kit choice, not a hardware rule: a slot holds one `PlaitsVoice`
/// and one `PeaksVoice` and whichever machine is loaded decides which sounds,
/// so the `MACH` selector can still reach all [`MiMachineId::COUNT`] machines on
/// any track. Tracks 0-3 default to Peaks because a drum kit wants actual drum
/// models there, and because the whole point of vendoring Peaks was to have
/// them, not 24 more oscillators.
pub const DEFAULT_KIT: [MiMachineId; TRACKS] = [
    MiMachineId::PeaksBassDrum,  // 0: kick
    MiMachineId::PeaksSnareDrum, // 1: snare
    MiMachineId::PeaksHighHat,   // 2: closed hat
    MiMachineId::PeaksFmDrum,    // 3: FM tom/conga-like
    MiMachineId::String,         // 4: melodic/string
    MiMachineId::Modal,          // 5: resonant/modal
];

/// The mi-drum engine: a [`DeviceEngine`] pre-configured for the kit.
pub struct MiDrumEngine {
    inner: Engine<MiSlot, NUM_MACROS, TRACKS>,
}

impl core::ops::Deref for MiDrumEngine {
    type Target = Engine<MiSlot, NUM_MACROS, TRACKS>;
    fn deref(&self) -> &Self::Target {
        &self.inner
    }
}

impl core::ops::DerefMut for MiDrumEngine {
    fn deref_mut(&mut self) -> &mut Self::Target {
        &mut self.inner
    }
}

impl DeviceEngine<NUM_MACROS> for MiDrumEngine {
    type Slot = MiSlot;

    fn tracks(&self) -> &[Track] {
        &self.inner.tracks
    }

    fn tracks_mut(&mut self) -> &mut [Track] {
        &mut self.inner.tracks
    }

    fn trigger(&mut self, track: usize, velocity: f32) {
        self.inner.trigger(track, velocity)
    }

    fn release(&mut self, track: usize) {
        self.inner.release(track)
    }

    fn release_note(&mut self, note: u8) -> Option<usize> {
        self.inner.release_note(note)
    }

    fn release_channel(&mut self, channel: u8, note: u8) -> Option<usize> {
        self.inner.release_channel(channel, note)
    }

    fn trigger_note(&mut self, note: u8, velocity: f32) -> Option<usize> {
        self.inner.trigger_note(note, velocity)
    }

    fn trigger_channel(&mut self, channel: u8, note: u8, velocity: f32) -> Option<usize> {
        self.inner.trigger_channel(channel, note, velocity)
    }

    fn set_note(&mut self, note: u8, track: Option<u8>) {
        self.inner.set_note(note, track)
    }

    fn panic(&mut self) {
        self.inner.panic()
    }

    fn schedule_timed(&mut self, offset: usize, event: EngineEvent) -> bool {
        self.inner.schedule_timed(offset, event)
    }

    fn load_sound(&mut self, track: usize, sound: &Sound) {
        self.inner.load_sound(track, sound)
    }

    fn trigger_with_sound(&mut self, track: usize, velocity: f32, sound: &Sound) {
        self.inner.trigger_with_sound(track, velocity, sound)
    }

    fn is_active(&self) -> bool {
        self.inner.is_active()
    }

    fn process(&mut self, out_l: &mut [f32], out_r: &mut [f32]) {
        self.inner.process(out_l, out_r)
    }

    fn process_dry_wet(
        &mut self,
        master_l: &mut [f32; BLOCK],
        master_r: &mut [f32; BLOCK],
        aux: &mut [[f32; BLOCK]; 6],
        wet_l: &mut [f32; BLOCK],
        wet_r: &mut [f32; BLOCK],
    ) {
        self.inner
            .process_dry_wet(master_l, master_r, aux, wet_l, wet_r)
    }

    fn master_gain(&self) -> f32 {
        self.inner.master_gain
    }

    fn set_master_gain(&mut self, value: f32) {
        self.inner.master_gain = value
    }

    fn fx_drive(&self) -> f32 {
        self.inner.send_fx.drive
    }

    fn set_fx_drive(&mut self, value: f32) {
        self.inner.send_fx.drive = value
    }

    fn load_kit(&mut self, kit: &[<MiSlot as Slot<NUM_MACROS>>::Id]) {
        self.inner
            .load_kit(kit.try_into().expect("kit length must match track count"))
    }
}

impl Default for MiDrumEngine {
    fn default() -> Self {
        Self::new()
    }
}

impl MiDrumEngine {
    /// Build a mi-drum engine with the default kit.
    pub fn new() -> Self {
        let mut e = Self {
            inner: Engine::new_with_kit(&DEFAULT_KIT),
        };
        e.configure_strip();
        e
    }

    /// Initialize a mi-drum engine in-place.
    ///
    /// # Safety
    /// `dst` must point to writable memory of at least `size_of::<MiDrumEngine>()` bytes.
    #[allow(unsafe_code)]
    pub unsafe fn new_in_place<'a>(dst: *mut MiDrumEngine) -> &'a mut MiDrumEngine {
        Engine::new_in_place_with_kit(core::ptr::addr_of_mut!((*dst).inner), &DEFAULT_KIT);
        let engine = &mut *dst;
        engine.configure_strip();
        engine
    }

    fn configure_strip(&mut self) {
        // mi-drum uses its own fixed Warps -> Ripples strip, so bypass the
        // generic per-track filter/amp/drive strip.
        for track in self.inner.tracks.iter_mut() {
            track.strip_bypass = true;
        }
    }
}

/// No default note map, and that is deliberate.
///
/// This device used to preload a GM-percussion map — note 36 to the kick, 38 to
/// the snare, and so on — inherited from the drum machine when the two devices
/// were split in Phase 13. It was wrong here, and worse, it hid a capability
/// that already worked: live MIDI routes NoteOn through
/// [`DeviceEngine::trigger_channel`](device_core::engine::DeviceEngine::trigger_channel),
/// which never consults `note_map` at all.
///
/// The device is a channel-per-track voice, so the conventions are:
///
/// - **MIDI channel selects the track** (`channel < TRACKS`).
/// - **The note selects pitch.** `TUNE` sets the pitch the voice plays at
///   middle C; every semitone away transposes the whole voice via
///   [`Track::retune`](device_core::track::Track::retune). That is
///   `core`'s existing chromatic convention, and it is what
///   `chromatic_play_transposes_by_octave` pins.
/// - `note_map` stays empty, so the programmatic [`trigger_note`](DeviceEngine::trigger_note)
///   path is silent unless a host assigns notes with `set_note`. A host that
///   wants a fixed note-to-track layout can build one; the device does not
///   impose a GM kit on a player who wants six instruments.

#[cfg(test)]
mod tests {
    use super::*;

    /// Heap-allocate the engine so the huge struct is not placed on the test
    /// thread's stack. The firmware uses static `.uninit` placement instead.
    #[allow(unsafe_code)]
    fn engine_box() -> std::boxed::Box<MiDrumEngine> {
        use std::alloc::{alloc, Layout};

        unsafe {
            let layout = Layout::new::<MiDrumEngine>();
            let ptr = alloc(layout) as *mut MiDrumEngine;
            assert!(!ptr.is_null(), "failed to allocate MiDrumEngine on heap");
            MiDrumEngine::new_in_place(ptr);
            std::boxed::Box::from_raw(ptr)
        }
    }

    /// Reloading machines must hand the outgoing voice's pool buffer back.
    ///
    /// `PlaitsVoice::new_in_place` cannot free what it overwrites, because it
    /// also runs on a `.uninit` static where the old buffer index is garbage.
    /// The free therefore lives in `MiSlot::load_in_place`, which only ever
    /// sees a constructed slot. If that hook stops being called, every reload
    /// below leaks one buffer and the finite pool runs dry — 200 reloads is
    /// well past the host pool's 128 entries, and past the firmware's 8.
    #[test]
    fn repeated_machine_reloads_do_not_exhaust_the_voice_pool() {
        let mut engine = engine_box();
        for i in 0..200 {
            let id = MiMachineId::ALL[i % MiMachineId::ALL.len()];
            engine.tracks_mut()[i % TRACKS].load_machine(id);
        }
        // A leaked pool would have panicked inside `load_machine`; reaching
        // here is the assertion. Render once so the voices are actually used.
        let _ = render_peak(&mut engine, 0, 2);
    }

    fn render_peak(engine: &mut MiDrumEngine, track: usize, blocks: usize) -> f32 {
        let mut l = [0.0f32; BLOCK];
        let mut r = [0.0f32; BLOCK];
        engine.trigger(track, 1.0);
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
        let mut e = engine_box();
        let mut l = [0.0f32; BLOCK];
        let mut r = [0.0f32; BLOCK];
        for _ in 0..16 {
            e.process(&mut l, &mut r);
        }
        assert_eq!(
            l.iter()
                .chain(r.iter())
                .fold(0.0f32, |a, &s| a.max(s.abs())),
            0.0
        );
        assert!(!e.is_active());
    }

    #[test]
    fn bass_drum_makes_noise_then_stops() {
        let mut e = engine_box();
        let peak = render_peak(&mut e, 0, 8);
        assert!(peak > 0.1, "bass drum should be audible: {peak}");

        // Run long enough for any reasonable decay.
        let mut l = [0.0f32; BLOCK];
        let mut r = [0.0f32; BLOCK];
        for _ in 0..(5.0 * SAMPLE_RATE / BLOCK as f32) as usize {
            e.process(&mut l, &mut r);
        }
        assert!(!e.is_active(), "voice should have decayed to silence");
    }

    #[test]
    fn trigger_channel_routes_channel_to_track() {
        let mut e = engine_box();
        assert_eq!(e.trigger_channel(3, 60, 1.0), Some(3));
        assert!(e.tracks[3].is_active());
    }

    /// The master clipper's bound, asserted against what the DAC can actually
    /// see rather than against exact unity.
    ///
    /// `fast::soft_clip` is a rational approximation carrying `recip`'s ~2 ulp
    /// of error, so a peak landing on its internal ±3 clamp can come out at
    /// 1.0000001. That is -200 dB, and in 24-bit it converts to exactly full
    /// scale rather than wrapping — so the real requirement is "cannot wrap the
    /// DAC", not "is bit-exactly 1.0". Asserting exact unity here once forced a
    /// `clamp` into a per-sample hot path on every device, which cost cycles and
    /// moved the drum engine's bit-identity baseline to remove an artifact no
    /// converter can distinguish from full scale.
    #[test]
    fn output_never_exceeds_unity() {
        // 1 ulp at 1.0 is 1.19e-7. Allow the documented approximation error.
        const TOL: f32 = 4.0 * 1.19e-7;
        let mut e = engine_box();
        let mut l = [0.0f32; BLOCK];
        let mut r = [0.0f32; BLOCK];
        let strip = StripParams::default();
        for t in e.tracks.iter_mut() {
            t.set_strip(&strip);
        }
        for i in 0..200 {
            for trk in 0..TRACKS {
                e.trigger(trk, 1.0);
            }
            e.process(&mut l, &mut r);
            for &s in l.iter().chain(r.iter()) {
                assert!(s.is_finite(), "non-finite sample on block {i}");
                assert!(
                    s.abs() <= 1.0 + TOL,
                    "clipper let {s} through on block {i} — more than the \
                     approximation's own error, so this is a real overflow"
                );
            }
        }
    }

    /// The note map is empty on purpose, so the programmatic `trigger_note`
    /// path is silent until a host assigns notes. Live MIDI is unaffected: it
    /// routes through `trigger_channel`, which never reads the map.
    #[test]
    fn trigger_note_is_silent_until_a_host_maps_it() {
        let mut e = engine_box();
        assert_eq!(
            e.trigger_note(36, 1.0),
            None,
            "no note map should be loaded"
        );
        assert!(!e.is_active());

        e.set_note(36, Some(0));
        assert_eq!(e.trigger_note(36, 1.0), Some(0));
        assert!(e.is_active());
    }

    /// Capture length for the modulation-route tests. Two channels per frame,
    /// a whole number of blocks.
    const MOD_CAPTURE: usize = 2 * BLOCK * 40;

    /// Render a single hit with every modulation depth at `depth`, for the
    /// per-route test.
    fn render_depth(slot: usize, depth: f32) -> [f32; MOD_CAPTURE] {
        let mut e = engine_box();
        for t in e.tracks.iter_mut() {
            t.set_macro(slot, depth);
            // The AD envelopes are unipolar, so `AD.FIL` can only open the
            // filter. With the default `RIP.CUT` fully open there is no
            // headroom above it and the route is *correctly* silent — so close
            // the filter first, which is what a player does before reaching
            // for a filter-envelope amount. Testing the route at the default
            // would be testing the default, not the route.
            if slot == SLOT_AD_FILTER_DEPTH {
                t.set_macro(SLOT_STRIP_CUT, 0.4);
            }
        }
        e.trigger(0, 1.0);
        let mut l = [0.0f32; BLOCK];
        let mut r = [0.0f32; BLOCK];
        let mut out = [0.0f32; MOD_CAPTURE];
        let mut w = 0usize;
        for _ in 0..(MOD_CAPTURE / (2 * BLOCK)) {
            e.process(&mut l, &mut r);
            for i in 0..BLOCK {
                out[w] = l[i];
                out[w + 1] = r[i];
                w += 2;
            }
        }
        out
    }

    /// Count zero crossings of a track's *source* voice over a fixed window,
    /// as a pitch proxy.
    ///
    /// Measured before the strip, deliberately. Warps cross-modulates the
    /// signal with itself and Ripples low-passes it, so the post-strip waveform
    /// carries difference frequencies and harmonics that make zero crossings a
    /// meaningless pitch proxy — the strip is a colour stage and is allowed to
    /// change the spectrum. The source is where pitch lives, and it is what
    /// chromatic play actually depends on.
    fn source_crossings(e: &mut MiDrumEngine, samples: usize) -> u32 {
        let slot = &mut e.tracks_mut()[0].slot;
        let mut prev = 0.0f32;
        let mut count = 0u32;
        for _ in 0..samples {
            let s = slot.tick();
            if (prev < 0.0) != (s < 0.0) {
                count += 1;
            }
            prev = s;
        }
        count
    }

    /// The headline capability the GM note map used to mask: the tracks are
    /// separate instruments playable chromatically, not six fixed percussion
    /// keys. Channel picks the track, the note picks the pitch, and two
    /// octaves must give four times the frequency.
    #[test]
    fn chromatic_play_transposes_by_octave() {
        const SAMPLES: usize = 1600;

        let crossings_at = |note: u8| {
            let mut e = engine_box();
            // A tonal oscillator, so crossings track the fundamental.
            e.tracks_mut()[0].load_machine(MiMachineId::VirtualAnalog);
            e.tracks_mut()[0].set_macro(SLOT_MACH_7, 0.95); // sustain
            assert_eq!(e.trigger_channel(0, note, 1.0), Some(0));
            source_crossings(&mut e, SAMPLES)
        };

        let middle = crossings_at(60);
        let octave_up = crossings_at(72);
        let two_octaves = crossings_at(84);
        assert!(middle > 4, "middle C should be clearly tonal, got {middle}");

        assert!(
            (1.8..2.2).contains(&(octave_up as f32 / middle as f32)),
            "one octave up should double the frequency: {middle} -> {octave_up}"
        );
        assert!(
            (3.6..4.4).contains(&(two_octaves as f32 / middle as f32)),
            "two octaves up should quadruple the frequency: {middle} -> {two_octaves}"
        );
    }

    #[test]
    fn every_modulation_route_changes_the_sound() {
        for route in [
            SLOT_LFO_FILTER_DEPTH,
            SLOT_LFO_WARPS_DEPTH,
            SLOT_AD_FILTER_DEPTH,
            SLOT_AD_WARPS_DEPTH,
        ] {
            let dry = render_depth(route, 0.0);
            let wet = render_depth(route, 0.8);
            let moved = dry
                .iter()
                .zip(wet.iter())
                .map(|(a, b)| (a - b).abs())
                .fold(0.0f32, f32::max);
            assert!(
                moved > 1.0e-4,
                "modulation route at slot {route} is dead (max delta {moved})"
            );
        }
    }

    /// Render track 0 for a fixed window with a set of macro overrides.
    fn render_with_macros(id: MiMachineId, overrides: &[(usize, f32)]) -> [f32; MOD_CAPTURE] {
        let mut e = engine_box();
        e.tracks_mut()[0].load_machine(id);
        for &(slot, v) in overrides {
            e.tracks_mut()[0].set_macro(slot, v);
        }
        e.trigger_channel(0, 60, 1.0);
        let mut l = [0.0f32; BLOCK];
        let mut r = [0.0f32; BLOCK];
        let mut out = [0.0f32; MOD_CAPTURE];
        let mut w = 0usize;
        for _ in 0..(MOD_CAPTURE / (2 * BLOCK)) {
            e.process(&mut l, &mut r);
            for i in 0..BLOCK {
                out[w] = l[i];
                out[w + 1] = r[i];
                w += 2;
            }
        }
        out
    }

    fn max_delta(a: &[f32], b: &[f32]) -> f32 {
        a.iter()
            .zip(b.iter())
            .map(|(x, y)| (x - y).abs())
            .fold(0.0f32, f32::max)
    }

    /// `WARP.MIX` at 0 must be the voice, exactly.
    ///
    /// Not "close to": the blend is `dry + mix * (wet - dry)`, so at mix 0 the
    /// wet path cannot contribute a single bit. This is the setting that makes
    /// Warps optional without reaching for the bypass detent, and a strip that
    /// leaked any wet signal here would be a strip you could not turn off.
    #[test]
    fn warps_mix_at_zero_is_the_dry_voice() {
        let dry = render_with_macros(
            MiMachineId::VirtualAnalog,
            &[(SLOT_WARPS_MIX, 0.0), (SLOT_WARPS_DRIVE, 0.8)],
        );
        // Same again with the drive somewhere else entirely: if mix 0 is
        // really dry, the drive cannot matter.
        let dry_other = render_with_macros(
            MiMachineId::VirtualAnalog,
            &[(SLOT_WARPS_MIX, 0.0), (SLOT_WARPS_DRIVE, 0.2)],
        );
        assert_eq!(
            max_delta(&dry, &dry_other),
            0.0,
            "WARP.DRV changed the output at WARP.MIX 0 — the wet path is leaking"
        );

        let wet = render_with_macros(
            MiMachineId::VirtualAnalog,
            &[(SLOT_WARPS_MIX, 1.0), (SLOT_WARPS_DRIVE, 0.8)],
        );
        assert!(
            max_delta(&dry, &wet) > 1.0e-3,
            "WARP.MIX did nothing between 0 and 1"
        );
    }

    /// Plaits' aux output as the modulator is a different patch, and must
    /// sound like one. This is the whole point of `WARP.IN`.
    #[test]
    fn warps_modulator_source_changes_the_sound_on_plaits() {
        let base = [(SLOT_WARPS_MIX, 1.0), (SLOT_WARPS_DRIVE, 0.5)];
        let mut selfmod = base.to_vec();
        selfmod.push((SLOT_WARPS_MOD_SRC, 0.0)); // self
        let mut auxmod = base.to_vec();
        auxmod.push((SLOT_WARPS_MOD_SRC, 0.5)); // aux

        let a = render_with_macros(MiMachineId::VirtualAnalog, &selfmod);
        let b = render_with_macros(MiMachineId::VirtualAnalog, &auxmod);
        assert!(
            max_delta(&a, &b) > 1.0e-3,
            "WARP.IN is inert — the aux output is not reaching the modulator"
        );
    }

    /// Warps' aux tap is the saturated input sum, not the cross-modulation, so
    /// selecting it has to produce a different signal.
    #[test]
    fn warps_output_tap_selects_a_different_signal() {
        let base = [(SLOT_WARPS_MIX, 1.0), (SLOT_WARPS_DRIVE, 0.6)];
        let mut main = base.to_vec();
        main.push((SLOT_WARPS_OUT_TAP, 0.0));
        let mut aux = base.to_vec();
        aux.push((SLOT_WARPS_OUT_TAP, 1.0));

        let a = render_with_macros(MiMachineId::VirtualAnalog, &main);
        let b = render_with_macros(MiMachineId::VirtualAnalog, &aux);
        assert!(
            max_delta(&a, &b) > 1.0e-3,
            "WARP.OUT is inert — both taps return the same buffer"
        );
    }

    /// `RIP.FM` was a labelled, defaulted macro that nothing read. It is wired
    /// now, and this is the guard that keeps it wired.
    #[test]
    fn ripples_fm_is_live() {
        // The filter has to be somewhere it can be pushed from, and the strip
        // has to be audible, or this measures the default rather than the knob.
        let base = [(SLOT_WARPS_MIX, 0.0), (SLOT_STRIP_CUT, 0.4)];
        let mut off = base.to_vec();
        off.push((SLOT_RIPPLES_FM, 0.0));
        let mut on = base.to_vec();
        on.push((SLOT_RIPPLES_FM, 0.9));

        let a = render_with_macros(MiMachineId::VirtualAnalog, &off);
        let b = render_with_macros(MiMachineId::VirtualAnalog, &on);
        assert!(
            max_delta(&a, &b) > 1.0e-4,
            "RIP.FM is dead again (max delta {})",
            max_delta(&a, &b)
        );
        for s in b.iter() {
            assert!(s.is_finite(), "RIP.FM produced a non-finite sample");
            assert!(s.abs() <= 1.0 + 4.0 * 1.19e-7, "RIP.FM blew the clipper");
        }
    }

    /// `WARP.TIM` has to change the *shape* of the sound, not just its level.
    ///
    /// With the same signal in both Warps inputs, `ALGORITHM_XFADE` reduces to
    /// `x * (fade_in + fade_out)` — a scalar in the timbre parameter and
    /// nothing else. The knob is then a trim, and the two modulation routes
    /// that target it (`LFO.WRP`, `AD.WRP`) are tremolo. Feeding the aux
    /// output into the modulator input is what gives the crossfade two
    /// different things to fade between.
    ///
    /// Measured by normalising both renders to the same RMS first: whatever
    /// survives that is shape rather than level.
    #[test]
    fn warps_timbre_is_a_timbre_control_once_aux_feeds_the_modulator() {
        let shape_delta = |mod_src: f32| {
            let render = |tim: f32| {
                let mut out = render_with_macros(
                    MiMachineId::VirtualAnalog,
                    &[
                        (SLOT_WARPS_MIX, 1.0),
                        (SLOT_WARPS_DRIVE, 0.3),
                        (SLOT_WARPS_ALGO, 0.0),
                        (SLOT_WARPS_MOD_SRC, mod_src),
                        (SLOT_WARPS_TIMBRE, tim),
                    ],
                );
                // Normalise to unit RMS so a pure gain difference cancels.
                let mut sum = 0.0f32;
                for s in out.iter() {
                    sum += s * s;
                }
                let rms = libm::sqrtf(sum / out.len() as f32);
                if rms > 1.0e-9 {
                    for s in out.iter_mut() {
                        *s /= rms;
                    }
                }
                out
            };
            let a = render(0.2);
            let b = render(0.8);
            let mut sum = 0.0f32;
            for (x, y) in a.iter().zip(b.iter()) {
                sum += (x - y) * (x - y);
            }
            libm::sqrtf(sum / a.len() as f32)
        };

        let selfmod = shape_delta(0.0);
        let auxmod = shape_delta(1.0);
        assert!(
            auxmod > selfmod * 2.0,
            "aux-modulated timbre is not meaningfully more than a gain change: \
             self {selfmod:.4} vs aux {auxmod:.4}"
        );
    }

    /// The interaction that makes `AD.FIL` look broken, pinned so it is a
    /// documented property rather than a mystery. The AD envelopes are
    /// unipolar, so the route can only open the filter; with the default
    /// `RIP.CUT` fully open there is no headroom and the depth is silent.
    #[test]
    fn ad_filter_depth_needs_the_filter_closed_first() {
        let moved = |cut: f32| {
            let dry = {
                let mut e = engine_box();
                for t in e.tracks.iter_mut() {
                    t.set_macro(SLOT_AD_FILTER_DEPTH, 0.0);
                    t.set_macro(SLOT_STRIP_CUT, cut);
                }
                e.trigger(0, 1.0);
                let mut l = [0.0f32; BLOCK];
                let mut r = [0.0f32; BLOCK];
                let mut peak = 0.0f32;
                for _ in 0..(0.2 * SAMPLE_RATE / BLOCK as f32) as usize {
                    e.process(&mut l, &mut r);
                    for &s in l.iter() {
                        peak = peak.max(s.abs());
                    }
                }
                peak
            };
            let wet = {
                let mut e = engine_box();
                for t in e.tracks.iter_mut() {
                    t.set_macro(SLOT_AD_FILTER_DEPTH, 0.8);
                    t.set_macro(SLOT_STRIP_CUT, cut);
                }
                e.trigger(0, 1.0);
                let mut l = [0.0f32; BLOCK];
                let mut r = [0.0f32; BLOCK];
                let mut peak = 0.0f32;
                for _ in 0..(0.2 * SAMPLE_RATE / BLOCK as f32) as usize {
                    e.process(&mut l, &mut r);
                    for &s in l.iter() {
                        peak = peak.max(s.abs());
                    }
                }
                peak
            };
            (dry - wet).abs()
        };

        // Filter closed: the route opens it and the level changes.
        assert!(
            moved(0.4) > 1.0e-3,
            "AD.FIL should work once RIP.CUT leaves room to open"
        );
        // Filter wide open: no headroom, so the route is legitimately inert.
        // This is a property of a unipolar envelope, not a broken route.
        assert_eq!(
            moved(1.0),
            0.0,
            "AD.FIL is bipolar now? If this changed, the doc on \
             SLOT_AD_FILTER_DEPTH is stale"
        );
    }

    /// The filter is neutral by default, so a voice is heard unfiltered. This
    /// is the regression guard for the 632 Hz default that made every engine
    /// sound like it was playing through a telephone. Paired with
    /// `output_never_exceeds_unity_even_with_the_filter_open`, which runs
    /// every track at full velocity through the now-wide-open strip.
    #[test]
    fn the_strip_is_neutral_by_default() {
        // Asserted against the cutoff the filter actually runs at, not the
        // one the macro nominally asks for. Those were different numbers
        // while the macro mapped to 20 kHz: the SVF clamped it to its
        // stability ceiling and the knob's claim was 1.6 octaves optimistic.
        for id in MiMachineId::ALL {
            let m = id.default_macros();
            let q = 0.5 + 19.5 * m[SLOT_RIPPLES_RESONANCE];
            let cut = MiSlot::ripples_cutoff_from_macro(m[SLOT_RIPPLES_CUTOFF], q);
            assert!(
                cut > 4_000.0,
                "{id:?} default RIP.CUT resolves to {cut} Hz — the strip is closed again"
            );
            // And it is the top of the knob's travel, not a point part-way up
            // with a dead zone above it.
            approx::assert_abs_diff_eq!(cut, stability_ceiling_hz(q, SAMPLE_RATE), epsilon = 1.0);
        }
    }

    /// `RIP.CUT` explicitly wide open, as a companion to
    /// `output_never_exceeds_unity` (which resets the strip and so does not
    /// exercise the mi-drum filter at all). mi-drum sets `strip_bypass`, so
    /// this is the only place the strip's SVF meets a full-velocity bus.
    #[test]
    fn output_never_exceeds_unity_even_with_the_filter_open() {
        // 1 ulp at 1.0 is 1.19e-7. Allow the documented approximation error.
        const TOL: f32 = 4.0 * 1.19e-7;
        let mut e = engine_box();
        for t in e.tracks.iter_mut() {
            t.set_macro(SLOT_RIPPLES_CUTOFF, 1.0);
        }
        let mut l = [0.0f32; BLOCK];
        let mut r = [0.0f32; BLOCK];
        for i in 0..200 {
            for trk in 0..TRACKS {
                e.trigger(trk, 1.0);
            }
            e.process(&mut l, &mut r);
            for &s in l.iter().chain(r.iter()) {
                assert!(s.is_finite(), "non-finite sample on block {i}");
                assert!(
                    s.abs() <= 1.0 + TOL,
                    "clipper let {s} through on block {i} — more than the \
                     approximation's own error, so this is a real overflow"
                );
            }
        }
    }
    /// The drive remap, which is the whole reason `WARP.DRV` is not passed
    /// straight to Warps. Pinned because getting it wrong is silent: a drive
    /// of 0 mutes the track, and a curve that is still linear at the top wastes
    /// half the knob.
    #[test]
    fn warps_drive_macro_maps_clean_to_destroyed() {
        // 0.0 is a bypass, and *not* a zero drive — that would be silence.
        let (bypass, drive) = warps_drive_from_macro(0.0);
        assert!(bypass, "WARP.DRV 0 must bypass Warps, not mute it");
        assert_eq!(drive, 0.0);

        // Monotonic and bounded above the floor.
        let mut prev = -1.0;
        for i in 1..=100 {
            let m = i as f32 / 100.0;
            let (bypass, d) = warps_drive_from_macro(m);
            assert!(!bypass, "WARP.DRV {m} should not bypass");
            assert!(d > prev, "drive must increase with the knob: {d} <= {prev}");
            assert!(
                (0.50..=1.00).contains(&d),
                "drive {d} out of range at macro {m}"
            );
            prev = d;
        }

        // The knob floor is the quietest unity point: Warps pre-gain 0.39.
        let (_, floor) = warps_drive_from_macro(f32::MIN_POSITIVE);
        assert!(
            (floor - 0.50).abs() < 1e-3,
            "the bottom of the knob maps to {floor}, not 0.50"
        );

        // The top of the knob is Warps at full drive — the destructive end has
        // to be reachable, not something shy of it.
        let (_, top) = warps_drive_from_macro(1.0);
        assert!((top - 1.00).abs() < 1e-6, "top maps to {top}");

        // Out-of-range macro values are clamped, not trusted.
        assert_eq!(warps_drive_from_macro(-1.0), (true, 0.0));
        assert_eq!(warps_drive_from_macro(2.0).1, 1.0);
    }

    /// A bypassed strip must be audibly transparent, and a driven one must not
    /// be. Without this, "clean" and "silent" are indistinguishable in a render
    /// and the knob could quietly regress to either.
    #[test]
    fn warps_bypass_is_a_real_setting() {
        let peak_at = |drive: f32| {
            let mut e = engine_box();
            e.tracks_mut()[0].load_machine(MiMachineId::SixOp1);
            e.tracks_mut()[0].set_macro(SLOT_WARPS_DRIVE, drive);
            e.trigger(0, 1.0);
            let mut l = [0.0f32; BLOCK];
            let mut r = [0.0f32; BLOCK];
            let mut peak = 0.0f32;
            for _ in 0..64 {
                e.process(&mut l, &mut r);
                for &s in l.iter().chain(r.iter()) {
                    peak = peak.max(s.abs());
                }
            }
            peak
        };

        let clean = peak_at(0.0);
        assert!(
            clean > 1.0e-3,
            "WARP.DRV 0 is silent ({clean}) — the bypass is not working, or \
             Warps is still being handed a zero drive"
        );

        // The default has to be off the bypass, or the shipped kit never
        // touches Warps at all.
        let default_peak = peak_at(MiMachineId::SixOp1.default_macros()[SLOT_WARPS_DRIVE]);
        assert!(
            default_peak > clean * 0.5,
            "the default drive is inaudible: {default_peak} vs clean {clean}"
        );

        // And the destructive end must actually reach the instrument.
        let hot = peak_at(1.0);
        assert!(hot > 1.0e-3, "WARP.DRV 1.0 is silent: {hot}");
    }

    #[test]
    fn extreme_modulation_macros_stay_finite() {
        let slots = [
            SLOT_LFO_RATE,
            SLOT_LFO_DEPTH,
            SLOT_AD_ATTACK,
            SLOT_AD_DECAY,
            SLOT_LFO_FILTER_DEPTH,
            SLOT_LFO_WARPS_DEPTH,
            SLOT_AD_FILTER_DEPTH,
            SLOT_AD_WARPS_DEPTH,
        ];
        for &slot in &slots {
            for &v in &[0.0f32, 1.0] {
                let mut e = engine_box();
                for t in e.tracks.iter_mut() {
                    t.set_macro(slot, v);
                }
                e.trigger(0, 1.0);
                let mut l = [0.0f32; BLOCK];
                let mut r = [0.0f32; BLOCK];
                for _ in 0..20 {
                    e.process(&mut l, &mut r);
                    for &s in l.iter().chain(r.iter()) {
                        assert!(
                            s.is_finite() && s.abs() <= 1.0,
                            "slot {slot} at {v} produced {s}"
                        );
                    }
                }
            }
        }
    }

    /// Every oscillator shape must be reachable and must change the sound,
    /// with `WARP.IN` on the oscillator.
    ///
    /// Replaces `every_warps_carrier_is_live`. The same five shapes used to
    /// sit on Warps' *carrier* input, where they replaced the voice; they are
    /// on the modulator input now, where they cross-modulate it.
    #[test]
    fn every_warps_oscillator_shape_is_live() {
        let base = [
            (SLOT_WARPS_MIX, 1.0f32),
            (SLOT_WARPS_DRIVE, 0.5),
            (SLOT_WARPS_ALGO, 0.25), // ring mod, so the modulator is audible
            (SLOT_WARPS_MOD_SRC, 1.0), // oscillator
        ];
        let render_shape = |shape: f32| {
            let mut m = base.to_vec();
            m.push((SLOT_WARPS_OSC_SHAPE, shape));
            render_with_macros(MiMachineId::VirtualAnalog, &m)
        };

        let mut rendered = std::vec::Vec::new();
        for (i, v) in [0.0f32, 0.25, 0.45, 0.65, 0.95].iter().enumerate() {
            let out = render_shape(*v);
            let peak = out.iter().fold(0.0f32, |a, &s| a.max(s.abs()));
            assert!(
                peak > 0.01,
                "shape {i} (macro {v}) was silent (peak {peak})"
            );
            for &s in out.iter() {
                assert!(s.is_finite(), "shape {i} produced {s}");
            }
            rendered.push(out);
        }
        for i in 1..rendered.len() {
            let delta = rendered[0]
                .iter()
                .zip(rendered[i].iter())
                .map(|(a, b)| (a - b).abs())
                .fold(0.0f32, f32::max);
            assert!(
                delta > 1.0e-4,
                "shape {i} is indistinguishable from the sine (max delta {delta})"
            );
        }
    }

    /// The oscillator is the only modulator source a Peaks voice can use, and
    /// that is most of why it exists: four of the six default tracks are
    /// Peaks, they have no aux output, and without this they are stuck
    /// cross-modulating against themselves.
    #[test]
    fn a_peaks_track_can_reach_a_real_modulator() {
        let base = [
            (SLOT_WARPS_MIX, 1.0f32),
            (SLOT_WARPS_DRIVE, 0.5),
            (SLOT_WARPS_ALGO, 0.25),
        ];
        let with_src = |src: f32| {
            let mut m = base.to_vec();
            m.push((SLOT_WARPS_MOD_SRC, src));
            render_with_macros(MiMachineId::PeaksBassDrum, &m)
        };

        let selfmod = with_src(0.0);
        let aux = with_src(0.5);
        let osc = with_src(1.0);

        let delta = |a: &[f32], b: &[f32]| {
            a.iter()
                .zip(b.iter())
                .map(|(x, y)| (x - y).abs())
                .fold(0.0f32, f32::max)
        };

        // Peaks has no aux, so that position has to fall back to self rather
        // than cross-modulate the voice against silence and mute the track.
        assert_eq!(
            delta(&selfmod, &aux),
            0.0,
            "WARP.IN aux moved a Peaks track, which has no aux to move to"
        );
        assert!(
            delta(&selfmod, &osc) > 1.0e-3,
            "the oscillator is not reaching a Peaks track's modulator input"
        );
        let peak = osc.iter().fold(0.0f32, |a, &s| a.max(s.abs()));
        assert!(peak > 1.0e-3, "the Peaks track went silent (peak {peak})");
    }

    /// Every Peaks model must be selectable, sound, and come to rest. Peaks
    /// needs its own silence gate, so this is also the test that would catch a
    /// regression there -- a track that never goes idle is a leak.
    #[test]
    fn every_peaks_model_sounds_and_decays() {
        for &id in &MiMachineId::PEAKS {
            let mut e = engine_box();
            e.tracks_mut()[0].load_machine(id);
            assert_eq!(e.tracks_mut()[0].id(), id, "machine did not load");
            e.trigger(0, 1.0);
            let mut l = [0.0f32; BLOCK];
            let mut r = [0.0f32; BLOCK];
            let mut peak = 0.0f32;
            for _ in 0..8 {
                e.process(&mut l, &mut r);
                for &s in l.iter().chain(r.iter()) {
                    assert!(s.is_finite(), "{id:?} produced {s}");
                    peak = peak.max(s.abs());
                }
            }
            // The threshold is low because one model deserves it: Peaks'
            // `HighHat::Configure` is empty upstream, so the hi-hat runs on
            // `Init()`'s fixed defaults and peaks around 0.022 raw, where the
            // bass drum reaches ~0.5. It is audible, and the track `LEVEL`
            // fader is what balances it -- a per-model output trim was
            // deliberately not added, see docs/peaks-vendoring.md.
            assert!(peak > 5.0e-3, "{id:?} was silent (peak {peak})");

            // Five seconds is far longer than any of these decays.
            for _ in 0..(5.0 * SAMPLE_RATE / BLOCK as f32) as usize {
                e.process(&mut l, &mut r);
            }
            assert!(!e.is_active(), "{id:?} never came to rest");
        }
    }

    /// A Peaks track must be silent until it is struck, like every other slot.
    #[test]
    fn peaks_is_silent_until_struck() {
        for &id in &MiMachineId::PEAKS {
            let mut e = engine_box();
            e.tracks_mut()[0].load_machine(id);
            let mut l = [0.0f32; BLOCK];
            let mut r = [0.0f32; BLOCK];
            for _ in 0..16 {
                e.process(&mut l, &mut r);
                for &s in l.iter().chain(r.iter()) {
                    assert_eq!(s, 0.0, "{id:?} made sound before being struck");
                }
            }
        }
    }

    // ----- the gate -----

    /// Peak of the output over `blocks` blocks, starting `skip_blocks` in.
    fn peak_from(engine: &mut MiDrumEngine, skip_blocks: usize, blocks: usize) -> f32 {
        let mut l = [0.0f32; BLOCK];
        let mut r = [0.0f32; BLOCK];
        for _ in 0..skip_blocks {
            engine.process(&mut l, &mut r);
        }
        let mut peak = 0.0f32;
        for _ in 0..blocks {
            engine.process(&mut l, &mut r);
            for &s in l.iter().chain(r.iter()) {
                peak = peak.max(s.abs());
            }
        }
        peak
    }

    /// Below this the output is inaudible: -80 dBFS.
    const INAUDIBLE: f32 = 1.0e-4;

    fn blocks_for(seconds: f32) -> usize {
        (seconds * SAMPLE_RATE / BLOCK as f32) as usize
    }

    /// The headline capability: a voice holds for as long as the gate is open.
    ///
    /// Before the gate existed, `MiSlot` set the Plaits trigger high for one
    /// 24-sample block and low for the rest of the note, so nothing could
    /// sustain at all. `SixOp1` is the sharpest witness because it reads the
    /// gate as a *level* and drives its FM operator envelopes from it.
    #[test]
    fn a_held_gate_sustains_the_voice() {
        for &id in &[MiMachineId::SixOp1, MiMachineId::SixOp3] {
            let mut e = engine_box();
            e.tracks_mut()[0].load_machine(id);
            e.trigger(0, 1.0);

            // Audible at the start...
            let onset = peak_from(&mut e, 0, blocks_for(0.05));
            assert!(onset > INAUDIBLE, "{id:?} was silent on note-on: {onset}");

            // ...and still audible four seconds later, with the gate never
            // closed. A pulsing gate dies here.
            let held = peak_from(&mut e, 0, blocks_for(4.0));
            assert!(
                held > INAUDIBLE,
                "{id:?} fell silent while the gate was held: {held}"
            );
            assert!(e.is_active(), "{id:?} went idle while the gate was held");
        }
    }

    /// A note-off starts the engine's release. Asserted as a drop in level
    /// rather than as silence: the release tail belongs to the engine, and
    /// `SixOp1` in particular keeps ringing for several seconds at a low
    /// level. What the gate guarantees is that the note stops being *held*,
    /// not that it stops instantly.
    #[test]
    fn a_release_stops_the_note_being_held() {
        for &id in &[
            MiMachineId::SixOp1,
            MiMachineId::SixOp3,
            MiMachineId::VirtualAnalog,
        ] {
            let mut e = engine_box();
            e.tracks_mut()[0].load_machine(id);
            e.trigger(0, 1.0);

            let held = peak_from(&mut e, 0, blocks_for(1.0));
            assert!(held > INAUDIBLE, "{id:?} was silent while held: {held}");

            e.release(0);

            // A second of holding, measured long after the release has had
            // time to act. Without a gate close this would equal `held`.
            let after = peak_from(&mut e, blocks_for(3.0), blocks_for(1.0));
            assert!(
                after < held * 0.1,
                "{id:?} is still being held after the release: {after} vs {held}"
            );
        }
    }

    /// A drum-grid trigger sends no note-off, and a drum machine must still
    /// come to rest. The gate is held for the note, so it has to end when the
    /// voice does rather than waiting for a key that will never come up.
    #[test]
    fn a_drum_hit_still_comes_to_rest_without_a_note_off() {
        for &id in &MiMachineId::PEAKS {
            let mut e = engine_box();
            e.tracks_mut()[0].load_machine(id);
            e.trigger(0, 1.0);
            for _ in 0..blocks_for(5.0) {
                let mut l = [0.0f32; BLOCK];
                let mut r = [0.0f32; BLOCK];
                e.process(&mut l, &mut r);
            }
            assert!(
                !e.is_active(),
                "{id:?} held its gate open with no note-off — a drum trigger never sends one"
            );
        }
    }

    /// The watchdog. A stuck key on a voice that sustains holds the gate high,
    /// which is what keeps a track at full DSP cost — `is_active` is the
    /// engine's per-track early-out. Asserted as the held level collapsing,
    /// since what the watchdog ends is the *hold*; whether the track then goes
    /// idle is the engine's own tail to decide.
    #[test]
    fn watchdog_stops_holding_a_gate_that_is_never_released() {
        let mut e = engine_box();
        e.tracks_mut()[0].load_machine(MiMachineId::SixOp1);
        e.trigger(0, 1.0);

        // Well inside the watchdog, still held.
        let held = peak_from(&mut e, blocks_for(2.0), blocks_for(1.0));
        assert!(held > 0.1, "{held} — expected a held note at 2 s");

        // Past it, with no note-off ever sent.
        let after = peak_from(&mut e, GATED_MAX_HOLD_BLOCKS as usize * 4, blocks_for(1.0));
        assert!(
            after < held * 0.1,
            "the gate watchdog never fired — a stuck key would cost full price forever: \
             {after} vs {held}"
        );
    }

    /// The regression this whole change is for. `SixOp1`/`2`/`3` read the
    /// Plaits gate as a *level*, not an edge, and feed it straight into FM
    /// operator envelopes that rest at exactly zero. With the old one-block
    /// pulse the note was over before the envelope rose, so all three emitted
    /// digital silence for the whole hit — three of 28 machines missing from
    /// the rendered baseline.
    #[test]
    fn six_op_engines_are_no_longer_silent() {
        for &id in &[
            MiMachineId::SixOp1,
            MiMachineId::SixOp2,
            MiMachineId::SixOp3,
        ] {
            let mut e = engine_box();
            e.tracks_mut()[0].load_machine(id);
            let peak = render_peak(&mut e, 0, 32);
            assert!(
                peak > 1.0e-3,
                "{id:?} is silent — the gate is a pulse again (peak {peak})"
            );
        }
    }

    /// Every catalogued engine must make sound. This is the assertion the
    /// rendered baseline could not make while the gate was pulsing: three of
    /// the 28 hit windows came back at exactly -inf.
    #[test]
    fn every_catalogued_engine_makes_sound() {
        for &id in &MiMachineId::ALL {
            let mut e = engine_box();
            e.tracks_mut()[0].load_machine(id);
            let peak = render_peak(&mut e, 0, 32);
            assert!(
                peak > 1.0e-5,
                "{id:?} ({}) is silent (peak {peak})",
                id.name()
            );
        }
    }

    /// Note-offs are not reliably paired. A release for a note that never
    /// started must make no sound, and repeat releases must leave the decay
    /// trajectory bit-identical to a run that released exactly once.
    #[test]
    fn stray_releases_are_inert() {
        let mut l = [0.0f32; BLOCK];
        let mut r = [0.0f32; BLOCK];

        // Never triggered.
        let mut e = engine_box();
        e.tracks_mut()[0].load_machine(MiMachineId::SixOp1);
        e.release(0);
        for _ in 0..8 {
            e.process(&mut l, &mut r);
        }
        for &s in l.iter().chain(r.iter()) {
            assert_eq!(s, 0.0, "a stray release made sound");
        }

        // One release versus three, on otherwise identical engines. Anything
        // other than bit-identical output means a repeat release is doing
        // something.
        let mut render_with_releases = |n: usize| {
            let mut e = engine_box();
            e.tracks_mut()[0].load_machine(MiMachineId::SixOp1);
            e.trigger(0, 1.0);
            for _ in 0..32 {
                e.process(&mut l, &mut r);
            }
            let mut out = [0.0f32; 8 * BLOCK];
            for blk in out.chunks_mut(BLOCK) {
                e.process(&mut l, &mut r);
                blk.copy_from_slice(&l);
            }
            for _ in 0..n {
                e.release(0);
            }
            // Re-render after the releases.
            for blk in out.chunks_mut(BLOCK) {
                e.process(&mut l, &mut r);
                blk.copy_from_slice(&l);
            }
            out
        };
        let once = render_with_releases(1);
        let thrice = render_with_releases(3);
        for (i, (a, b)) in once.iter().zip(thrice.iter()).enumerate() {
            assert_eq!(
                a.to_bits(),
                b.to_bits(),
                "a stray release changed the output at {i}: {a} vs {b}"
            );
        }
    }

    /// Releasing one track must not disturb another. A gate is per track, and
    /// a bug that shared the state would make a note-off on one voice cut
    /// another.
    #[test]
    fn release_is_per_track() {
        let mut e = engine_box();
        // Tracks 4 and 5 are the two Plaits voices in the default kit.
        e.tracks_mut()[4].load_machine(MiMachineId::SixOp1);
        e.tracks_mut()[5].load_machine(MiMachineId::SixOp3);
        e.trigger(4, 1.0);
        e.trigger(5, 1.0);
        for _ in 0..32 {
            let mut l = [0.0f32; BLOCK];
            let mut r = [0.0f32; BLOCK];
            e.process(&mut l, &mut r);
        }

        e.release(4);
        // Four seconds for track 4's release to run out while 5 stays held.
        let after = peak_from(&mut e, blocks_for(4.0), blocks_for(1.0));
        assert!(
            after > INAUDIBLE,
            "releasing track 4 also silenced track 5: {after}"
        );
        assert!(e.tracks()[5].is_active(), "track 5 must still be held");
    }

    /// Peaks models take their four parameters from MACH 1..4, and turning
    /// them must change the sound. Guards against the machine being loadable
    /// but inert.
    #[test]
    fn peaks_parameters_are_live() {
        let mut dry = engine_box();
        dry.tracks_mut()[0].load_machine(MiMachineId::PeaksBassDrum);
        dry.trigger(0, 1.0);
        let mut l = [0.0f32; BLOCK];
        let mut r = [0.0f32; BLOCK];
        const BLOCKS: usize = 20;
        let mut a = [0.0f32; BLOCK * BLOCKS];
        for i in 0..BLOCK * BLOCKS {
            dry.process(&mut l, &mut r);
            a[i] = l[i % BLOCK];
        }

        let mut wet = engine_box();
        wet.tracks_mut()[0].load_machine(MiMachineId::PeaksBassDrum);
        // MACH 3 is the bass drum's decay; push it long.
        wet.tracks_mut()[0].set_macro(SLOT_MACH_3, 0.95);
        wet.trigger(0, 1.0);
        let mut b = [0.0f32; BLOCK * BLOCKS];
        for i in 0..BLOCK * BLOCKS {
            wet.process(&mut l, &mut r);
            b[i] = l[i % BLOCK];
        }

        let delta = a
            .iter()
            .zip(b.iter())
            .map(|(x, y)| (x - y).abs())
            .fold(0.0f32, f32::max);
        assert!(
            delta > 1.0e-3,
            "Peaks parameters are inert (max delta {delta})"
        );
    }

    /// Documented, deliberate: Peaks has no velocity input, so a soft hit and a
    /// hard hit sound the same. This pins that as a known gap so it cannot be
    /// mistaken for a bug -- see docs/peaks-vendoring.md for why it is
    /// deliberately not "fixed" by scaling velocity onto the output.
    #[test]
    fn peaks_velocity_is_intentionally_ignored() {
        let peak_for = |velocity: f32| {
            let mut e = engine_box();
            e.tracks_mut()[0].load_machine(MiMachineId::PeaksBassDrum);
            e.trigger(0, velocity);
            let mut l = [0.0f32; BLOCK];
            let mut r = [0.0f32; BLOCK];
            let mut peak = 0.0f32;
            for _ in 0..8 {
                e.process(&mut l, &mut r);
                for &s in l.iter() {
                    peak = peak.max(s.abs());
                }
            }
            peak
        };
        let soft = peak_for(0.15);
        let hard = peak_for(1.0);
        assert!(soft > 0.01, "the soft hit should still be audible");
        assert_eq!(
            soft, hard,
            "Peaks velocity is deliberately ignored -- if this now differs, \
             velocity has been wired up and docs/peaks-vendoring.md needs updating"
        );
    }

    /// Switching a track between the two voice families must work in both
    /// directions, since the machine selector spans all 28 machines.
    #[test]
    fn tracks_switch_voice_type() {
        let mut e = engine_box();
        for &id in &[
            MiMachineId::PeaksBassDrum,
            MiMachineId::String,
            MiMachineId::PeaksSnareDrum,
            MiMachineId::Modal,
        ] {
            e.tracks_mut()[0].load_machine(id);
            assert_eq!(e.tracks_mut()[0].id(), id);
            e.trigger(0, 1.0);
            let mut l = [0.0f32; BLOCK];
            let mut r = [0.0f32; BLOCK];
            let mut peak = 0.0f32;
            for _ in 0..8 {
                e.process(&mut l, &mut r);
                for &s in l.iter() {
                    assert!(s.is_finite(), "{id:?} produced {s}");
                    peak = peak.max(s.abs());
                }
            }
            assert!(peak > 5.0e-3, "{id:?} silent after a voice-type switch");
        }
    }

    #[test]
    fn engine_size_fits_ocram_budget() {
        let sz = core::mem::size_of::<MiDrumEngine>();
        std::println!("MiDrumEngine size = {sz}");
        // The Teensy 4.1 OCRAM is 512 KB. Plaits voices are large; assert we
        // stay under a 500 KB cap so the static engine plus other statics fit.
        assert!(
            sz < 500_000,
            "MiDrumEngine is {sz} bytes — too large for OCRAM .uninit placement"
        );
    }
}
