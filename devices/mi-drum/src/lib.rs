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

pub use device_core::macros::{
    SLOT_FILT_0, SLOT_FILT_1, SLOT_STRIP_ATK, SLOT_STRIP_CUT, SLOT_STRIP_DEC, SLOT_STRIP_HOLD,
    SLOT_STRIP_RESO,
};

use device_core::dsp::Svf;
use device_core::macros::{
    macro_index, mi, resv, MacroInfo, BANK_FILT, BANK_MOD, MACH_INFO, NUM_MACROS, OUT_INFO,
    PAN_INFO, SEND_DLY_INFO, SEND_RVB_INFO, SLOT_LEVEL, SLOT_MACHINE, SLOT_MACH_0, SLOT_MACH_1,
    SLOT_MACH_2, SLOT_MACH_3, SLOT_MACH_4, SLOT_MACH_5, SLOT_MACH_6, SLOT_MACH_7, SLOT_OUT,
    SLOT_PAN, SLOT_SEND_DELAY, SLOT_SEND_REVERB,
};
use mi_dsp::plaits::{MiPlaitsModulations, MiPlaitsPatch, PlaitsVoice};
use mi_dsp::stages::{Stages as ModStages, GATE_LOW, GATE_RISING, SEGMENT_ALT};
use mi_dsp::warps::{Carrier, Warps, MAX_BLOCK as WARPS_MAX_BLOCK};

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

        // Phase 14 fixed strip defaults.
        m[SLOT_WARPS_ALGO] = 0.0;
        m[SLOT_WARPS_TIMBRE] = 0.5;
        m[SLOT_WARPS_DRIVE] = 0.7; // Warps input VCA; 0 is silent
        m[SLOT_WARPS_CARRIER] = 0.0; // external cross-modulation
        m[SLOT_RIPPLES_CUTOFF] = 0.5; // ~1 kHz
        m[SLOT_RIPPLES_RESONANCE] = 0.01; // gentle Q
        m[SLOT_RIPPLES_FM] = 0.0;

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
/// FILT 6: Warps carrier source. 0 = the input cross-modulates itself (the
/// pre-14.4 behaviour); the rest select Warps' internal sine/triangle/saw/
/// pulse/noise oscillators, pitched from the voice's own note.
pub const SLOT_WARPS_CARRIER: usize = macro_index(BANK_FILT, 6);
const SLOT_WARPS_DRIVE: usize = SLOT_STRIP_HOLD;
const SLOT_RIPPLES_CUTOFF: usize = SLOT_STRIP_CUT;
const SLOT_RIPPLES_RESONANCE: usize = SLOT_STRIP_RESO;
const SLOT_RIPPLES_FM: usize = SLOT_STRIP_ATK;

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
pub const SLOT_AD_FILTER_DEPTH: usize = macro_index(BANK_MOD, 6);
/// MOD 7: AD env 2 depth into the Warps timbre parameter.
pub const SLOT_AD_WARPS_DEPTH: usize = macro_index(BANK_MOD, 7);

const WARPS_ALGO_INFO: MacroInfo = mi("WARP.ALG", "WAL", 0.0);
const WARPS_TIMBRE_INFO: MacroInfo = mi("WARP.TIM", "WTM", 0.5);
const WARPS_CARRIER_INFO: MacroInfo = mi("WARP.CAR", "WCA", 0.0);
const WARPS_DRIVE_INFO: MacroInfo = mi("WARP.DRV", "WDR", 0.7);
const RIPPLES_CUTOFF_INFO: MacroInfo = mi("RIP.CUT", "RCT", 0.5);
const RIPPLES_RESONANCE_INFO: MacroInfo = mi("RIP.RES", "RRS", 0.5);
const RIPPLES_FM_INFO: MacroInfo = mi("RIP.FM", "RFM", 0.0);
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
    WARPS_CARRIER_INFO,       // FILT 6 — Warps carrier source
    resv(),                   // FILT 7
    MACH_INFO,                // TRACK 0
    OUT_INFO,                 // TRACK 1
    PAN_INFO,                 // TRACK 2
    mi("LEVEL", "LVL", 0.85), // TRACK 3
    SEND_DLY_INFO,            // TRACK 4
    SEND_RVB_INFO,            // TRACK 5
    resv(),                   // TRACK 6
    resv(),                   // TRACK 7
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
    // Phase 14 fixed-strip modules.
    warps: Warps,
    warps_carrier: Carrier,
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
    warps_drive: f32,
    ripples_cutoff_hz: f32,
    ripples_reso_q: f32,
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
    fn carrier_from_macro(v: f32) -> Carrier {
        match (v * 6.0) as u32 {
            0 => Carrier::External,
            1 => Carrier::Sine,
            2 => Carrier::Triangle,
            3 => Carrier::Saw,
            4 => Carrier::Pulse,
            _ => Carrier::NoiseLp,
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

    /// Render the next block if the buffer is exhausted, honouring any pending
    /// trigger, and update the active/silence state.
    fn render_if_needed(&mut self) {
        if self.block_pos < VOICE_BLOCK {
            return;
        }

        self.modulations.trigger = if self.trigger_pending {
            self.trigger_pending = false;
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
            warps: Warps::new(SAMPLE_RATE),
            warps_carrier: Carrier::External,
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
            ripples_cutoff_hz: 1000.0,
            ripples_reso_q: 0.707,
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
            core::ptr::addr_of_mut!((*ptr).warps).write(Warps::new(SAMPLE_RATE));
            core::ptr::addr_of_mut!((*ptr).warps_carrier).write(Carrier::External);
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
            core::ptr::addr_of_mut!((*ptr).ripples_cutoff_hz).write(1000.0);
            core::ptr::addr_of_mut!((*ptr).ripples_reso_q).write(0.707);
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

        self.warps_algorithm = macros[SLOT_WARPS_ALGO];
        self.warps_timbre = macros[SLOT_WARPS_TIMBRE];
        self.warps_drive = macros[SLOT_WARPS_DRIVE];
        self.warps_carrier = Self::carrier_from_macro(macros[SLOT_WARPS_CARRIER]);
        self.ripples_cutoff_hz = 20.0 * libm::powf(1000.0, macros[SLOT_RIPPLES_CUTOFF]);
        self.ripples_reso_q = 0.5 + 19.5 * macros[SLOT_RIPPLES_RESONANCE];
        self.ripples
            .recalc(self.ripples_cutoff_hz, self.ripples_reso_q, SAMPLE_RATE);

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

            // The internal carrier is pitched from the voice's own note, so
            // Warps tracks pitch instead of sitting on one fixed frequency. It
            // is ignored for `Carrier::External`.
            self.warps.set_parameters(
                self.warps_algorithm,
                modulated_timbre,
                self.warps_drive,
                self.warps_carrier,
                self.patch.note,
            );
            self.warps.process(chunk);
        }

        // Ripples multimode SVF after Warps, with per-sample cutoff modulation
        // from LFO 1 and AD envelope 1.
        let lfo_filter_scale = self.lfo_filter_depth * lfo_master * 3.0;
        let ad_filter_scale = self.ad_filter_depth * 3.0;
        let base_cutoff = self.ripples_cutoff_hz;

        for i in 0..n {
            let cutoff = (base_cutoff
                * libm::powf(
                    2.0,
                    self.lfo1_out[i] * lfo_filter_scale + self.env1_out[i] * ad_filter_scale,
                ))
            .clamp(20.0, 20000.0);
            self.ripples.set_cutoff(cutoff, SAMPLE_RATE);
            buf[i] = self.ripples.tick(buf[i]);
        }
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

    /// Extremes on every modulation macro must stay finite and bounded.
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

    /// Every Warps carrier source must be reachable and audible through the
    /// strip. `WARP.CAR` at 0 keeps the cross-modulator; the rest select Warps'
    /// internal oscillators, which the strip now pitches from the voice's note.
    #[test]
    fn every_warps_carrier_is_live() {
        let external = render_depth(SLOT_WARPS_CARRIER, 0.0);
        let mut prev_peak = 0.0f32;
        for (i, v) in [0.2f32, 0.4, 0.6, 0.8, 1.0].iter().enumerate() {
            let out = render_depth(SLOT_WARPS_CARRIER, *v);
            let peak = out.iter().fold(0.0f32, |a, &s| a.max(s.abs()));
            assert!(
                peak > 0.01,
                "carrier step {i} (macro {v}) was silent (peak {peak})"
            );
            for &s in &out {
                assert!(s.is_finite(), "carrier step {i} produced {s}");
            }
            let delta = external
                .iter()
                .zip(out.iter())
                .map(|(a, b)| (a - b).abs())
                .fold(0.0f32, f32::max);
            assert!(
                delta > 1.0e-4,
                "carrier step {i} (macro {v}) is dead (max delta {delta})"
            );
            assert!(
                (peak - prev_peak).abs() > 1.0e-4,
                "carrier step {i} (macro {v}) is inaudible in level (peak {peak})"
            );
            prev_peak = peak;
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
