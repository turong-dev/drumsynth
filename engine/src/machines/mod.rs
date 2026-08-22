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
//! | 0    | 0–7   | MACH (tune, sweep, FM, decay, shape) |
//! | 1    | 8–15  | FILT (machine filter + strip SVF + strip AHD env) |
//! | 2    | 16–23 | TRACK (machine select, output, pan, level, sends) |
//! | 3    | 24–31 | MOD (FM mod amount/env + LFOs) |

pub mod bd_classic;
pub mod bd_fm;
pub mod bd_va;
pub mod cb_classic;
pub mod cp;
pub mod cy_metallic;
pub mod dub_siren;
pub mod hat_classic;
pub mod hh_basic;
pub mod rs;
pub mod sd_fm;
pub mod sd_natural;
pub mod sweep_fx;
pub mod sy_tone;
pub mod tom;

pub use bd_classic::BdClassic;
pub use bd_fm::BdFm;
pub use bd_va::BdVa;
pub use cb_classic::CbClassic;
pub use cp::Cp;
pub use cy_metallic::CyMetallic;
pub use dub_siren::DubSiren;
pub use hat_classic::HatClassic;
pub use hh_basic::HhBasic;
pub use rs::Rs;
pub use sd_fm::SdFm;
pub use sd_natural::SdNatural;
pub use sweep_fx::SweepFx;
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

/// MACH bank: machine-specific synthesis controls. Numbered, not named —
/// each machine assigns meaning via `MACHINE_INFO` and `set_macros`.
pub const BANK_MACH: usize = 0;
/// FILT bank: machine-internal filter (numbered) + strip SVF + strip AHD env.
pub const BANK_FILT: usize = 1;
/// TRACK bank: track-level routing and mixing (machine select, output, pan, level, sends).
pub const BANK_TRACK: usize = 2;
/// MOD bank: LFOs only.
pub const BANK_MOD: usize = 3;

// MACH slots (bank 0). CC 20 + flat. Numbered — each machine lays out its
// own parameters here. The name/abbrev/default for each slot is defined
// per-machine in `MACHINE_INFO`.
/// MACH bank slot 0. CC 20. Machine-specific.
pub const SLOT_MACH_0: usize = macro_index(BANK_MACH, 0);
/// MACH bank slot 1. CC 21. Machine-specific.
pub const SLOT_MACH_1: usize = macro_index(BANK_MACH, 1);
/// MACH bank slot 2. CC 22. Machine-specific.
pub const SLOT_MACH_2: usize = macro_index(BANK_MACH, 2);
/// MACH bank slot 3. CC 23. Machine-specific.
pub const SLOT_MACH_3: usize = macro_index(BANK_MACH, 3);
/// MACH bank slot 4. CC 24. Machine-specific.
pub const SLOT_MACH_4: usize = macro_index(BANK_MACH, 4);
/// MACH bank slot 5. CC 25. Machine-specific.
pub const SLOT_MACH_5: usize = macro_index(BANK_MACH, 5);
/// MACH bank slot 6. CC 26. Machine-specific.
pub const SLOT_MACH_6: usize = macro_index(BANK_MACH, 6);
/// MACH bank slot 7. CC 27. Machine-specific.
pub const SLOT_MACH_7: usize = macro_index(BANK_MACH, 7);

// FILT slots (bank 1). CC 20 + flat.
/// FILT bank: machine-internal filter slot 0. CC 28. Numbered — meaning
/// varies per machine (HPF, BPF, etc.).
pub const SLOT_FILT_0: usize = macro_index(BANK_FILT, 0);
/// FILT bank: machine-internal filter slot 1. CC 29. Numbered — meaning
/// varies per machine (LPF, resonance, Q, etc.).
pub const SLOT_FILT_1: usize = macro_index(BANK_FILT, 1);
/// FILT bank: per-track strip SVF cutoff (log-mapped 20 Hz..20 kHz). CC 30.
/// Track-routed: drives [`crate::StripParams::f_cutoff_hz`], not the machine.
pub const SLOT_STRIP_CUT: usize = macro_index(BANK_FILT, 2);
/// FILT bank: per-track strip SVF resonance (0.5..20 Q). CC 31.
/// Track-routed: drives [`crate::StripParams::f_reso_q`], not the machine.
pub const SLOT_STRIP_RESO: usize = macro_index(BANK_FILT, 3);
/// FILT bank: per-track strip AHD attack (0..1 s). CC 32.
/// Track-routed: drives [`crate::StripParams::amp_attack_s`].
pub const SLOT_STRIP_ATK: usize = macro_index(BANK_FILT, 4);
/// FILT bank: per-track strip AHD hold (0..10 s). CC 33.
/// Track-routed: drives [`crate::StripParams::amp_hold_s`].
pub const SLOT_STRIP_HOLD: usize = macro_index(BANK_FILT, 5);
/// FILT bank: per-track strip AHD decay (0.01..10 s). CC 34.
/// Track-routed: drives [`crate::StripParams::amp_decay_s`].
pub const SLOT_STRIP_DEC: usize = macro_index(BANK_FILT, 6);

// TRACK slots (bank 2). CC 20 + flat.
/// TRACK bank: machine selector, quantised over [`MachineId::ALL`]. CC 36.
/// Track-routed: swaps the machine on this track.
pub const SLOT_MACHINE: usize = macro_index(BANK_TRACK, 0);
/// TRACK bank: per-track output routing. 0..1 quantised over [`crate::OutPair`]
/// (`Master`, `Aux1`, `Aux2`, `Aux3`). CC 37. Track-routed.
pub const SLOT_OUT: usize = macro_index(BANK_TRACK, 1);
/// TRACK bank: per-track panning. CC 38. Track-routed.
pub const SLOT_PAN: usize = macro_index(BANK_TRACK, 2);
/// TRACK bank: per-track output level. CC 39. Track-routed.
pub const SLOT_LEVEL: usize = macro_index(BANK_TRACK, 3);
/// TRACK bank: delay send (track-routed). CC 40. Zero this (and
/// [`SLOT_SEND_REVERB`]) to take the send-FX bus out of the picture when
/// isolating a single track's CPU cost.
pub const SLOT_SEND_DELAY: usize = macro_index(BANK_TRACK, 4);
/// TRACK bank: reverb send (track-routed). CC 41. See [`SLOT_SEND_DELAY`].
pub const SLOT_SEND_REVERB: usize = macro_index(BANK_TRACK, 5);

// MOD slots (bank 3). CC 20 + flat. LFOs only.
/// MOD bank: LFO 1 rate. 0..0.5 = slow range (0.1..10 Hz), 0.5..1 = fast
/// range (1..100 Hz), log-mapped within each half. CC 46. Track-routed.
pub const SLOT_LFO1_RATE: usize = macro_index(BANK_MOD, 2);
/// MOD bank: LFO 1 depth (0..1). CC 47. Track-routed.
pub const SLOT_LFO1_DEPTH: usize = macro_index(BANK_MOD, 3);
/// MOD bank: LFO 1 destination (quantised over [`crate::dsp::ModDest`]). CC 48.
/// Track-routed.
pub const SLOT_LFO1_DEST: usize = macro_index(BANK_MOD, 4);
/// MOD bank: LFO 2 rate. Same split mapping as [`SLOT_LFO1_RATE`]. CC 49.
/// Track-routed.
pub const SLOT_LFO2_RATE: usize = macro_index(BANK_MOD, 5);
/// MOD bank: LFO 2 depth (0..1). CC 50. Track-routed.
pub const SLOT_LFO2_DEPTH: usize = macro_index(BANK_MOD, 6);
/// MOD bank: LFO 2 destination (quantised over [`crate::dsp::ModDest`]). CC 51.
/// Track-routed.
pub const SLOT_LFO2_DEST: usize = macro_index(BANK_MOD, 7);

/// Stable index for a macro knob. Stored as `usize` in arrays `[f32;
/// NUM_MACROS]`, indexed by this enum so spread-by-name stays readable.
///
/// The variant order is stable across versions; firmware CC assignments
/// and host render flags depend on absolute indices staying put.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Macro {
    /// Macro knob slot 0 (MACH 0).
    M0 = 0,
    /// Macro knob slot 1 (MACH 1).
    M1 = 1,
    /// Macro knob slot 2 (MACH 2).
    M2 = 2,
    /// Macro knob slot 3 (MACH 3).
    M3 = 3,
    /// Macro knob slot 4 (MACH 4).
    M4 = 4,
    /// Macro knob slot 5 (MACH 5).
    M5 = 5,
    /// Macro knob slot 6 (MACH 6).
    M6 = 6,
    /// Macro knob slot 7 (MACH 7).
    M7 = 7,
    /// Macro knob slot 8 (FILT 0).
    M8 = 8,
    /// Macro knob slot 9 (FILT 1).
    M9 = 9,
    /// Macro knob slot 10 (FILT 2).
    M10 = 10,
    /// Macro knob slot 11 (FILT 3).
    M11 = 11,
    /// Macro knob slot 12 (FILT 4).
    M12 = 12,
    /// Macro knob slot 13 (FILT 5).
    M13 = 13,
    /// Macro knob slot 14 (FILT 6).
    M14 = 14,
    /// Macro knob slot 15 (FILT 7).
    M15 = 15,
    /// Macro knob slot 16 (TRACK 0).
    M16 = 16,
    /// Macro knob slot 17 (TRACK 1).
    M17 = 17,
    /// Macro knob slot 18 (TRACK 2).
    M18 = 18,
    /// Macro knob slot 19 (TRACK 3).
    M19 = 19,
    /// Macro knob slot 20 (TRACK 4).
    M20 = 20,
    /// Macro knob slot 21 (TRACK 5).
    M21 = 21,
    /// Macro knob slot 22 (TRACK 6).
    M22 = 22,
    /// Macro knob slot 23 (TRACK 7).
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
    MacroInfo {
        name,
        abbrev,
        default,
    }
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
    /// Virtual-analogue kick via bridged-T resonator — TR-808-style thump.
    BdVa,
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
    /// Dub siren: sine carrier with an internal pitch-modulating LFO,
    /// gated by a long self-timed AHD gesture.
    DubSiren,
    /// Sweep FX: white noise through a swept SVF, gated by a long
    /// self-timed AHD gesture.
    SweepFx,
}

impl MachineId {
    /// Number of machines currently catalogued.
    pub const COUNT: usize = 15;

    /// All machines, in catalogue order. Renaming a machine is fine; the
    /// order is part of the binary layout (firmware maps CC slots against
    /// indices that pick from here).
    pub const ALL: [Self; Self::COUNT] = [
        Self::BdClassic,
        Self::BdFm,
        Self::BdVa,
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
        Self::DubSiren,
        Self::SweepFx,
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
            Self::BdVa => "bd-va",
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
            Self::DubSiren => "dub-siren",
            Self::SweepFx => "sweep-fx",
        }
    }

    /// Human-readable label for display.
    pub fn label(self) -> &'static str {
        match self {
            Self::BdClassic => "BD Classic",
            Self::BdFm => "BD FM",
            Self::BdVa => "BD VA",
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
            Self::DubSiren => "Dub Siren",
            Self::SweepFx => "Sweep FX",
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


// ---- Track-routed macro info ----
// Same on every machine: defined once, referenced by every `MACHINE_INFO` row.
// Editing a shared slot's name/abbrev/default here propagates to all 15 machines.

/// MACH selector (TRACK 0). CC 36.
const MACH_INFO: MacroInfo = mi("MACH", "MCH", 0.0);
/// Strip SVF cutoff (FILT 2). CC 30. Default 1.0 = 20 kHz (fully open).
const STRIP_CUT_INFO: MacroInfo = mi("STRIP.CUT", "SCUT", 1.0);
/// Strip SVF resonance (FILT 3). CC 31. Default ~0.01 = Butterworth Q.
const STRIP_RESO_INFO: MacroInfo = mi("STRIP.RESO", "SRES", 0.01);
/// Strip AHD attack (FILT 4). CC 32. Default 0.0 = instant.
const STRIP_ATK_INFO: MacroInfo = mi("STRIP.ATK", "SATK", 0.0);
/// Strip AHD hold (FILT 5). CC 33. Default 1.0 = 10 s (long, machine shapes the hit).
const STRIP_HOLD_INFO: MacroInfo = mi("STRIP.HOLD", "SHLD", 1.0);
/// Strip AHD decay (FILT 6). CC 34. Default 1.0 = 10 s (long, machine shapes the hit).
const STRIP_DEC_INFO: MacroInfo = mi("STRIP.DEC", "SDEC", 1.0);
/// Strip pan (TRACK 2). CC 38. Default 0.5 = centre.
const PAN_INFO: MacroInfo = mi("PAN", "PAN", 0.5);
/// Delay send (TRACK 4). CC 40.
const SEND_DLY_INFO: MacroInfo = mi("SEND.DLY", "SDY", 0.0);
/// Reverb send (TRACK 5). CC 41.
const SEND_RVB_INFO: MacroInfo = mi("SEND.RVB", "SRV", 0.0);
/// Output routing (TRACK 1). CC 37. Default 0.0 = Master.
const OUT_INFO: MacroInfo = mi("OUT", "OUT", 0.0);
/// LFO 1 rate (MOD 2). CC 46. 0..0.5 slow, 0.5..1 fast.
const LFO1_RATE_INFO: MacroInfo = mi("LFO1.RATE", "L1R", 0.0);
/// LFO 1 depth (MOD 3). CC 47.
const LFO1_DEPTH_INFO: MacroInfo = mi("LFO1.DEP", "L1D", 0.0);
/// LFO 1 destination (MOD 4). CC 48. Default 0.0 = Macro(0).
const LFO1_DEST_INFO: MacroInfo = mi("LFO1.DST", "L1S", 0.0);
/// LFO 2 rate (MOD 5). CC 49. 0..0.5 slow, 0.5..1 fast.
const LFO2_RATE_INFO: MacroInfo = mi("LFO2.RATE", "L2R", 0.0);
/// LFO 2 depth (MOD 6). CC 50.
const LFO2_DEPTH_INFO: MacroInfo = mi("LFO2.DEP", "L2D", 0.0);
/// LFO 2 destination (MOD 7). CC 51. Default 0.0 = Macro(0).
const LFO2_DEST_INFO: MacroInfo = mi("LFO2.DST", "L2S", 0.0);

/// Per-machine metadata, indexed by [`MachineId::index`].
struct MachineInfo {
    macros: [MacroInfo; NUM_MACROS],
}

static MACHINE_INFO: [MachineInfo; MachineId::COUNT] = [
    // 0: BD Classic
    MachineInfo {
        macros: [
            mi("TUNE", "TUN", 0.2),         // MACH 0
            mi("SWEEP", "SWP", 0.36),       // MACH 1
            mi("SWP_T", "SWT", 0.15),       // MACH 2
            resv(),                         // MACH 3
            resv(),                         // MACH 4
            mi("DEC", "DEC", 0.255),        // MACH 5
            resv(),                         // MACH 6
            mi("DRIVE", "DRV", 0.16),       // MACH 7
            resv(),                         // FILT 0
            resv(),                         // FILT 1
            STRIP_CUT_INFO,                 // FILT 2
            STRIP_RESO_INFO,                // FILT 3
            STRIP_ATK_INFO,                 // FILT 4
            STRIP_HOLD_INFO,                // FILT 5
            STRIP_DEC_INFO,                 // FILT 6
            resv(),                         // FILT 7
            MACH_INFO,                      // TRACK 0
            OUT_INFO,                       // TRACK 1
            PAN_INFO,                       // TRACK 2
            mi("LEVEL", "LVL", 0.9),        // TRACK 3
            SEND_DLY_INFO,                  // TRACK 4
            SEND_RVB_INFO,                  // TRACK 5
            resv(),                         // TRACK 6
            resv(),                         // TRACK 7
            resv(),                         // MOD 0
            resv(),                         // MOD 1
            LFO1_RATE_INFO,                 // MOD 2
            LFO1_DEPTH_INFO,                // MOD 3
            LFO1_DEST_INFO,                 // MOD 4
            LFO2_RATE_INFO,                 // MOD 5
            LFO2_DEPTH_INFO,                // MOD 6
            LFO2_DEST_INFO,                 // MOD 7
        ],
    },
    // 1: BD FM
    MachineInfo {
        macros: [
            mi("TUNE", "TUN", 0.2),         // MACH 0
            mi("SWEEP", "SWP", 0.3),        // MACH 1
            mi("SWP_T", "SWT", 0.15),       // MACH 2
            mi("MOD.HZ", "MDH", 0.43),      // MACH 3
            mi("MOD.DC", "MDD", 0.15),      // MACH 4
            mi("DEC", "DEC", 0.255),        // MACH 5
            mi("MOD.AMT", "MDA", 0.35),     // MACH 6
            resv(),                         // MACH 7
            resv(),                         // FILT 0
            resv(),                         // FILT 1
            STRIP_CUT_INFO,                 // FILT 2
            STRIP_RESO_INFO,                // FILT 3
            STRIP_ATK_INFO,                 // FILT 4
            STRIP_HOLD_INFO,                // FILT 5
            STRIP_DEC_INFO,                 // FILT 6
            resv(),                         // FILT 7
            MACH_INFO,                      // TRACK 0
            OUT_INFO,                       // TRACK 1
            PAN_INFO,                       // TRACK 2
            mi("LEVEL", "LVL", 0.9),        // TRACK 3
            SEND_DLY_INFO,                  // TRACK 4
            SEND_RVB_INFO,                  // TRACK 5
            resv(),                         // TRACK 6
            resv(),                         // TRACK 7
            resv(),                         // MOD 0
            resv(),                         // MOD 1
            LFO1_RATE_INFO,                 // MOD 2
            LFO1_DEPTH_INFO,                // MOD 3
            LFO1_DEST_INFO,                 // MOD 4
            LFO2_RATE_INFO,                 // MOD 5
            LFO2_DEPTH_INFO,                // MOD 6
            LFO2_DEST_INFO,                 // MOD 7
        ],
    },
    // 2: BD VA
    MachineInfo {
        macros: [
            mi("TUNE", "TUN", 0.28),        // MACH 0
            mi("SWEEP", "SWP", 1.0),        // MACH 1
            mi("SWP_T", "SWT", 0.1),        // MACH 2
            resv(),                         // MACH 3
            resv(),                         // MACH 4
            mi("DEC", "DEC", 0.255),        // MACH 5
            resv(),                         // MACH 6
            resv(),                         // MACH 7
            resv(),                         // FILT 0
            mi("Q", "Q", 0.42),             // FILT 1
            STRIP_CUT_INFO,                 // FILT 2
            STRIP_RESO_INFO,                // FILT 3
            STRIP_ATK_INFO,                 // FILT 4
            STRIP_HOLD_INFO,                // FILT 5
            STRIP_DEC_INFO,                 // FILT 6
            resv(),                         // FILT 7
            MACH_INFO,                      // TRACK 0
            OUT_INFO,                       // TRACK 1
            PAN_INFO,                       // TRACK 2
            mi("LEVEL", "LVL", 0.9),        // TRACK 3
            SEND_DLY_INFO,                  // TRACK 4
            SEND_RVB_INFO,                  // TRACK 5
            resv(),                         // TRACK 6
            resv(),                         // TRACK 7
            resv(),                         // MOD 0
            resv(),                         // MOD 1
            LFO1_RATE_INFO,                 // MOD 2
            LFO1_DEPTH_INFO,                // MOD 3
            LFO1_DEST_INFO,                 // MOD 4
            LFO2_RATE_INFO,                 // MOD 5
            LFO2_DEPTH_INFO,                // MOD 6
            LFO2_DEST_INFO,                 // MOD 7
        ],
    },
    // 3: Tom
    MachineInfo {
        macros: [
            mi("TUNE", "TUN", 0.35),        // MACH 0
            mi("SWEEP", "SWP", 0.4),        // MACH 1
            mi("SWP_T", "SWT", 0.4),        // MACH 2
            resv(),                         // MACH 3
            resv(),                         // MACH 4
            mi("DEC", "DEC", 0.4),          // MACH 5
            resv(),                         // MACH 6
            mi("STICK", "STK", 0.3),        // MACH 7
            resv(),                         // FILT 0
            resv(),                         // FILT 1
            STRIP_CUT_INFO,                 // FILT 2
            STRIP_RESO_INFO,                // FILT 3
            STRIP_ATK_INFO,                 // FILT 4
            STRIP_HOLD_INFO,                // FILT 5
            STRIP_DEC_INFO,                 // FILT 6
            resv(),                         // FILT 7
            MACH_INFO,                      // TRACK 0
            OUT_INFO,                       // TRACK 1
            PAN_INFO,                       // TRACK 2
            mi("LEVEL", "LVL", 0.85),       // TRACK 3
            SEND_DLY_INFO,                  // TRACK 4
            SEND_RVB_INFO,                  // TRACK 5
            resv(),                         // TRACK 6
            resv(),                         // TRACK 7
            resv(),                         // MOD 0
            resv(),                         // MOD 1
            LFO1_RATE_INFO,                 // MOD 2
            LFO1_DEPTH_INFO,                // MOD 3
            LFO1_DEST_INFO,                 // MOD 4
            LFO2_RATE_INFO,                 // MOD 5
            LFO2_DEPTH_INFO,                // MOD 6
            LFO2_DEST_INFO,                 // MOD 7
        ],
    },
    // 4: SD Natural
    MachineInfo {
        macros: [
            mi("TUNE", "TUN", 0.28),        // MACH 0
            mi("RATIO", "RTO", 0.48),       // MACH 1
            resv(),                         // MACH 2
            resv(),                         // MACH 3
            resv(),                         // MACH 4
            mi("BDEC", "BDC", 0.13),        // MACH 5
            mi("NDEC", "NDC", 0.209),       // MACH 6
            mi("NMIX", "NM", 0.62),         // MACH 7
            mi("HPF", "HPF", 0.14),         // FILT 0
            resv(),                         // FILT 1
            STRIP_CUT_INFO,                 // FILT 2
            STRIP_RESO_INFO,                // FILT 3
            STRIP_ATK_INFO,                 // FILT 4
            STRIP_HOLD_INFO,                // FILT 5
            STRIP_DEC_INFO,                 // FILT 6
            resv(),                         // FILT 7
            MACH_INFO,                      // TRACK 0
            OUT_INFO,                       // TRACK 1
            PAN_INFO,                       // TRACK 2
            mi("LEVEL", "LVL", 0.7),        // TRACK 3
            SEND_DLY_INFO,                  // TRACK 4
            SEND_RVB_INFO,                  // TRACK 5
            resv(),                         // TRACK 6
            resv(),                         // TRACK 7
            resv(),                         // MOD 0
            resv(),                         // MOD 1
            LFO1_RATE_INFO,                 // MOD 2
            LFO1_DEPTH_INFO,                // MOD 3
            LFO1_DEST_INFO,                 // MOD 4
            LFO2_RATE_INFO,                 // MOD 5
            LFO2_DEPTH_INFO,                // MOD 6
            LFO2_DEST_INFO,                 // MOD 7
        ],
    },
    // 5: SD FM
    MachineInfo {
        macros: [
            mi("TUNE", "TUN", 0.28),        // MACH 0
            mi("RAT", "RAT", 0.33),         // MACH 1
            mi("MOD.AMT", "MDA", 0.3),      // MACH 2
            mi("MENV", "MEN", 0.15),        // MACH 3
            resv(),                         // MACH 4
            mi("BDEC", "BDC", 0.13),        // MACH 5
            mi("NDEC", "NDC", 0.209),       // MACH 6
            mi("NMIX", "NM", 0.62),         // MACH 7
            resv(),                         // FILT 0
            resv(),                         // FILT 1
            STRIP_CUT_INFO,                 // FILT 2
            STRIP_RESO_INFO,                // FILT 3
            STRIP_ATK_INFO,                 // FILT 4
            STRIP_HOLD_INFO,                // FILT 5
            STRIP_DEC_INFO,                 // FILT 6
            resv(),                         // FILT 7
            MACH_INFO,                      // TRACK 0
            OUT_INFO,                       // TRACK 1
            PAN_INFO,                       // TRACK 2
            mi("LEVEL", "LVL", 0.7),        // TRACK 3
            SEND_DLY_INFO,                  // TRACK 4
            SEND_RVB_INFO,                  // TRACK 5
            resv(),                         // TRACK 6
            resv(),                         // TRACK 7
            resv(),                         // MOD 0
            resv(),                         // MOD 1
            LFO1_RATE_INFO,                 // MOD 2
            LFO1_DEPTH_INFO,                // MOD 3
            LFO1_DEST_INFO,                 // MOD 4
            LFO2_RATE_INFO,                 // MOD 5
            LFO2_DEPTH_INFO,                // MOD 6
            LFO2_DEST_INFO,                 // MOD 7
        ],
    },
    // 6: RS
    MachineInfo {
        macros: [
            mi("TUNE", "TUN", 0.4),         // MACH 0
            mi("DET", "DET", 0.25),         // MACH 1
            resv(),                         // MACH 2
            resv(),                         // MACH 3
            resv(),                         // MACH 4
            mi("DEC", "DEC", 0.3),          // MACH 5
            mi("NDEC", "NDC", 0.3),         // MACH 6
            mi("NLEV", "NLV", 0.4),         // MACH 7
            mi("HPF", "HPF", 0.3),          // FILT 0
            resv(),                         // FILT 1
            STRIP_CUT_INFO,                 // FILT 2
            STRIP_RESO_INFO,                // FILT 3
            STRIP_ATK_INFO,                 // FILT 4
            STRIP_HOLD_INFO,                // FILT 5
            STRIP_DEC_INFO,                 // FILT 6
            resv(),                         // FILT 7
            MACH_INFO,                      // TRACK 0
            OUT_INFO,                       // TRACK 1
            PAN_INFO,                       // TRACK 2
            mi("LEVEL", "LVL", 0.75),       // TRACK 3
            SEND_DLY_INFO,                  // TRACK 4
            SEND_RVB_INFO,                  // TRACK 5
            resv(),                         // TRACK 6
            resv(),                         // TRACK 7
            resv(),                         // MOD 0
            resv(),                         // MOD 1
            LFO1_RATE_INFO,                 // MOD 2
            LFO1_DEPTH_INFO,                // MOD 3
            LFO1_DEST_INFO,                 // MOD 4
            LFO2_RATE_INFO,                 // MOD 5
            LFO2_DEPTH_INFO,                // MOD 6
            LFO2_DEST_INFO,                 // MOD 7
        ],
    },
    // 7: CP
    MachineInfo {
        macros: [
            mi("TUNE", "TUN", 0.3),         // MACH 0
            mi("RATIO", "RTO", 0.5),        // MACH 1
            resv(),                         // MACH 2
            resv(),                         // MACH 3
            resv(),                         // MACH 4
            mi("BDEC", "BDC", 0.2),         // MACH 5
            mi("NDEC", "NDC", 0.3),         // MACH 6
            mi("BAL", "BAL", 0.8),          // MACH 7
            mi("HPF", "HPF", 0.2),          // FILT 0
            mi("LPF", "LPF", 0.5),          // FILT 1
            STRIP_CUT_INFO,                 // FILT 2
            STRIP_RESO_INFO,                // FILT 3
            STRIP_ATK_INFO,                 // FILT 4
            STRIP_HOLD_INFO,                // FILT 5
            STRIP_DEC_INFO,                 // FILT 6
            resv(),                         // FILT 7
            MACH_INFO,                      // TRACK 0
            OUT_INFO,                       // TRACK 1
            PAN_INFO,                       // TRACK 2
            mi("LEVEL", "LVL", 0.7),        // TRACK 3
            SEND_DLY_INFO,                  // TRACK 4
            SEND_RVB_INFO,                  // TRACK 5
            resv(),                         // TRACK 6
            resv(),                         // TRACK 7
            resv(),                         // MOD 0
            resv(),                         // MOD 1
            LFO1_RATE_INFO,                 // MOD 2
            LFO1_DEPTH_INFO,                // MOD 3
            LFO1_DEST_INFO,                 // MOD 4
            LFO2_RATE_INFO,                 // MOD 5
            LFO2_DEPTH_INFO,                // MOD 6
            LFO2_DEST_INFO,                 // MOD 7
        ],
    },
    // 8: Hat Classic
    MachineInfo {
        macros: [
            resv(),                         // MACH 0
            resv(),                         // MACH 1
            resv(),                         // MACH 2
            resv(),                         // MACH 3
            resv(),                         // MACH 4
            mi("DEC", "DEC", 0.092),        // MACH 5
            resv(),                         // MACH 6
            resv(),                         // MACH 7
            mi("HPF", "HPF", 0.45),         // FILT 0
            mi("LPF", "LPF", 0.75),         // FILT 1
            STRIP_CUT_INFO,                 // FILT 2
            STRIP_RESO_INFO,                // FILT 3
            STRIP_ATK_INFO,                 // FILT 4
            STRIP_HOLD_INFO,                // FILT 5
            STRIP_DEC_INFO,                 // FILT 6
            resv(),                         // FILT 7
            MACH_INFO,                      // TRACK 0
            OUT_INFO,                       // TRACK 1
            PAN_INFO,                       // TRACK 2
            mi("LEVEL", "LVL", 0.4),        // TRACK 3
            SEND_DLY_INFO,                  // TRACK 4
            SEND_RVB_INFO,                  // TRACK 5
            resv(),                         // TRACK 6
            resv(),                         // TRACK 7
            resv(),                         // MOD 0
            resv(),                         // MOD 1
            LFO1_RATE_INFO,                 // MOD 2
            LFO1_DEPTH_INFO,                // MOD 3
            LFO1_DEST_INFO,                 // MOD 4
            LFO2_RATE_INFO,                 // MOD 5
            LFO2_DEPTH_INFO,                // MOD 6
            LFO2_DEST_INFO,                 // MOD 7
        ],
    },
    // 9: HH Basic
    MachineInfo {
        macros: [
            mi("TUNE", "TUN", 0.3),         // MACH 0
            mi("TONE", "TON", 0.5),         // MACH 1
            resv(),                         // MACH 2
            resv(),                         // MACH 3
            resv(),                         // MACH 4
            mi("DEC", "DEC", 0.092),        // MACH 5
            mi("TDEC", "TDC", 0.3),         // MACH 6
            mi("RST", "RST", 1.0),          // MACH 7
            mi("BPF", "BPF", 0.5),          // FILT 0
            resv(),                         // FILT 1
            STRIP_CUT_INFO,                 // FILT 2
            STRIP_RESO_INFO,                // FILT 3
            STRIP_ATK_INFO,                 // FILT 4
            STRIP_HOLD_INFO,                // FILT 5
            STRIP_DEC_INFO,                 // FILT 6
            resv(),                         // FILT 7
            MACH_INFO,                      // TRACK 0
            OUT_INFO,                       // TRACK 1
            PAN_INFO,                       // TRACK 2
            mi("LEVEL", "LVL", 0.8),        // TRACK 3
            SEND_DLY_INFO,                  // TRACK 4
            SEND_RVB_INFO,                  // TRACK 5
            resv(),                         // TRACK 6
            resv(),                         // TRACK 7
            resv(),                         // MOD 0
            resv(),                         // MOD 1
            LFO1_RATE_INFO,                 // MOD 2
            LFO1_DEPTH_INFO,                // MOD 3
            LFO1_DEST_INFO,                 // MOD 4
            LFO2_RATE_INFO,                 // MOD 5
            LFO2_DEPTH_INFO,                // MOD 6
            LFO2_DEST_INFO,                 // MOD 7
        ],
    },
    // 10: CY Metallic
    MachineInfo {
        macros: [
            mi("TUNE", "TUN", 0.2),         // MACH 0
            mi("TONE", "TON", 0.3),         // MACH 1
            resv(),                         // MACH 2
            resv(),                         // MACH 3
            resv(),                         // MACH 4
            mi("DEC", "DEC", 0.3),          // MACH 5
            mi("TDEC", "TDC", 0.25),        // MACH 6
            resv(),                         // MACH 7
            mi("NCOL", "NCL", 0.3),         // FILT 0
            resv(),                         // FILT 1
            STRIP_CUT_INFO,                 // FILT 2
            STRIP_RESO_INFO,                // FILT 3
            STRIP_ATK_INFO,                 // FILT 4
            STRIP_HOLD_INFO,                // FILT 5
            STRIP_DEC_INFO,                 // FILT 6
            resv(),                         // FILT 7
            MACH_INFO,                      // TRACK 0
            OUT_INFO,                       // TRACK 1
            PAN_INFO,                       // TRACK 2
            mi("LEVEL", "LVL", 0.5),        // TRACK 3
            SEND_DLY_INFO,                  // TRACK 4
            SEND_RVB_INFO,                  // TRACK 5
            resv(),                         // TRACK 6
            resv(),                         // TRACK 7
            resv(),                         // MOD 0
            resv(),                         // MOD 1
            LFO1_RATE_INFO,                 // MOD 2
            LFO1_DEPTH_INFO,                // MOD 3
            LFO1_DEST_INFO,                 // MOD 4
            LFO2_RATE_INFO,                 // MOD 5
            LFO2_DEPTH_INFO,                // MOD 6
            LFO2_DEST_INFO,                 // MOD 7
        ],
    },
    // 11: CB Classic
    MachineInfo {
        macros: [
            mi("TUNE", "TUN", 0.4),         // MACH 0
            mi("DET", "DET", 0.86),         // MACH 1
            resv(),                         // MACH 2
            resv(),                         // MACH 3
            resv(),                         // MACH 4
            mi("DEC", "DEC", 0.15),         // MACH 5
            resv(),                         // MACH 6
            resv(),                         // MACH 7
            mi("BPF", "BPF", 0.35),         // FILT 0
            resv(),                         // FILT 1
            STRIP_CUT_INFO,                 // FILT 2
            STRIP_RESO_INFO,                // FILT 3
            STRIP_ATK_INFO,                 // FILT 4
            STRIP_HOLD_INFO,                // FILT 5
            STRIP_DEC_INFO,                 // FILT 6
            resv(),                         // FILT 7
            MACH_INFO,                      // TRACK 0
            OUT_INFO,                       // TRACK 1
            PAN_INFO,                       // TRACK 2
            mi("LEVEL", "LVL", 0.55),       // TRACK 3
            SEND_DLY_INFO,                  // TRACK 4
            SEND_RVB_INFO,                  // TRACK 5
            resv(),                         // TRACK 6
            resv(),                         // TRACK 7
            resv(),                         // MOD 0
            resv(),                         // MOD 1
            LFO1_RATE_INFO,                 // MOD 2
            LFO1_DEPTH_INFO,                // MOD 3
            LFO1_DEST_INFO,                 // MOD 4
            LFO2_RATE_INFO,                 // MOD 5
            LFO2_DEPTH_INFO,                // MOD 6
            LFO2_DEST_INFO,                 // MOD 7
        ],
    },
    // 12: SY Tone
    MachineInfo {
        macros: [
            mi("TUNE", "TUN", 0.5),         // MACH 0
            mi("RATIO", "RTO", 0.25),       // MACH 1
            mi("FDBK", "FDB", 0.2),         // MACH 2
            mi("MOD.AMT", "MDA", 0.4),      // MACH 3
            mi("MENV", "MEN", 0.25),        // MACH 4
            mi("DEC", "DEC", 0.3),          // MACH 5
            resv(),                         // MACH 6
            resv(),                         // MACH 7
            resv(),                         // FILT 0
            resv(),                         // FILT 1
            STRIP_CUT_INFO,                 // FILT 2
            STRIP_RESO_INFO,                // FILT 3
            STRIP_ATK_INFO,                 // FILT 4
            STRIP_HOLD_INFO,                // FILT 5
            STRIP_DEC_INFO,                 // FILT 6
            resv(),                         // FILT 7
            MACH_INFO,                      // TRACK 0
            OUT_INFO,                       // TRACK 1
            PAN_INFO,                       // TRACK 2
            mi("LEVEL", "LVL", 0.7),        // TRACK 3
            SEND_DLY_INFO,                  // TRACK 4
            SEND_RVB_INFO,                  // TRACK 5
            resv(),                         // TRACK 6
            resv(),                         // TRACK 7
            resv(),                         // MOD 0
            resv(),                         // MOD 1
            LFO1_RATE_INFO,                 // MOD 2
            LFO1_DEPTH_INFO,                // MOD 3
            LFO1_DEST_INFO,                 // MOD 4
            LFO2_RATE_INFO,                 // MOD 5
            LFO2_DEPTH_INFO,                // MOD 6
            LFO2_DEST_INFO,                 // MOD 7
        ],
    },
    // 13: Dub Siren
    MachineInfo {
        macros: [
            mi("TUNE", "TUN", 0.3),         // MACH 0
            mi("DEPTH", "DPT", 0.5),        // MACH 1
            mi("RATE", "RTE", 0.2),         // MACH 2
            resv(),                         // MACH 3
            resv(),                         // MACH 4
            mi("DEC", "DEC", 0.255),        // MACH 5
            resv(),                         // MACH 6
            mi("SHAPE", "SHP", 0.5),        // MACH 7
            resv(),                         // FILT 0
            resv(),                         // FILT 1
            STRIP_CUT_INFO,                 // FILT 2
            STRIP_RESO_INFO,                // FILT 3
            STRIP_ATK_INFO,                 // FILT 4
            STRIP_HOLD_INFO,                // FILT 5
            STRIP_DEC_INFO,                 // FILT 6
            resv(),                         // FILT 7
            MACH_INFO,                      // TRACK 0
            OUT_INFO,                       // TRACK 1
            PAN_INFO,                       // TRACK 2
            mi("LEVEL", "LVL", 0.8),        // TRACK 3
            SEND_DLY_INFO,                  // TRACK 4
            SEND_RVB_INFO,                  // TRACK 5
            resv(),                         // TRACK 6
            resv(),                         // TRACK 7
            resv(),                         // MOD 0
            resv(),                         // MOD 1
            LFO1_RATE_INFO,                 // MOD 2
            LFO1_DEPTH_INFO,                // MOD 3
            LFO1_DEST_INFO,                 // MOD 4
            LFO2_RATE_INFO,                 // MOD 5
            LFO2_DEPTH_INFO,                // MOD 6
            LFO2_DEST_INFO,                 // MOD 7
        ],
    },
    // 14: Sweep FX
    MachineInfo {
        macros: [
            mi("RATE", "RTE", 0.2),         // MACH 0
            mi("DEPTH", "DPT", 0.5),        // MACH 1
            mi("START", "STR", 0.45),       // MACH 2
            resv(),                         // MACH 3
            resv(),                         // MACH 4
            mi("DEC", "DEC", 0.255),        // MACH 5
            resv(),                         // MACH 6
            mi("MODE", "MOD", 0.0),         // MACH 7
            resv(),                         // FILT 0
            mi("RESO", "RES", 0.2),         // FILT 1
            STRIP_CUT_INFO,                 // FILT 2
            STRIP_RESO_INFO,                // FILT 3
            STRIP_ATK_INFO,                 // FILT 4
            STRIP_HOLD_INFO,                // FILT 5
            STRIP_DEC_INFO,                 // FILT 6
            resv(),                         // FILT 7
            MACH_INFO,                      // TRACK 0
            OUT_INFO,                       // TRACK 1
            PAN_INFO,                       // TRACK 2
            mi("LEVEL", "LVL", 0.7),        // TRACK 3
            SEND_DLY_INFO,                  // TRACK 4
            SEND_RVB_INFO,                  // TRACK 5
            resv(),                         // TRACK 6
            resv(),                         // TRACK 7
            resv(),                         // MOD 0
            resv(),                         // MOD 1
            LFO1_RATE_INFO,                 // MOD 2
            LFO1_DEPTH_INFO,                // MOD 3
            LFO1_DEST_INFO,                 // MOD 4
            LFO2_RATE_INFO,                 // MOD 5
            LFO2_DEPTH_INFO,                // MOD 6
            LFO2_DEST_INFO,                 // MOD 7
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
    /// Virtual-analogue bridged-T kick.
    BdVa(BdVa),
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
    /// Dub siren: sine + internal pitch LFO, AHD gesture.
    DubSiren(DubSiren),
    /// Sweep FX: noise through swept SVF, AHD gesture.
    SweepFx(SweepFx),
}

impl MachineSlot {
    /// Build a slot of the given machine with the given macros applied.
    pub fn new(id: MachineId, macros: &[f32; NUM_MACROS]) -> Self {
        match id {
            MachineId::BdClassic => Self::BdClassic(BdClassic::new(macros)),
            MachineId::BdFm => Self::BdFm(BdFm::new(macros)),
            MachineId::BdVa => Self::BdVa(BdVa::new(macros)),
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
            MachineId::DubSiren => Self::DubSiren(DubSiren::new(macros)),
            MachineId::SweepFx => Self::SweepFx(SweepFx::new(macros)),
        }
    }

    /// Which machine this slot holds.
    pub fn id(&self) -> MachineId {
        match self {
            Self::BdClassic(_) => MachineId::BdClassic,
            Self::BdFm(_) => MachineId::BdFm,
            Self::BdVa(_) => MachineId::BdVa,
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
            Self::DubSiren(_) => MachineId::DubSiren,
            Self::SweepFx(_) => MachineId::SweepFx,
        }
    }

    /// Recompute coefficients from the supplied macros. Setup rate.
    pub fn set_macros(&mut self, macros: &[f32; NUM_MACROS]) {
        match self {
            Self::BdClassic(m) => m.set_macros(macros),
            Self::BdFm(m) => m.set_macros(macros),
            Self::BdVa(m) => m.set_macros(macros),
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
            Self::DubSiren(m) => m.set_macros(macros),
            Self::SweepFx(m) => m.set_macros(macros),
        }
    }

    /// Begin a hit.
    pub fn trigger(&mut self, velocity: f32) {
        match self {
            Self::BdClassic(m) => m.trigger(velocity),
            Self::BdFm(m) => m.trigger(velocity),
            Self::BdVa(m) => m.trigger(velocity),
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
            Self::DubSiren(m) => m.trigger(velocity),
            Self::SweepFx(m) => m.trigger(velocity),
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
            Self::BdVa(m) => m.retune(semis),
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
            Self::DubSiren(m) => m.retune(semis),
            Self::SweepFx(m) => m.retune(semis),
        }
    }

    /// Force to silence.
    pub fn reset(&mut self) {
        match self {
            Self::BdClassic(m) => m.reset(),
            Self::BdFm(m) => m.reset(),
            Self::BdVa(m) => m.reset(),
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
            Self::DubSiren(m) => m.reset(),
            Self::SweepFx(m) => m.reset(),
        }
    }

    /// Still producing output?
    pub fn is_active(&self) -> bool {
        match self {
            Self::BdClassic(m) => m.is_active(),
            Self::BdFm(m) => m.is_active(),
            Self::BdVa(m) => m.is_active(),
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
            Self::DubSiren(m) => m.is_active(),
            Self::SweepFx(m) => m.is_active(),
        }
    }

    /// One sample of machine output, *pre-track-strip*.
    #[inline(always)]
    pub fn tick(&mut self) -> f32 {
        match self {
            Self::BdClassic(m) => m.tick(),
            Self::BdFm(m) => m.tick(),
            Self::BdVa(m) => m.tick(),
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
            Self::DubSiren(m) => m.tick(),
            Self::SweepFx(m) => m.tick(),
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
        assert_eq!(m.macro_by_name("TUNE").map(|(i, _)| i), Some(SLOT_MACH_0));
        assert_eq!(m.macro_by_name("tune").map(|(i, _)| i), Some(SLOT_MACH_0));
        assert_eq!(m.macro_by_name("DEC").map(|(i, _)| i), Some(SLOT_MACH_5));
        assert_eq!(m.macro_by_name("MISSING"), None);
    }

    #[test]
    fn canonical_slot_constants_are_stable() {
        assert_eq!(NUM_MACROS, 32);
        assert_eq!(NUM_BANKS, 4);
        assert_eq!(MACROS_PER_BANK, 8);
        // Bank 0: MACH (numbered)
        assert_eq!(SLOT_MACH_0, 0);
        assert_eq!(SLOT_MACH_1, 1);
        assert_eq!(SLOT_MACH_2, 2);
        assert_eq!(SLOT_MACH_3, 3);
        assert_eq!(SLOT_MACH_4, 4);
        assert_eq!(SLOT_MACH_5, 5);
        assert_eq!(SLOT_MACH_6, 6);
        assert_eq!(SLOT_MACH_7, 7);
        // Bank 1: FILT
        assert_eq!(SLOT_FILT_0, 8);
        assert_eq!(SLOT_FILT_1, 9);
        assert_eq!(SLOT_STRIP_CUT, 10);
        assert_eq!(SLOT_STRIP_RESO, 11);
        assert_eq!(SLOT_STRIP_ATK, 12);
        assert_eq!(SLOT_STRIP_HOLD, 13);
        assert_eq!(SLOT_STRIP_DEC, 14);
        // Bank 2: TRACK
        assert_eq!(SLOT_MACHINE, 16);
        assert_eq!(SLOT_OUT, 17);
        assert_eq!(SLOT_PAN, 18);
        assert_eq!(SLOT_LEVEL, 19);
        assert_eq!(SLOT_SEND_DELAY, 20);
        assert_eq!(SLOT_SEND_REVERB, 21);
        // Bank 3: MOD (LFOs only)
        assert_eq!(SLOT_LFO1_RATE, 26);
        assert_eq!(SLOT_LFO1_DEPTH, 27);
        assert_eq!(SLOT_LFO1_DEST, 28);
        assert_eq!(SLOT_LFO2_RATE, 29);
        assert_eq!(SLOT_LFO2_DEPTH, 30);
        assert_eq!(SLOT_LFO2_DEST, 31);
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
