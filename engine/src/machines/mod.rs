//! Drum machines.
//!
//! The Syntakt-architecture centerpiece. A *machine* is a named synthesis
//! model that owns its own DSP state and exposes a fixed set of
//! [`NUM_MACROS`] normalised (0..1) macro knobs. Each macro maps through a
//! machine-specific curve to internal coefficients (`set_macros`).
//!
//! A [`MachineSlot`] is an enum of all machines — the no-`alloc`, no-`dyn`
//! equivalent of a trait-object collection. The enum carries the largest
//! machine, but `tick` only touches the active variant. Adding a machine
//! means adding a module here, an arm on `MachineId`, an arm on
//! `MachineSlot`, and an entry in [`MACHINE_INFO`]. Nothing else changes.
//!
//! # Macros
//!
//! Eight macros is the right number because it lines up with MIDI CC and
//! with the Syntakt SYN-page layout. The 8th is conventionally an overdrive
//! amount; on the machines we ship here, that lives on the *track strip*
//! instead, freeing the slot for things that shape the synthesis itself.
//! The slot index order is stable across versions — firmware CC mappings
//! depend on it.

pub mod bd_classic;
pub mod bd_fm;
pub mod cb_classic;
pub mod cp;
pub mod cy_metallic;
pub mod hat_classic;
pub mod hh_basic;
pub mod rs;
pub mod sd_fm;
pub mod sd_natural;
pub mod sy_tone;
pub mod tom;

pub use bd_classic::BdClassic;
pub use bd_fm::BdFm;
pub use cb_classic::CbClassic;
pub use cp::Cp;
pub use cy_metallic::CyMetallic;
pub use hat_classic::HatClassic;
pub use hh_basic::HhBasic;
pub use rs::Rs;
pub use sd_fm::SdFm;
pub use sd_natural::SdNatural;
pub use sy_tone::SyTone;
pub use tom::Tom;

/// How many macro knobs every machine exposes.
///
/// Deliberately a `const` rather than per-machine so callers can size
/// arrays against the type, not against the largest variant.
pub const NUM_MACROS: usize = 8;

/// Stable index for a macro knob. Stored as `usize` in arrays `[f32;
/// NUM_MACROS]`, indexed by this enum so spread-by-name stays readable.
///
/// The variant order is stable across versions; firmware CC assignments
/// and host render flags depend on absolute indices staying put.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Macro {
    /// Macro knob slot 0.
    M0 = 0,
    /// Macro knob slot 1.
    M1 = 1,
    /// Macro knob slot 2.
    M2 = 2,
    /// Macro knob slot 3.
    M3 = 3,
    /// Macro knob slot 4.
    M4 = 4,
    /// Macro knob slot 5.
    M5 = 5,
    /// Macro knob slot 6.
    M6 = 6,
    /// Macro knob slot 7.
    M7 = 7,
}

impl Macro {
    /// All macros, in slot order.
    pub const ALL: [Self; 8] = [
        Self::M0,
        Self::M1,
        Self::M2,
        Self::M3,
        Self::M4,
        Self::M5,
        Self::M6,
        Self::M7,
    ];
}

/// Metadata for one macro knob.
///
/// `name` is the human label (`"TUNE"`); `abbrev` is the short firmware
/// display form (`"TUN"`). `default` is the canonical factory value used
/// by [`MachineId::default_macros`] — same value on host and target so a
/// rendered WAV lines up bit-for-bit against hardware.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct MacroInfo {
    /// Human label for display surfaces.
    pub name: &'static str,
    /// Short form for OLED / serial.
    pub abbrev: &'static str,
    /// Canonical factory macro value, used by [`MachineId::default_macros`].
    pub default: f32,
}

/// A named synthesis model.
///
/// Used as a discriminator to build the matching [`MachineSlot`] variant and
/// to look up display metadata in [`MACHINE_INFO`]. Cheap to copy around.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum MachineId {
    /// Pitch-swept sine kick with drive — the classic analogue-style body.
    BdClassic,
    /// 2-operator FM kick — punchier, more "electronic" attack than BD Classic.
    BdFm,
    /// Pitch-swept sine tom with an optional stick transient; clean (no drive).
    Tom,
    /// Osc-plus-filtered-noise snare: tom body and wire rattle in one voice.
    SdNatural,
    /// FM snare: 2-operator FM body plus highpassed-noise rattle.
    SdFm,
    /// Rimshot: two detuned oscillators plus a short noise tick (the "crack").
    Rs,
    /// Clap: multi-burst noise envelope layered with a short FM body.
    Cp,
    /// Bandfiltered-noise closed hat. Cheap, plumbing beat.
    HatClassic,
    /// Six-detuned-oscillator 808-style hat. Metallicher than Hat Classic.
    HhBasic,
    /// Ring-modulated metallic cymbal. Long, shimmering decay.
    CyMetallic,
    /// Two-oscillator cowbell: square pair through a bandpass.
    CbClassic,
    /// 2-operator FM tonal synth with modulator feedback.
    SyTone,
}

impl MachineId {
    /// Number of machines currently catalogued.
    pub const COUNT: usize = 12;

    /// All machines, in catalogue order. Renaming a machine is fine; the
    /// order is part of the binary layout (firmware maps CC slots against
    /// indices that pick from here).
    pub const ALL: [Self; Self::COUNT] = [
        Self::BdClassic,
        Self::BdFm,
        Self::Tom,
        Self::SdNatural,
        Self::SdFm,
        Self::Rs,
        Self::Cp,
        Self::HatClassic,
        Self::HhBasic,
        Self::CyMetallic,
        Self::CbClassic,
        Self::SyTone,
    ];

    /// Catalogue index of this machine.
    pub fn index(self) -> usize {
        self as usize
    }

    /// Short canonical name used in CLI `<machine>` args and logs.
    pub fn name(self) -> &'static str {
        match self {
            Self::BdClassic => "bd-classic",
            Self::BdFm => "bd-fm",
            Self::Tom => "tom",
            Self::SdNatural => "sd-natural",
            Self::SdFm => "sd-fm",
            Self::Rs => "rs",
            Self::Cp => "cp",
            Self::HatClassic => "hat-classic",
            Self::HhBasic => "hh-basic",
            Self::CyMetallic => "cy-metallic",
            Self::CbClassic => "cb-classic",
            Self::SyTone => "sy-tone",
        }
    }

    /// Human-readable label for display.
    pub fn label(self) -> &'static str {
        match self {
            Self::BdClassic => "BD Classic",
            Self::BdFm => "BD FM",
            Self::Tom => "Tom",
            Self::SdNatural => "SD Natural",
            Self::SdFm => "SD FM",
            Self::Rs => "RS",
            Self::Cp => "CP",
            Self::HatClassic => "Hat Classic",
            Self::HhBasic => "HH Basic",
            Self::CyMetallic => "CY Metallic",
            Self::CbClassic => "CB Classic",
            Self::SyTone => "SY Tone",
        }
    }

    /// Macros this machine exposes, with their default values.
    pub fn macros(self) -> [MacroInfo; NUM_MACROS] {
        MACHINE_INFO[self.index()].macros
    }

    /// Look up a macro's metadata by name. Case-insensitive, ASCII only.
    pub fn macro_by_name(self, name: &str) -> Option<(usize, MacroInfo)> {
        for (i, m) in self.macros().into_iter().enumerate() {
            // `eq_ignore_ascii_case` is core-only; `to_ascii_lowercase`
            // would allocate and break the no-alloc contract.
            if m.name.eq_ignore_ascii_case(name.trim()) {
                return Some((i, m));
            }
        }
        None
    }

    /// Factory macro values, packed into an array ready for
    /// `Track::set_macros`.
    pub fn default_macros(self) -> [f32; NUM_MACROS] {
        let mut out = [0.0f32; NUM_MACROS];
        for (i, m) in self.macros().into_iter().enumerate() {
            out[i] = m.default;
        }
        out
    }
}

/// Per-machine metadata, indexed by [`MachineId::index`].
struct MachineInfo {
    macros: [MacroInfo; NUM_MACROS],
}

static MACHINE_INFO: [MachineInfo; MachineId::COUNT] = [
    // 0: BD Classic
    MachineInfo {
        macros: [
            MacroInfo {
                name: "TUNE",
                abbrev: "TUN",
                default: 0.20,
            },
            MacroInfo {
                name: "SWEEP",
                abbrev: "SWP",
                default: 0.36,
            },
            MacroInfo {
                name: "SWP_T",
                abbrev: "SWT",
                default: 0.15,
            },
            MacroInfo {
                name: "DEC",
                abbrev: "DEC",
                default: 0.255,
            },
            MacroInfo {
                name: "DRIVE",
                abbrev: "DRV",
                default: 0.16,
            },
            MacroInfo {
                name: "LEVEL",
                abbrev: "LVL",
                default: 0.9,
            },
            MacroInfo {
                name: "WAVE",
                abbrev: "WAV",
                default: 0.0,
            },
            MacroInfo {
                name: "TRN",
                abbrev: "TRN",
                default: 0.0,
            },
        ],
    },
    // 1: BD FM
    MachineInfo {
        macros: [
            MacroInfo {
                name: "TUNE",
                abbrev: "TUN",
                default: 0.20,
            },
            MacroInfo {
                name: "SWEEP",
                abbrev: "SWP",
                default: 0.30,
            },
            MacroInfo {
                name: "SWP_T",
                abbrev: "SWT",
                default: 0.15,
            },
            MacroInfo {
                name: "DEC",
                abbrev: "DEC",
                default: 0.255,
            },
            MacroInfo {
                name: "MOD.HZ",
                abbrev: "MDH",
                default: 0.43,
            },
            MacroInfo {
                name: "MOD.DC",
                abbrev: "MDD",
                default: 0.15,
            },
            MacroInfo {
                name: "MOD.AMT",
                abbrev: "MDA",
                default: 0.35,
            },
            MacroInfo {
                name: "LEVEL",
                abbrev: "LVL",
                default: 0.9,
            },
        ],
    },
    // 2: Tom
    MachineInfo {
        macros: [
            MacroInfo {
                name: "TUNE",
                abbrev: "TUN",
                default: 0.35,
            },
            MacroInfo {
                name: "SWEEP",
                abbrev: "SWP",
                default: 0.40,
            },
            MacroInfo {
                name: "SWP_T",
                abbrev: "SWT",
                default: 0.40,
            },
            MacroInfo {
                name: "DEC",
                abbrev: "DEC",
                default: 0.40,
            },
            MacroInfo {
                name: "STICK",
                abbrev: "STK",
                default: 0.30,
            },
            MacroInfo {
                name: "LEVEL",
                abbrev: "LVL",
                default: 0.85,
            },
            MacroInfo {
                name: "WAVE",
                abbrev: "WAV",
                default: 0.0,
            },
            MacroInfo {
                name: "RESV",
                abbrev: "RSV",
                default: 0.0,
            },
        ],
    },
    // 3: SD Natural
    MachineInfo {
        macros: [
            MacroInfo {
                name: "TUNE",
                abbrev: "TUN",
                default: 0.28,
            },
            MacroInfo {
                name: "RATIO",
                abbrev: "RTO",
                default: 0.48,
            },
            MacroInfo {
                name: "BDEC",
                abbrev: "BDC",
                default: 0.13,
            },
            MacroInfo {
                name: "NDEC",
                abbrev: "NDC",
                default: 0.209,
            },
            MacroInfo {
                name: "HPF",
                abbrev: "HPF",
                default: 0.14,
            },
            MacroInfo {
                name: "NMIX",
                abbrev: "NM",
                default: 0.62,
            },
            MacroInfo {
                name: "LEVEL",
                abbrev: "LVL",
                default: 0.7,
            },
            MacroInfo {
                name: "RESV",
                abbrev: "RSV",
                default: 0.0,
            },
        ],
    },
    // 4: SD FM
    MachineInfo {
        macros: [
            MacroInfo {
                name: "TUNE",
                abbrev: "TUN",
                default: 0.28,
            },
            MacroInfo {
                name: "RAT",
                abbrev: "RAT",
                default: 0.33,
            },
            MacroInfo {
                name: "BDEC",
                abbrev: "BDC",
                default: 0.13,
            },
            MacroInfo {
                name: "NDEC",
                abbrev: "NDC",
                default: 0.209,
            },
            MacroInfo {
                name: "MENV",
                abbrev: "MEN",
                default: 0.15,
            },
            MacroInfo {
                name: "AMT",
                abbrev: "AMT",
                default: 0.30,
            },
            MacroInfo {
                name: "NMIX",
                abbrev: "NM",
                default: 0.62,
            },
            MacroInfo {
                name: "LEVEL",
                abbrev: "LVL",
                default: 0.7,
            },
        ],
    },
    // 5: RS (rimshot)
    MachineInfo {
        macros: [
            MacroInfo {
                name: "TUNE",
                abbrev: "TUN",
                default: 0.40,
            },
            MacroInfo {
                name: "DET",
                abbrev: "DET",
                default: 0.25,
            },
            MacroInfo {
                name: "DEC",
                abbrev: "DEC",
                default: 0.30,
            },
            MacroInfo {
                name: "NDEC",
                abbrev: "NDC",
                default: 0.30,
            },
            MacroInfo {
                name: "NLEV",
                abbrev: "NLV",
                default: 0.40,
            },
            MacroInfo {
                name: "HPF",
                abbrev: "HPF",
                default: 0.30,
            },
            MacroInfo {
                name: "LEVEL",
                abbrev: "LVL",
                default: 0.75,
            },
            MacroInfo {
                name: "RESV",
                abbrev: "RSV",
                default: 0.0,
            },
        ],
    },
    // 6: CP (clap)
    MachineInfo {
        macros: [
            MacroInfo {
                name: "TUNE",
                abbrev: "TUN",
                default: 0.30,
            },
            MacroInfo {
                name: "RATIO",
                abbrev: "RTO",
                default: 0.50,
            },
            MacroInfo {
                name: "BDEC",
                abbrev: "BDC",
                default: 0.20,
            },
            MacroInfo {
                name: "NDEC",
                abbrev: "NDC",
                default: 0.30,
            },
            MacroInfo {
                name: "HPF",
                abbrev: "HPF",
                default: 0.20,
            },
            MacroInfo {
                name: "LPF",
                abbrev: "LPF",
                default: 0.50,
            },
            MacroInfo {
                name: "BAL",
                abbrev: "BAL",
                default: 0.80,
            },
            MacroInfo {
                name: "LEVEL",
                abbrev: "LVL",
                default: 0.7,
            },
        ],
    },
    // 7: Hat Classic
    MachineInfo {
        macros: [
            MacroInfo {
                name: "DEC",
                abbrev: "DEC",
                default: 0.092,
            },
            MacroInfo {
                name: "HPF",
                abbrev: "HPF",
                default: 0.45,
            },
            MacroInfo {
                name: "LPF",
                abbrev: "LPF",
                default: 0.75,
            },
            MacroInfo {
                name: "LEVEL",
                abbrev: "LVL",
                default: 0.4,
            },
            MacroInfo {
                name: "RESV",
                abbrev: "RSV",
                default: 0.0,
            },
            MacroInfo {
                name: "RESV2",
                abbrev: "RS2",
                default: 0.0,
            },
            MacroInfo {
                name: "RESV3",
                abbrev: "RS3",
                default: 0.0,
            },
            MacroInfo {
                name: "RESV4",
                abbrev: "RS4",
                default: 0.0,
            },
        ],
    },
    // 8: HH Basic
    MachineInfo {
        macros: [
            MacroInfo {
                name: "TUNE",
                abbrev: "TUN",
                default: 0.30,
            },
            MacroInfo {
                name: "TONE",
                abbrev: "TON",
                default: 0.50,
            },
            MacroInfo {
                name: "TDEC",
                abbrev: "TDC",
                default: 0.30,
            },
            MacroInfo {
                name: "DEC",
                abbrev: "DEC",
                default: 0.092,
            },
            MacroInfo {
                name: "RST",
                abbrev: "RST",
                default: 1.0,
            },
            MacroInfo {
                name: "LEVEL",
                abbrev: "LVL",
                default: 0.4,
            },
            MacroInfo {
                name: "BPF",
                abbrev: "BPF",
                default: 0.50,
            },
            MacroInfo {
                name: "RESV",
                abbrev: "RSV",
                default: 0.0,
            },
        ],
    },
    // 9: CY Metallic
    MachineInfo {
        macros: [
            MacroInfo {
                name: "TUNE",
                abbrev: "TUN",
                default: 0.20,
            },
            MacroInfo {
                name: "TONE",
                abbrev: "TON",
                default: 0.30,
            },
            MacroInfo {
                name: "TDEC",
                abbrev: "TDC",
                default: 0.15,
            },
            MacroInfo {
                name: "DEC",
                abbrev: "DEC",
                default: 0.30,
            },
            MacroInfo {
                name: "NCOL",
                abbrev: "NCL",
                default: 0.30,
            },
            MacroInfo {
                name: "LEVEL",
                abbrev: "LVL",
                default: 0.5,
            },
            MacroInfo {
                name: "RESV",
                abbrev: "RSV",
                default: 0.0,
            },
            MacroInfo {
                name: "RESV2",
                abbrev: "RS2",
                default: 0.0,
            },
        ],
    },
    // 10: CB Classic
    MachineInfo {
        macros: [
            MacroInfo {
                name: "TUNE",
                abbrev: "TUN",
                default: 0.40,
            },
            MacroInfo {
                name: "DEC",
                abbrev: "DEC",
                default: 0.15,
            },
            MacroInfo {
                name: "DET",
                abbrev: "DET",
                default: 0.86,
            },
            MacroInfo {
                name: "BPF",
                abbrev: "BPF",
                default: 0.35,
            },
            MacroInfo {
                name: "LEVEL",
                abbrev: "LVL",
                default: 0.55,
            },
            MacroInfo {
                name: "RESV",
                abbrev: "RSV",
                default: 0.0,
            },
            MacroInfo {
                name: "RESV2",
                abbrev: "RS2",
                default: 0.0,
            },
            MacroInfo {
                name: "RESV3",
                abbrev: "RS3",
                default: 0.0,
            },
        ],
    },
    // 11: SY Tone
    MachineInfo {
        macros: [
            MacroInfo {
                name: "TUNE",
                abbrev: "TUN",
                default: 0.50,
            },
            MacroInfo {
                name: "RATIO",
                abbrev: "RTO",
                default: 0.25,
            },
            MacroInfo {
                name: "FDBK",
                abbrev: "FDB",
                default: 0.20,
            },
            MacroInfo {
                name: "MENV",
                abbrev: "MEN",
                default: 0.25,
            },
            MacroInfo {
                name: "MOD.AMT",
                abbrev: "MDA",
                default: 0.40,
            },
            MacroInfo {
                name: "DEC",
                abbrev: "DEC",
                default: 0.30,
            },
            MacroInfo {
                name: "LEVEL",
                abbrev: "LVL",
                default: 0.7,
            },
            MacroInfo {
                name: "RESV",
                abbrev: "RSV",
                default: 0.0,
            },
        ],
    },
];

/// One of every machine. Enum dispatch keeps the per-sample path
/// branch-predictable and avoids trait objects; extending the catalogue is
/// additive — adding a variant and a match arm.
pub enum MachineSlot {
    /// Pitch-swept sine kick with drive.
    BdClassic(BdClassic),
    /// 2-operator FM kick.
    BdFm(BdFm),
    /// Pitch-swept sine tom with stick transient.
    Tom(Tom),
    /// Osc + filtered-noise snare.
    SdNatural(SdNatural),
    /// FM snare + noise.
    SdFm(SdFm),
    /// Rimshot: two detuned oscillators + noise tick.
    Rs(Rs),
    /// Clap: multi-burst noise + FM body.
    Cp(Cp),
    /// Bandpassed-noise closed hat.
    HatClassic(HatClassic),
    /// Six-oscillator 808-style hat.
    HhBasic(HhBasic),
    /// Ring-modulated metallic cymbal.
    CyMetallic(CyMetallic),
    /// Two-oscillator cowbell.
    CbClassic(CbClassic),
    /// 2-operator FM tonal synth.
    SyTone(SyTone),
}

impl MachineSlot {
    /// Build a slot of the given machine with the given macros applied.
    pub fn new(id: MachineId, macros: &[f32; NUM_MACROS]) -> Self {
        match id {
            MachineId::BdClassic => Self::BdClassic(BdClassic::new(macros)),
            MachineId::BdFm => Self::BdFm(BdFm::new(macros)),
            MachineId::Tom => Self::Tom(Tom::new(macros)),
            MachineId::SdNatural => Self::SdNatural(SdNatural::new(macros)),
            MachineId::SdFm => Self::SdFm(SdFm::new(macros)),
            MachineId::Rs => Self::Rs(Rs::new(macros)),
            MachineId::Cp => Self::Cp(Cp::new(macros)),
            MachineId::HatClassic => Self::HatClassic(HatClassic::new(macros)),
            MachineId::HhBasic => Self::HhBasic(HhBasic::new(macros)),
            MachineId::CyMetallic => Self::CyMetallic(CyMetallic::new(macros)),
            MachineId::CbClassic => Self::CbClassic(CbClassic::new(macros)),
            MachineId::SyTone => Self::SyTone(SyTone::new(macros)),
        }
    }

    /// Which machine this slot holds.
    pub fn id(&self) -> MachineId {
        match self {
            Self::BdClassic(_) => MachineId::BdClassic,
            Self::BdFm(_) => MachineId::BdFm,
            Self::Tom(_) => MachineId::Tom,
            Self::SdNatural(_) => MachineId::SdNatural,
            Self::SdFm(_) => MachineId::SdFm,
            Self::Rs(_) => MachineId::Rs,
            Self::Cp(_) => MachineId::Cp,
            Self::HatClassic(_) => MachineId::HatClassic,
            Self::HhBasic(_) => MachineId::HhBasic,
            Self::CyMetallic(_) => MachineId::CyMetallic,
            Self::CbClassic(_) => MachineId::CbClassic,
            Self::SyTone(_) => MachineId::SyTone,
        }
    }

    /// Recompute coefficients from the supplied macros. Setup rate.
    pub fn set_macros(&mut self, macros: &[f32; NUM_MACROS]) {
        match self {
            Self::BdClassic(m) => m.set_macros(macros),
            Self::BdFm(m) => m.set_macros(macros),
            Self::Tom(m) => m.set_macros(macros),
            Self::SdNatural(m) => m.set_macros(macros),
            Self::SdFm(m) => m.set_macros(macros),
            Self::Rs(m) => m.set_macros(macros),
            Self::Cp(m) => m.set_macros(macros),
            Self::HatClassic(m) => m.set_macros(macros),
            Self::HhBasic(m) => m.set_macros(macros),
            Self::CyMetallic(m) => m.set_macros(macros),
            Self::CbClassic(m) => m.set_macros(macros),
            Self::SyTone(m) => m.set_macros(macros),
        }
    }

    /// Begin a hit.
    pub fn trigger(&mut self, velocity: f32) {
        match self {
            Self::BdClassic(m) => m.trigger(velocity),
            Self::BdFm(m) => m.trigger(velocity),
            Self::Tom(m) => m.trigger(velocity),
            Self::SdNatural(m) => m.trigger(velocity),
            Self::SdFm(m) => m.trigger(velocity),
            Self::Rs(m) => m.trigger(velocity),
            Self::Cp(m) => m.trigger(velocity),
            Self::HatClassic(m) => m.trigger(velocity),
            Self::HhBasic(m) => m.trigger(velocity),
            Self::CyMetallic(m) => m.trigger(velocity),
            Self::CbClassic(m) => m.trigger(velocity),
            Self::SyTone(m) => m.trigger(velocity),
        }
    }

    /// Transpose by `semis` semitones relative to the machine's macro pitch.
    ///
    /// Scales every oscillator the machine owns so the whole voice — sweep,
    /// FM ratio, detune — moves in pitch. Noise-only machines (Hat Classic)
    /// no-op. Control rate, never in the per-sample path. Absolute, not
    /// incremental: passing the same value twice is a no-op.
    pub fn retune(&mut self, semis: f32) {
        match self {
            Self::BdClassic(m) => m.retune(semis),
            Self::BdFm(m) => m.retune(semis),
            Self::Tom(m) => m.retune(semis),
            Self::SdNatural(m) => m.retune(semis),
            Self::SdFm(m) => m.retune(semis),
            Self::Rs(m) => m.retune(semis),
            Self::Cp(m) => m.retune(semis),
            Self::HatClassic(m) => m.retune(semis),
            Self::HhBasic(m) => m.retune(semis),
            Self::CyMetallic(m) => m.retune(semis),
            Self::CbClassic(m) => m.retune(semis),
            Self::SyTone(m) => m.retune(semis),
        }
    }

    /// Force to silence.
    pub fn reset(&mut self) {
        match self {
            Self::BdClassic(m) => m.reset(),
            Self::BdFm(m) => m.reset(),
            Self::Tom(m) => m.reset(),
            Self::SdNatural(m) => m.reset(),
            Self::SdFm(m) => m.reset(),
            Self::Rs(m) => m.reset(),
            Self::Cp(m) => m.reset(),
            Self::HatClassic(m) => m.reset(),
            Self::HhBasic(m) => m.reset(),
            Self::CyMetallic(m) => m.reset(),
            Self::CbClassic(m) => m.reset(),
            Self::SyTone(m) => m.reset(),
        }
    }

    /// Still producing output?
    pub fn is_active(&self) -> bool {
        match self {
            Self::BdClassic(m) => m.is_active(),
            Self::BdFm(m) => m.is_active(),
            Self::Tom(m) => m.is_active(),
            Self::SdNatural(m) => m.is_active(),
            Self::SdFm(m) => m.is_active(),
            Self::Rs(m) => m.is_active(),
            Self::Cp(m) => m.is_active(),
            Self::HatClassic(m) => m.is_active(),
            Self::HhBasic(m) => m.is_active(),
            Self::CyMetallic(m) => m.is_active(),
            Self::CbClassic(m) => m.is_active(),
            Self::SyTone(m) => m.is_active(),
        }
    }

    /// One sample of machine output, *pre-track-strip*.
    #[inline(always)]
    pub fn tick(&mut self) -> f32 {
        match self {
            Self::BdClassic(m) => m.tick(),
            Self::BdFm(m) => m.tick(),
            Self::Tom(m) => m.tick(),
            Self::SdNatural(m) => m.tick(),
            Self::SdFm(m) => m.tick(),
            Self::Rs(m) => m.tick(),
            Self::Cp(m) => m.tick(),
            Self::HatClassic(m) => m.tick(),
            Self::HhBasic(m) => m.tick(),
            Self::CyMetallic(m) => m.tick(),
            Self::CbClassic(m) => m.tick(),
            Self::SyTone(m) => m.tick(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ids_have_stable_indices() {
        for (i, m) in MachineId::ALL.iter().enumerate() {
            assert_eq!(m.index(), i, "index/order mismatch");
        }
    }

    #[test]
    fn every_machine_has_named_macros_and_defaults() {
        for m in MachineId::ALL {
            let macros = m.macros();
            for info in &macros {
                assert!(!info.name.is_empty(), "empty name on {m:?}");
                assert!(
                    info.default >= 0.0 && info.default <= 1.0,
                    "bad default on {m:?}"
                );
            }
            assert_eq!(macros.len(), NUM_MACROS);
        }
    }

    #[test]
    fn default_macros_round_trip() {
        let m = MachineId::BdClassic;
        let d = m.default_macros();
        for (i, info) in m.macros().into_iter().enumerate() {
            assert_eq!(d[i], info.default, "macro {i} defaulted wrong");
        }
    }

    #[test]
    fn macro_by_name_finds_uppercase_exact() {
        let m = MachineId::BdClassic;
        assert_eq!(m.macro_by_name("TUNE").map(|(i, _)| i), Some(0));
        assert_eq!(m.macro_by_name("tune").map(|(i, _)| i), Some(0));
        assert_eq!(m.macro_by_name("DEC").map(|(i, _)| i), Some(3));
        assert_eq!(m.macro_by_name("MISSING"), None);
    }

    #[test]
    fn every_slot_acts_like_its_machine() {
        for id in MachineId::ALL {
            let macros = id.default_macros();
            let mut slot = MachineSlot::new(id, &macros);
            assert!(!slot.is_active());
            slot.trigger(1.0);
            assert!(slot.is_active());
        }
    }
}
