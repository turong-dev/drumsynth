//! Mutable-Instruments-based drum device.
//!
//! Hosts Plaits macro-oscillator engines (and eventually Peaks drum models) as
//! per-track voices inside the generic [`device_core::Engine`] framework.
//! The C++ Plaits voice is block-rate, while `device_core::Slot` is
//! per-sample, so [`MiSlot`] buffers one rendered block and yields samples one
//! at a time.

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

pub use device_core::macros::SLOT_FILT_0;

use device_core::macros::{
    mi, resv, MacroInfo, LFO1_DEPTH_INFO, LFO1_DEST_INFO, LFO1_RATE_INFO, LFO2_DEPTH_INFO,
    LFO2_DEST_INFO, LFO2_RATE_INFO, MACH_INFO, NUM_MACROS, OUT_INFO, PAN_INFO, SEND_DLY_INFO,
    SEND_RVB_INFO, SLOT_LEVEL, SLOT_MACHINE, SLOT_MACH_0, SLOT_MACH_1, SLOT_MACH_2, SLOT_MACH_3,
    SLOT_MACH_4, SLOT_MACH_5, SLOT_MACH_6, SLOT_MACH_7, SLOT_OUT, SLOT_PAN,
    SLOT_SEND_DELAY,
    SLOT_SEND_REVERB, SLOT_STRIP_ATK, SLOT_STRIP_CUT, SLOT_STRIP_DEC, SLOT_STRIP_HOLD,
    SLOT_STRIP_RESO, STRIP_ATK_INFO, STRIP_CUT_INFO, STRIP_DEC_INFO, STRIP_HOLD_INFO,
    STRIP_RESO_INFO,
};
use mi_dsp::plaits::{MiPlaitsModulations, MiPlaitsPatch, PlaitsVoice};
use mi_dsp::spike_stages::{Lpg, Overdrive, Resonator};

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

/// How many tracks the engine owns.
pub const TRACKS: usize = 6;

/// Render block size used internally by [`MiSlot`]. Matches Plaits
/// `kMaxBlockSize` so every render call is one native block.
const VOICE_BLOCK: usize = 24;

/// Consider a block silent when every sample is below this magnitude.
const SILENCE_THRESHOLD: f32 = 1.0e-6;
/// Number of consecutive silent blocks before the slot declares itself idle.
const SILENCE_BLOCKS: u8 = 4;

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
}

impl MiMachineId {
    /// Number of machines currently catalogued.
    pub const COUNT: usize = 24;

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
        }
    }

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
        }
    }

    /// The Plaits engine index this machine maps to.
    const fn plaits_engine(self) -> i32 {
        self.index() as i32
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
        m[SLOT_MACH_0] = tune;
        m[SLOT_MACH_1] = 0.5;
        m[SLOT_MACH_2] = 0.5;
        m[SLOT_MACH_3] = 0.5;
        m[SLOT_MACH_4] = 0.0;
        m[SLOT_MACH_5] = 0.0;
        m[SLOT_MACH_6] = 0.0;
        m[SLOT_MACH_7] = decay;

        // Track-routed defaults.
        m[SLOT_MACHINE] = self.index() as f32 / ((Self::COUNT.saturating_sub(1).max(1)) as f32);
        m[SLOT_OUT] = 0.0;
        m[SLOT_PAN] = 0.5;
        m[SLOT_LEVEL] = 0.85;
        m[SLOT_SEND_DELAY] = 0.0;
        m[SLOT_SEND_REVERB] = 0.0;

        // Strip defaults.
        m[SLOT_STRIP_CUT] = 1.0;
        m[SLOT_STRIP_RESO] = 0.01;
        m[SLOT_STRIP_ATK] = 0.0;
        m[SLOT_STRIP_HOLD] = 1.0;
        m[SLOT_STRIP_DEC] = 1.0;

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
    mi("STAGE", "STG", 0.0),  // FILT 0 — Phase 14 spike stage selector
    resv(),                   // FILT 1
    STRIP_CUT_INFO,           // FILT 2
    STRIP_RESO_INFO,          // FILT 3
    STRIP_ATK_INFO,           // FILT 4
    STRIP_HOLD_INFO,          // FILT 5
    STRIP_DEC_INFO,           // FILT 6
    resv(),                   // FILT 7
    MACH_INFO,                // TRACK 0
    OUT_INFO,                 // TRACK 1
    PAN_INFO,                 // TRACK 2
    mi("LEVEL", "LVL", 0.85), // TRACK 3
    SEND_DLY_INFO,            // TRACK 4
    SEND_RVB_INFO,            // TRACK 5
    resv(),                   // TRACK 6
    resv(),                   // TRACK 7
    resv(),                   // MOD 0
    resv(),                   // MOD 1
    LFO1_RATE_INFO,           // MOD 2
    LFO1_DEPTH_INFO,          // MOD 3
    LFO1_DEST_INFO,           // MOD 4
    LFO2_RATE_INFO,           // MOD 5
    LFO2_DEPTH_INFO,          // MOD 6
    LFO2_DEST_INFO,           // MOD 7
];

/// Which MI processing stage runs on the voice's output.
///
/// **Phase 14 spike.** The eventual design puts stage selection on its own
/// macro slots, ahead of the track strip, with the strip's continuous knobs
/// reinterpreted per stage. This is the cheap version: one selector on
/// `SLOT_FILT_0`, applied inside `MiSlot` where a 24-sample block already
/// exists, purely to find out what the stages cost and sound like before
/// committing to the architecture. It is not the shape the phase ships in.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum StageKind {
    /// No stage. The voice's own output, unchanged.
    None,
    /// Buchla-style low-pass gate, the vactrol pair Plaits uses internally.
    Lpg,
    /// Gain-compensated soft-clip overdrive.
    Overdrive,
    /// 24-mode modal resonator: the voice becomes an exciter.
    Resonator,
    /// LPG followed by overdrive — two stages chained, to measure whether the
    /// Phase 14 chain (env + colour + drive) is affordable at all.
    LpgDrive,
}

impl StageKind {
    /// Quantise a 0..1 macro over the stage list. 0.0 is [`StageKind::None`],
    /// so an untouched sound is unchanged.
    fn from_macro(v: f32) -> Self {
        match (v * 5.0) as u32 {
            0 => Self::None,
            1 => Self::Lpg,
            2 => Self::Overdrive,
            3 => Self::Resonator,
            _ => Self::LpgDrive,
        }
    }
}

/// The MI stages a slot can run, all constructed up front.
///
/// Held as concrete fields rather than an enum of storages because the engine
/// lives in a `.uninit` static and is built by `new_in_place`; keeping the set
/// fixed means no discriminant to maintain and no re-init on stage change.
/// The spike pays for all three per voice — 2 KB of that is the resonator —
/// which is precisely the kind of cost the real design has to avoid.
struct Stages {
    lpg: Lpg,
    overdrive: Overdrive,
    resonator: Resonator,
    scratch: [f32; VOICE_BLOCK],
}

impl Stages {
    fn new() -> Self {
        Self {
            lpg: Lpg::new(),
            overdrive: Overdrive::new(),
            // Struck a third of the way along, all 24 modes. Both are
            // hardcoded for the spike; they are macro targets in the real
            // thing.
            resonator: Resonator::new(0.3, 24),
            scratch: [0.0f32; VOICE_BLOCK],
        }
    }

    /// Apply the selected stage to `buf` in place.
    fn process(&mut self, kind: StageKind, note: f32, buf: &mut [f32; VOICE_BLOCK]) {
        match kind {
            StageKind::None => {}
            StageKind::Lpg => {
                // Parameters in the region Plaits itself uses for a plucky
                // decay. Fixed for the spike.
                self.lpg.process(0.05, 0.5, 0.3, 0.2, buf);
            }
            StageKind::Overdrive => {
                // Not 0.0 — that mutes rather than passing dry. See the
                // measured table on `mi_dsp::stages::Overdrive`.
                self.overdrive.process(0.6, buf);
            }
            StageKind::LpgDrive => {
                self.lpg.process(0.05, 0.5, 0.3, 0.2, buf);
                self.overdrive.process(0.6, buf);
            }
            StageKind::Resonator => {
                // f0 as a fraction of the sample rate, from the voice's note
                // so the body tracks pitch.
                let hz = 440.0 * libm::exp2f((note - 69.0) / 12.0);
                let f0 = (hz * INV_SAMPLE_RATE).clamp(0.001, 0.4);
                self.resonator
                    .process(f0, 0.3, 0.5, 0.3, buf, &mut self.scratch);
                buf.copy_from_slice(&self.scratch);
            }
        }
    }
}

/// A block-buffered Plaits voice implementing the per-sample `Slot` trait.
///
/// `PlaitsVoice` renders 24-sample blocks; this slot feeds the core engine's
/// per-sample `tick()` by buffering one block and stepping through it.
pub struct MiSlot {
    voice: PlaitsVoice,
    block_out: [f32; VOICE_BLOCK],
    block_aux: [f32; VOICE_BLOCK],
    block_pos: usize,
    patch: MiPlaitsPatch,
    modulations: MiPlaitsModulations,
    trigger_pending: bool,
    active: bool,
    silence_counter: u8,
    tune_macro: f32,
    retune_semitones: f32,
    stage_kind: StageKind,
    stages: Stages,
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

    /// Render the next block if the buffer is exhausted, honouring any pending
    /// trigger, and update the active/silence state.
    fn render_if_needed(&mut self) {
        if self.block_pos < VOICE_BLOCK {
            return;
        }

        self.modulations.trigger = if self.trigger_pending {
            self.trigger_pending = false;
            if matches!(self.stage_kind, StageKind::Lpg | StageKind::LpgDrive) {
                self.stages.lpg.trigger();
            }
            1.0
        } else {
            0.0
        };

        self.voice.render_f32(
            &self.patch,
            &self.modulations,
            &mut self.block_out,
            &mut self.block_aux,
            VOICE_BLOCK,
        );

        // The stage runs here, on the voice's own block, before the silence
        // check — so a stage that gates or rings can decide when the note is
        // over rather than the raw voice doing it. This is also why the spike
        // needs no change in `core`: a block already exists at this point.
        self.stages
            .process(self.stage_kind, self.patch.note, &mut self.block_out);

        self.block_pos = 0;

        let mut peak = 0.0f32;
        for &s in &self.block_out {
            peak = peak.max(libm::fabsf(s));
        }
        if peak < SILENCE_THRESHOLD {
            self.silence_counter += 1;
            if self.silence_counter >= SILENCE_BLOCKS {
                self.active = false;
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
            voice: PlaitsVoice::new(),
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
            active: false,
            silence_counter: 0,
            tune_macro: 0.45,
            retune_semitones: 0.0,
            stage_kind: StageKind::None,
            stages: Stages::new(),
        };
        slot.set_macros(macros);
        slot
    }

    #[allow(unsafe_code)]
    unsafe fn new_in_place(id: Self::Id, macros: &[f32; NUM_MACROS], ptr: *mut Self) {
        unsafe {
            PlaitsVoice::new_in_place(core::ptr::addr_of_mut!((*ptr).voice));
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
            core::ptr::addr_of_mut!((*ptr).active).write(false);
            core::ptr::addr_of_mut!((*ptr).silence_counter).write(0);
            core::ptr::addr_of_mut!((*ptr).tune_macro).write(0.45);
            core::ptr::addr_of_mut!((*ptr).retune_semitones).write(0.0);
            core::ptr::addr_of_mut!((*ptr).stage_kind).write(StageKind::None);
            core::ptr::addr_of_mut!((*ptr).stages).write(Stages::new());
            (*ptr).set_macros(macros);
        }
    }

    fn id(&self) -> Self::Id {
        MiMachineId::from_index(self.patch.engine as usize).unwrap_or(MiMachineId::VirtualAnalog)
    }

    fn set_macros(&mut self, macros: &[f32; NUM_MACROS]) {
        self.tune_macro = macros[SLOT_MACH_0];
        self.update_note();
        self.patch.harmonics = macros[SLOT_MACH_1];
        self.patch.timbre = macros[SLOT_MACH_2];
        self.patch.morph = macros[SLOT_MACH_3];
        self.patch.frequency_modulation_amount = macros[SLOT_MACH_4];
        self.patch.timbre_modulation_amount = macros[SLOT_MACH_5];
        self.patch.morph_modulation_amount = macros[SLOT_MACH_6];
        self.patch.decay = macros[SLOT_MACH_7];
        self.stage_kind = StageKind::from_macro(macros[SLOT_FILT_0]);
    }

    fn trigger(&mut self, _velocity: f32) {
        self.trigger_pending = true;
        self.active = true;
        self.silence_counter = 0;
    }

    fn retune(&mut self, semis: f32) {
        self.retune_semitones = semis;
        self.update_note();
    }

    fn reset(&mut self) {
        self.trigger_pending = false;
        self.active = false;
        self.silence_counter = 0;
        self.block_pos = VOICE_BLOCK;
    }

    fn is_active(&self) -> bool {
        self.active || self.trigger_pending
    }

    fn tick(&mut self) -> f32 {
        if !self.is_active() {
            return 0.0;
        }
        self.render_if_needed();
        let s = self.block_out[self.block_pos];
        self.block_pos += 1;
        s
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
pub const DEFAULT_KIT: [MiMachineId; TRACKS] = [
    MiMachineId::BassDrum,  // 0: kick
    MiMachineId::SnareDrum, // 1: snare
    MiMachineId::HiHat,     // 2: closed hat
    MiMachineId::Modal,     // 3: tom/conga-like
    MiMachineId::Noise,     // 4: clap/noise
    MiMachineId::String,    // 5: melodic/string
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
        configure_default_notes(&mut e.note_map);
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
        configure_default_notes(&mut engine.note_map);
        engine
    }
}

/// Default GM-percussion note map.
fn configure_default_notes(map: &mut [Option<u8>; 128]) {
    map[36] = Some(0); // Acoustic bass drum → track 0 (BassDrum)
    map[38] = Some(1); // Acoustic snare → track 1 (SnareDrum)
    map[42] = Some(2); // Closed hat → track 2 (HiHat)
    map[46] = Some(3); // Open hat/tom → track 3 (Modal)
    map[39] = Some(4); // Hand clap → track 4 (Noise)
    map[50] = Some(5); // High tom → track 5 (String)
}

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
    fn trigger_note_routes_kick_to_track_zero() {
        let mut e = engine_box();
        assert_eq!(e.trigger_note(36, 1.0), Some(0));
        assert!(e.is_active());
    }

    #[test]
    fn trigger_channel_routes_channel_to_track() {
        let mut e = engine_box();
        assert_eq!(e.trigger_channel(3, 60, 1.0), Some(3));
        assert!(e.tracks[3].is_active());
    }

    #[test]
    fn output_never_exceeds_unity() {
        let mut e = engine_box();
        let mut l = [0.0f32; BLOCK];
        let mut r = [0.0f32; BLOCK];
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
