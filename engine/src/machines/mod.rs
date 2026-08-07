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
//! Every machine exposes [`NUM_MACROS`] macro knobs laid out as
//! [`NUM_BANKS`] banks of [`MACROS_PER_BANK`], mirroring the Syntakt
//! PITCH/FILTER/AMP/MOD pages. The flat slot index is `bank * 8 + index`
//! ([`macro_index`]); MIDI CC is `CC_TRACK_BASE + flat` (20-based), so
//! slot 0 maps to CC 20 and slot 31 maps to CC 51. A fixed layout lets the
//! same macro be a knob on one machine and a different-but-equivalent knob
//! on another (e.g. slot 8 is the filter cutoff family on every machine
//! that has a filter). Slots a machine does not use are `RESV` (default
//! 0.0) and ignored. The slot index order is stable across versions —
//! firmware CC mappings depend on it.
//!
//! | Bank | Slots | Group                 |
//! |------|-------|-----------------------|
//! | 0    | 0–7   | PITCH (tune, sweep, mod source, machine) |
//! | 1    | 8–15  | FILTER (cutoff/HPF/BPF, resonance/LPF)   |
//! | 2    | 16–23 | AMP (decay, level, shape, mix, sends)    |
//! | 3    | 24–31 | MOD (FM/mod amplitude, env decay)        |

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
pub const NUM_MACROS: usize = 32;

/// Number of macro banks, mirroring the Syntakt PITCH/FILTER/AMP/MOD pages.
pub const NUM_BANKS: usize = 4;

/// Macros per bank. Each bank maps to a 0–7 group on the Syntakt UI.
pub const MACROS_PER_BANK: usize = NUM_MACROS / NUM_BANKS;

/// Flat slot index for a macro, from its bank and in-bank position.
///
/// `bank * MACROS_PER_BANK + index`; MIDI CC is `CC_TRACK_BASE + flat`.
pub const fn macro_index(bank: usize, index: usize) -> usize {
    bank * MACROS_PER_BANK + index
}

/// PITCH bank: tune, pitch sweep, and the FM mod source.
pub const BANK_PITCH: usize = 0;
/// FILTER bank: cutoff/HPF/BPF and resonance/LPF.
pub const BANK_FILTER: usize = 1;
/// AMP bank: envelope, level, shape, mix, and sends.
pub const BANK_AMP: usize = 2;
/// MOD bank: FM/mod amplitude and env decay.
pub const BANK_MOD: usize = 3;

// PITCH slots (bank 0). CC 20 + flat.
/// PITCH bank: settled fundamental.
pub const SLOT_TUNE: usize = macro_index(BANK_PITCH, 0);
/// PITCH bank: pitch-sweep depth.
pub const SLOT_SWEEP: usize = macro_index(BANK_PITCH, 1);
/// PITCH bank: pitch-sweep time / modulator feedback.
pub const SLOT_SWEEP_TIME: usize = macro_index(BANK_PITCH, 2);
/// PITCH bank: FM modulator ratio (BD FM only).
pub const SLOT_MOD_HZ: usize = macro_index(BANK_PITCH, 3);
/// PITCH bank: FM modulator envelope decay (BD FM only).
pub const SLOT_MOD_DC: usize = macro_index(BANK_PITCH, 4);
/// PITCH bank: machine selector, quantised over [`MachineId::ALL`].
pub const SLOT_MACHINE: usize = macro_index(BANK_PITCH, 5);

// FILTER slots (bank 1). CC 20 + flat.
/// FILTER bank: cutoff / HPF / BPF frequency family.
pub const SLOT_CUT: usize = macro_index(BANK_FILTER, 0);
/// FILTER bank: resonance / lowpass cutoff family.
pub const SLOT_LPF: usize = macro_index(BANK_FILTER, 1);

// AMP slots (bank 2). CC 20 + flat.
/// AMP bank: per-machine output level.
pub const SLOT_LEVEL: usize = macro_index(BANK_AMP, 0);
/// AMP bank: per-machine panning.
pub const SLOT_PAN: usize = macro_index(BANK_AMP, 1);
/// AMP bank: amp-envelope decay time.
pub const SLOT_DECAY: usize = macro_index(BANK_AMP, 2);
/// AMP bank: secondary/noise decay time.
pub const SLOT_DECAY_2: usize = macro_index(BANK_AMP, 3);
/// AMP bank: voice-shaping amount (drive / stick / noise level).
pub const SLOT_SHAPE: usize = macro_index(BANK_AMP, 4);
/// AMP bank: dry/wet or noise/body mix.
pub const SLOT_MIX: usize = macro_index(BANK_AMP, 5);
/// AMP bank: delay send (track-routed).
pub const SLOT_SEND_DELAY: usize = macro_index(BANK_AMP, 6);
/// AMP bank: reverb send (track-routed).
pub const SLOT_SEND_REVERB: usize = macro_index(BANK_AMP, 7);

// MOD slots (bank 3). CC 20 + flat.
/// MOD bank: FM/mod depth.
pub const SLOT_MOD_AMOUNT: usize = macro_index(BANK_MOD, 0);
/// MOD bank: FM/mod envelope decay.
pub const SLOT_MOD_ENV: usize = macro_index(BANK_MOD, 1);

/// Stable index for a macro knob. Stored as `usize` in arrays `[f32;
/// NUM_MACROS]`, indexed by this enum so spread-by-name stays readable.
///
/// The variant order is stable across versions; firmware CC assignments
/// and host render flags depend on absolute indices staying put.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Macro {
    /// Macro knob slot 0 (PITCH 0).
    M0 = 0,
    /// Macro knob slot 1 (PITCH 1).
    M1 = 1,
    /// Macro knob slot 2 (PITCH 2).
    M2 = 2,
    /// Macro knob slot 3 (PITCH 3).
    M3 = 3,
    /// Macro knob slot 4 (PITCH 4).
    M4 = 4,
    /// Macro knob slot 5 (PITCH 5).
    M5 = 5,
    /// Macro knob slot 6 (PITCH 6).
    M6 = 6,
    /// Macro knob slot 7 (PITCH 7).
    M7 = 7,
    /// Macro knob slot 8 (FILTER 0).
    M8 = 8,
    /// Macro knob slot 9 (FILTER 1).
    M9 = 9,
    /// Macro knob slot 10 (FILTER 2).
    M10 = 10,
    /// Macro knob slot 11 (FILTER 3).
    M11 = 11,
    /// Macro knob slot 12 (FILTER 4).
    M12 = 12,
    /// Macro knob slot 13 (FILTER 5).
    M13 = 13,
    /// Macro knob slot 14 (FILTER 6).
    M14 = 14,
    /// Macro knob slot 15 (FILTER 7).
    M15 = 15,
    /// Macro knob slot 16 (AMP 0).
    M16 = 16,
    /// Macro knob slot 17 (AMP 1).
    M17 = 17,
    /// Macro knob slot 18 (AMP 2).
    M18 = 18,
    /// Macro knob slot 19 (AMP 3).
    M19 = 19,
    /// Macro knob slot 20 (AMP 4).
    M20 = 20,
    /// Macro knob slot 21 (AMP 5).
    M21 = 21,
    /// Macro knob slot 22 (AMP 6).
    M22 = 22,
    /// Macro knob slot 23 (AMP 7).
    M23 = 23,
    /// Macro knob slot 24 (MOD 0).
    M24 = 24,
    /// Macro knob slot 25 (MOD 1).
    M25 = 25,
    /// Macro knob slot 26 (MOD 2).
    M26 = 26,
    /// Macro knob slot 27 (MOD 3).
    M27 = 27,
    /// Macro knob slot 28 (MOD 4).
    M28 = 28,
    /// Macro knob slot 29 (MOD 5).
    M29 = 29,
    /// Macro knob slot 30 (MOD 6).
    M30 = 30,
    /// Macro knob slot 31 (MOD 7).
    M31 = 31,
}

impl Macro {
    /// All macros, in slot order.
    pub const ALL: [Self; NUM_MACROS] = [
        Self::M0,
        Self::M1,
        Self::M2,
        Self::M3,
        Self::M4,
        Self::M5,
        Self::M6,
        Self::M7,
        Self::M8,
        Self::M9,
        Self::M10,
        Self::M11,
        Self::M12,
        Self::M13,
        Self::M14,
        Self::M15,
        Self::M16,
        Self::M17,
        Self::M18,
        Self::M19,
        Self::M20,
        Self::M21,
        Self::M22,
        Self::M23,
        Self::M24,
        Self::M25,
        Self::M26,
        Self::M27,
        Self::M28,
        Self::M29,
        Self::M30,
        Self::M31,
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

/// Build a [`MacroInfo`] literal.
const fn mi(name: &'static str, abbrev: &'static str, default: f32) -> MacroInfo {
    MacroInfo { name, abbrev, default }
}

/// A reserved slot: no knob, canonical default 0.0, ignored by the voice.
const fn resv() -> MacroInfo {
    mi("RESV", "RSV", 0.0)
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
            mi("TUNE", "TUN", 0.20), // PITCH 0
            mi("SWEEP", "SWP", 0.36), // PITCH 1
            mi("SWP_T", "SWT", 0.15), // PITCH 2
            resv(), // PITCH 3
            resv(), // PITCH 4
            mi("MACH", "MCH", 0.0), // PITCH 5
            resv(), // PITCH 6
            resv(), // PITCH 7
            resv(), // FILTER 0
            resv(), // FILTER 1
            resv(), // FILTER 2
            resv(), // FILTER 3
            resv(), // FILTER 4
            resv(), // FILTER 5
            resv(), // FILTER 6
            resv(), // FILTER 7
            mi("LEVEL", "LVL", 0.9), // AMP o
            mi("PAN", "PAN", 0.5), // AMP 1
            mi("DEC", "DEC", 0.255), // AMP 2
            resv(), // AMP 3
            mi("DRIVE", "DRV", 0.16), // AMP 4
            resv(), // AMP 5
            mi("SEND.DLY", "SDY", 0.0), // AMP 6
            mi("SEND.RVB", "SRV", 0.0), // AMP 7
            resv(), // MOD 0
            resv(), // MOD 1
            resv(), // MOD 2
            resv(), // MOD 3
            resv(), // MOD 4
            resv(), // MOD 5
            resv(), // MOD 6
            resv(), // MOD 7
        ],
    },
    // 1: BD FM
    MachineInfo {
        macros: [
            mi("TUNE", "TUN", 0.20), // PITCH 0
            mi("SWEEP", "SWP", 0.30), // PITCH 1
            mi("SWP_T", "SWT", 0.15), // PITCH 2
            mi("MOD.HZ", "MDH", 0.43), // PITCH 3
            mi("MOD.DC", "MDD", 0.15), // PITCH 4
            mi("MACH", "MCH", 0.0), // PITCH 5
            resv(), // PITCH 6
            resv(), // PITCH 7
            resv(), // FILTER 0
            resv(), // FILTER 1
            resv(), // FILTER 2
            resv(), // FILTER 3
            resv(), // FILTER 4
            resv(), // FILTER 5
            resv(), // FILTER 6
            resv(), // FILTER 7
            mi("LEVEL", "LVL", 0.9), // AMP o
            mi("PAN", "PAN", 0.5), // AMP 1
            mi("DEC", "DEC", 0.255), // AMP 2
            resv(), // AMP 3
            resv(), // AMP 4
            resv(), // AMP 5
            mi("SEND.DLY", "SDY", 0.0), // AMP 6
            mi("SEND.RVB", "SRV", 0.0), // AMP 7
            mi("MOD.AMT", "MDA", 0.35), // MOD 0
            resv(), // MOD 1
            resv(), // MOD 2
            resv(), // MOD 3
            resv(), // MOD 4
            resv(), // MOD 5
            resv(), // MOD 6
            resv(), // MOD 7
        ],
    },
    // 2: Tom
    MachineInfo {
        macros: [
            mi("TUNE", "TUN", 0.35), // PITCH 0
            mi("SWEEP", "SWP", 0.40), // PITCH 1
            mi("SWP_T", "SWT", 0.40), // PITCH 2
            resv(), // PITCH 3
            resv(), // PITCH 4
            mi("MACH", "MCH", 0.0), // PITCH 5
            resv(), // PITCH 6
            resv(), // PITCH 7
            resv(), // FILTER 0
            resv(), // FILTER 1
            resv(), // FILTER 2
            resv(), // FILTER 3
            resv(), // FILTER 4
            resv(), // FILTER 5
            resv(), // FILTER 6
            resv(), // FILTER 7
            mi("LEVEL", "LVL", 0.85), // AMP o
            mi("PAN", "PAN", 0.5), // AMP 1
            mi("DEC", "DEC", 0.40), // AMP 2
            resv(), // AMP 3
            mi("STICK", "STK", 0.30), // AMP 4
            resv(), // AMP 5
            mi("SEND.DLY", "SDY", 0.0), // AMP 6
            mi("SEND.RVB", "SRV", 0.0), // AMP 7
            resv(), // MOD 0
            resv(), // MOD 1
            resv(), // MOD 2
            resv(), // MOD 3
            resv(), // MOD 4
            resv(), // MOD 5
            resv(), // MOD 6
            resv(), // MOD 7
        ],
    },
    // 3: SD Natural
    MachineInfo {
        macros: [
            mi("TUNE", "TUN", 0.28), // PITCH 0
            mi("RATIO", "RTO", 0.48), // PITCH 1
            resv(), // PITCH 2
            resv(), // PITCH 3
            resv(), // PITCH 4
            mi("MACH", "MCH", 0.0), // PITCH 5
            resv(), // PITCH 6
            resv(), // PITCH 7
            mi("HPF", "HPF", 0.14), // FILTER 0
            resv(), // FILTER 1
            resv(), // FILTER 2
            resv(), // FILTER 3
            resv(), // FILTER 4
            resv(), // FILTER 5
            resv(), // FILTER 6
            resv(), // FILTER 7
            mi("LEVEL", "LVL", 0.7), // AMP o
            mi("PAN", "PAN", 0.5), // AMP 1
            mi("BDEC", "BDC", 0.13), // AMP 2
            mi("NDEC", "NDC", 0.209), // AMP 3
            resv(), // AMP 4
            mi("NMIX", "NM", 0.62), // AMP 5
            mi("SEND.DLY", "SDY", 0.0), // AMP 6
            mi("SEND.RVB", "SRV", 0.0), // AMP 7
            resv(), // MOD 0
            resv(), // MOD 1
            resv(), // MOD 2
            resv(), // MOD 3
            resv(), // MOD 4
            resv(), // MOD 5
            resv(), // MOD 6
            resv(), // MOD 7
        ],
    },
    // 4: SD FM
    MachineInfo {
        macros: [
            mi("TUNE", "TUN", 0.28), // PITCH 0
            mi("RAT", "RAT", 0.33), // PITCH 1
            resv(), // PITCH 2
            resv(), // PITCH 3
            resv(), // PITCH 4
            mi("MACH", "MCH", 0.0), // PITCH 5
            resv(), // PITCH 6
            resv(), // PITCH 7
            resv(), // FILTER 0
            resv(), // FILTER 1
            resv(), // FILTER 2
            resv(), // FILTER 3
            resv(), // FILTER 4
            resv(), // FILTER 5
            resv(), // FILTER 6
            resv(), // FILTER 7
            mi("LEVEL", "LVL", 0.7), // AMP o
            mi("PAN", "PAN", 0.5), // AMP 1
            mi("BDEC", "BDC", 0.13), // AMP 2
            mi("NDEC", "NDC", 0.209), // AMP 3
            resv(), // AMP 4
            mi("NMIX", "NM", 0.62), // AMP 5
            mi("SEND.DLY", "SDY", 0.0), // AMP 6
            mi("SEND.RVB", "SRV", 0.0), // AMP 7
            mi("MOD.AMT", "MDA", 0.30), // MOD 0
            mi("MENV", "MEN", 0.15), // MOD 1
            resv(), // MOD 2
            resv(), // MOD 3
            resv(), // MOD 4
            resv(), // MOD 5
            resv(), // MOD 6
            resv(), // MOD 7
        ],
    },
    // 5: RS (rimshot)
    MachineInfo {
        macros: [
            mi("TUNE", "TUN", 0.40), // PITCH 0
            mi("DET", "DET", 0.25), // PITCH 1
            resv(), // PITCH 2
            resv(), // PITCH 3
            resv(), // PITCH 4
            mi("MACH", "MCH", 0.0), // PITCH 5
            resv(), // PITCH 6
            resv(), // PITCH 7
            mi("HPF", "HPF", 0.30), // FILTER 0
            resv(), // FILTER 1
            resv(), // FILTER 2
            resv(), // FILTER 3
            resv(), // FILTER 4
            resv(), // FILTER 5
            resv(), // FILTER 6
            resv(), // FILTER 7
            mi("LEVEL", "LVL", 0.75), // AMP o
            mi("PAN", "PAN", 0.5), // AMP 1
            mi("DEC", "DEC", 0.30), // AMP 2
            mi("NDEC", "NDC", 0.30), // AMP 3
            mi("NLEV", "NLV", 0.40), // AMP 4
            resv(), // AMP 5
            mi("SEND.DLY", "SDY", 0.0), // AMP 6
            mi("SEND.RVB", "SRV", 0.0), // AMP 7
            resv(), // MOD 0
            resv(), // MOD 1
            resv(), // MOD 2
            resv(), // MOD 3
            resv(), // MOD 4
            resv(), // MOD 5
            resv(), // MOD 6
            resv(), // MOD 7
        ],
    },
    // 6: CP (clap)
    MachineInfo {
        macros: [
            mi("TUNE", "TUN", 0.30), // PITCH 0
            mi("RATIO", "RTO", 0.50), // PITCH 1
            resv(), // PITCH 2
            resv(), // PITCH 3
            resv(), // PITCH 4
            mi("MACH", "MCH", 0.0), // PITCH 5
            resv(), // PITCH 6
            resv(), // PITCH 7
            mi("HPF", "HPF", 0.20), // FILTER 0
            mi("LPF", "LPF", 0.50), // FILTER 1
            resv(), // FILTER 2
            resv(), // FILTER 3
            resv(), // FILTER 4
            resv(), // FILTER 5
            resv(), // FILTER 6
            resv(), // FILTER 7
            mi("LEVEL", "LVL", 0.7), // AMP o
            mi("PAN", "PAN", 0.5), // AMP 1
            mi("BDEC", "BDC", 0.20), // AMP 2
            mi("NDEC", "NDC", 0.30), // AMP 3
            resv(), // AMP 4
            mi("BAL", "BAL", 0.80), // AMP 5
            mi("SEND.DLY", "SDY", 0.0), // AMP 6
            mi("SEND.RVB", "SRV", 0.0), // AMP 7
            resv(), // MOD 0
            resv(), // MOD 1
            resv(), // MOD 2
            resv(), // MOD 3
            resv(), // MOD 4
            resv(), // MOD 5
            resv(), // MOD 6
            resv(), // MOD 7
        ],
    },
    // 7: Hat Classic
    MachineInfo {
        macros: [
            resv(), // PITCH 0
            resv(), // PITCH 1
            resv(), // PITCH 2
            resv(), // PITCH 3
            resv(), // PITCH 4
            mi("MACH", "MCH", 0.0), // PITCH 5
            resv(), // PITCH 6
            resv(), // PITCH 7
            mi("HPF", "HPF", 0.45), // FILTER 0
            mi("LPF", "LPF", 0.75), // FILTER 1
            resv(), // FILTER 2
            resv(), // FILTER 3
            resv(), // FILTER 4
            resv(), // FILTER 5
            resv(), // FILTER 6
            resv(), // FILTER 7
            mi("LEVEL", "LVL", 0.4), // AMP o
            mi("PAN", "PAN", 0.5), // AMP 1
            mi("DEC", "DEC", 0.092), // AMP 2
            resv(), // AMP 3
            resv(), // AMP 4
            resv(), // AMP 5
            mi("SEND.DLY", "SDY", 0.0), // AMP 6
            mi("SEND.RVB", "SRV", 0.0), // AMP 7
            resv(), // MOD 0
            resv(), // MOD 1
            resv(), // MOD 2
            resv(), // MOD 3
            resv(), // MOD 4
            resv(), // MOD 5
            resv(), // MOD 6
            resv(), // MOD 7
        ],
    },
    // 8: HH Basic
    MachineInfo {
        macros: [
            mi("TUNE", "TUN", 0.30), // PITCH 0
            mi("TONE", "TON", 0.50), // PITCH 1
            resv(), // PITCH 2
            resv(), // PITCH 3
            resv(), // PITCH 4
            mi("MACH", "MCH", 0.0), // PITCH 5
            resv(), // PITCH 6
            resv(), // PITCH 7
            mi("BPF", "BPF", 0.50), // FILTER 0
            resv(), // FILTER 1
            resv(), // FILTER 2
            resv(), // FILTER 3
            resv(), // FILTER 4
            resv(), // FILTER 5
            resv(), // FILTER 6
            resv(), // FILTER 7
            mi("LEVEL", "LVL", 0.4), // AMP 0
            mi("PAN", "PAN", 0.5), // AMP 1
            mi("DEC", "DEC", 0.092), // AMP 2
            mi("TDEC", "TDC", 0.30), // AMP 3
            resv(), // AMP 4
            mi("RST", "RST", 1.0), // AMP 5
            mi("SEND.DLY", "SDY", 0.0), // AMP 6
            mi("SEND.RVB", "SRV", 0.0), // AMP 7
            resv(), // MOD 0
            resv(), // MOD 1
            resv(), // MOD 2
            resv(), // MOD 3
            resv(), // MOD 4
            resv(), // MOD 5
            resv(), // MOD 6
            resv(), // MOD 7
        ],
    },
    // 9: CY Metallic
    MachineInfo {
        macros: [
            mi("TUNE", "TUN", 0.20), // PITCH 0
            mi("TONE", "TON", 0.30), // PITCH 1
            resv(), // PITCH 2
            resv(), // PITCH 3
            resv(), // PITCH 4
            mi("MACH", "MCH", 0.0), // PITCH 5
            resv(), // PITCH 6
            resv(), // PITCH 7
            mi("NCOL", "NCL", 0.30), // FILTER 0
            resv(), // FILTER 1
            resv(), // FILTER 2
            resv(), // FILTER 3
            resv(), // FILTER 4
            resv(), // FILTER 5
            resv(), // FILTER 6
            resv(), // FILTER 7
            mi("LEVEL", "LVL", 0.5), // AMP 0
            mi("PAN", "PAN", 0.5), // AMP 1
            mi("DEC", "DEC", 0.30), // AMP 2
            mi("TDEC", "TDC", 0.15), // AMP 3
            resv(), // AMP 4
            resv(), // AMP 5
            mi("SEND.DLY", "SDY", 0.0), // AMP 6
            mi("SEND.RVB", "SRV", 0.0), // AMP 7
            resv(), // MOD 0
            resv(), // MOD 1
            resv(), // MOD 2
            resv(), // MOD 3
            resv(), // MOD 4
            resv(), // MOD 5
            resv(), // MOD 6
            resv(), // MOD 7
        ],
    },
    // 10: CB Classic
    MachineInfo {
        macros: [
            mi("TUNE", "TUN", 0.40), // PITCH 0
            mi("DET", "DET", 0.86), // PITCH 1
            resv(), // PITCH 2
            resv(), // PITCH 3
            resv(), // PITCH 4
            mi("MACH", "MCH", 0.0), // PITCH 5
            resv(), // PITCH 6
            resv(), // PITCH 7
            mi("BPF", "BPF", 0.35), // FILTER 0
            resv(), // FILTER 1
            resv(), // FILTER 2
            resv(), // FILTER 3
            resv(), // FILTER 4
            resv(), // FILTER 5
            resv(), // FILTER 6
            resv(), // FILTER 7
            mi("LEVEL", "LVL", 0.55), // AMP 0
            mi("PAN", "PAN", 0.5), // AMP 1
            mi("DEC", "DEC", 0.15), // AMP 2
            resv(), // AMP 3
            resv(), // AMP 4
            resv(), // AMP 5
            mi("SEND.DLY", "SDY", 0.0), // AMP 6
            mi("SEND.RVB", "SRV", 0.0), // AMP 7
            resv(), // MOD 0
            resv(), // MOD 1
            resv(), // MOD 2
            resv(), // MOD 3
            resv(), // MOD 4
            resv(), // MOD 5
            resv(), // MOD 6
            resv(), // MOD 7
        ],
    },
    // 11: SY Tone
    MachineInfo {
        macros: [
            mi("TUNE", "TUN", 0.50), // PITCH 0
            mi("RATIO", "RTO", 0.25), // PITCH 1
            mi("FDBK", "FDB", 0.20), // PITCH 2
            resv(), // PITCH 3
            resv(), // PITCH 4
            mi("MACH", "MCH", 0.0), // PITCH 5
            resv(), // PITCH 6
            resv(), // PITCH 7
            resv(), // FILTER 0
            resv(), // FILTER 1
            resv(), // FILTER 2
            resv(), // FILTER 3
            resv(), // FILTER 4
            resv(), // FILTER 5
            resv(), // FILTER 6
            resv(), // FILTER 7
            mi("LEVEL", "LVL", 0.7), // AMP 0
            mi("PAN", "PAN", 0.5), // AMP 1
            mi("DEC", "DEC", 0.30), // AMP 2
            resv(), // AMP 3
            resv(), // AMP 4
            resv(), // AMP 5
            mi("SEND.DLY", "SDY", 0.0), // AMP 6
            mi("SEND.RVB", "SRV", 0.0), // AMP 7
            mi("MOD.AMT", "MDA", 0.40), // MOD 0
            mi("MENV", "MEN", 0.25), // MOD 1
            resv(), // MOD 2
            resv(), // MOD 3
            resv(), // MOD 4
            resv(), // MOD 5
            resv(), // MOD 6
            resv(), // MOD 7
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
        assert_eq!(m.macro_by_name("TUNE").map(|(i, _)| i), Some(SLOT_TUNE));
        assert_eq!(m.macro_by_name("tune").map(|(i, _)| i), Some(SLOT_TUNE));
        assert_eq!(m.macro_by_name("DEC").map(|(i, _)| i), Some(SLOT_DECAY));
        assert_eq!(m.macro_by_name("MISSING"), None);
    }

    #[test]
    fn canonical_slot_constants_are_stable() {
        assert_eq!(NUM_MACROS, 32);
        assert_eq!(NUM_BANKS, 4);
        assert_eq!(MACROS_PER_BANK, 8);
        assert_eq!(SLOT_TUNE, 0);
        assert_eq!(SLOT_SWEEP, 1);
        assert_eq!(SLOT_SWEEP_TIME, 2);
        assert_eq!(SLOT_MOD_HZ, 3);
        assert_eq!(SLOT_MOD_DC, 4);
        assert_eq!(SLOT_MACHINE, 5);
        assert_eq!(SLOT_CUT, 8);
        assert_eq!(SLOT_LPF, 9);
        assert_eq!(SLOT_LEVEL, 16);
        assert_eq!(SLOT_PAN, 17);
        assert_eq!(SLOT_DECAY, 18);
        assert_eq!(SLOT_DECAY_2, 19);
        assert_eq!(SLOT_SHAPE, 20);
        assert_eq!(SLOT_MIX, 21);
        assert_eq!(SLOT_SEND_DELAY, 22);
        assert_eq!(SLOT_SEND_REVERB, 23);
        assert_eq!(SLOT_MOD_AMOUNT, 24);
        assert_eq!(SLOT_MOD_ENV, 25);
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
