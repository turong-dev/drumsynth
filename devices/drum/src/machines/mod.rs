//! Device-specific drum machine catalog.
//!
//! The Syntakt-architecture machine layer: named synthesis models, enum
//! dispatch via [`MachineSlot`], and the per-machine metadata table
//! [`MACHINE_INFO`]. The generic macro/CC slot system lives in
//! [`device_core::macros`].

pub use device_core::macros::*;

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

/// Per-machine metadata, indexed by [`MachineId::index`].
struct MachineInfo {
    macros: [MacroInfo; NUM_MACROS],
}

static MACHINE_INFO: [MachineInfo; MachineId::COUNT] = [
    // 0: BD Classic
    MachineInfo {
        macros: [
            mi("TUNE", "TUN", 0.2),   // MACH 0
            mi("SWEEP", "SWP", 0.36), // MACH 1
            mi("SWP_T", "SWT", 0.15), // MACH 2
            resv(),                   // MACH 3
            resv(),                   // MACH 4
            mi("DEC", "DEC", 0.255),  // MACH 5
            resv(),                   // MACH 6
            mi("DRIVE", "DRV", 0.16), // MACH 7
            resv(),                   // FILT 0
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
            mi("LEVEL", "LVL", 0.9),  // TRACK 3
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
        ],
    },
    // 1: BD FM
    MachineInfo {
        macros: [
            mi("TUNE", "TUN", 0.2),     // MACH 0
            mi("SWEEP", "SWP", 0.3),    // MACH 1
            mi("SWP_T", "SWT", 0.15),   // MACH 2
            mi("MOD.HZ", "MDH", 0.43),  // MACH 3
            mi("MOD.DC", "MDD", 0.15),  // MACH 4
            mi("DEC", "DEC", 0.255),    // MACH 5
            mi("MOD.AMT", "MDA", 0.35), // MACH 6
            resv(),                     // MACH 7
            resv(),                     // FILT 0
            resv(),                     // FILT 1
            STRIP_CUT_INFO,             // FILT 2
            STRIP_RESO_INFO,            // FILT 3
            STRIP_ATK_INFO,             // FILT 4
            STRIP_HOLD_INFO,            // FILT 5
            STRIP_DEC_INFO,             // FILT 6
            resv(),                     // FILT 7
            MACH_INFO,                  // TRACK 0
            OUT_INFO,                   // TRACK 1
            PAN_INFO,                   // TRACK 2
            mi("LEVEL", "LVL", 0.9),    // TRACK 3
            SEND_DLY_INFO,              // TRACK 4
            SEND_RVB_INFO,              // TRACK 5
            resv(),                     // TRACK 6
            resv(),                     // TRACK 7
            resv(),                     // MOD 0
            resv(),                     // MOD 1
            LFO1_RATE_INFO,             // MOD 2
            LFO1_DEPTH_INFO,            // MOD 3
            LFO1_DEST_INFO,             // MOD 4
            LFO2_RATE_INFO,             // MOD 5
            LFO2_DEPTH_INFO,            // MOD 6
            LFO2_DEST_INFO,             // MOD 7
        ],
    },
    // 2: BD VA
    MachineInfo {
        macros: [
            mi("TUNE", "TUN", 0.28), // MACH 0
            mi("SWEEP", "SWP", 1.0), // MACH 1
            mi("SWP_T", "SWT", 0.1), // MACH 2
            resv(),                  // MACH 3
            resv(),                  // MACH 4
            mi("DEC", "DEC", 0.255), // MACH 5
            resv(),                  // MACH 6
            resv(),                  // MACH 7
            resv(),                  // FILT 0
            mi("Q", "Q", 0.42),      // FILT 1
            STRIP_CUT_INFO,          // FILT 2
            STRIP_RESO_INFO,         // FILT 3
            STRIP_ATK_INFO,          // FILT 4
            STRIP_HOLD_INFO,         // FILT 5
            STRIP_DEC_INFO,          // FILT 6
            resv(),                  // FILT 7
            MACH_INFO,               // TRACK 0
            OUT_INFO,                // TRACK 1
            PAN_INFO,                // TRACK 2
            mi("LEVEL", "LVL", 0.9), // TRACK 3
            SEND_DLY_INFO,           // TRACK 4
            SEND_RVB_INFO,           // TRACK 5
            resv(),                  // TRACK 6
            resv(),                  // TRACK 7
            resv(),                  // MOD 0
            resv(),                  // MOD 1
            LFO1_RATE_INFO,          // MOD 2
            LFO1_DEPTH_INFO,         // MOD 3
            LFO1_DEST_INFO,          // MOD 4
            LFO2_RATE_INFO,          // MOD 5
            LFO2_DEPTH_INFO,         // MOD 6
            LFO2_DEST_INFO,          // MOD 7
        ],
    },
    // 3: Tom
    MachineInfo {
        macros: [
            mi("TUNE", "TUN", 0.35),  // MACH 0
            mi("SWEEP", "SWP", 0.4),  // MACH 1
            mi("SWP_T", "SWT", 0.4),  // MACH 2
            resv(),                   // MACH 3
            resv(),                   // MACH 4
            mi("DEC", "DEC", 0.4),    // MACH 5
            resv(),                   // MACH 6
            mi("STICK", "STK", 0.3),  // MACH 7
            resv(),                   // FILT 0
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
        ],
    },
    // 4: SD Natural
    MachineInfo {
        macros: [
            mi("TUNE", "TUN", 0.28),  // MACH 0
            mi("RATIO", "RTO", 0.48), // MACH 1
            resv(),                   // MACH 2
            resv(),                   // MACH 3
            resv(),                   // MACH 4
            mi("BDEC", "BDC", 0.13),  // MACH 5
            mi("NDEC", "NDC", 0.209), // MACH 6
            mi("NMIX", "NM", 0.62),   // MACH 7
            mi("HPF", "HPF", 0.14),   // FILT 0
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
            mi("LEVEL", "LVL", 0.7),  // TRACK 3
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
        ],
    },
    // 5: SD FM
    MachineInfo {
        macros: [
            mi("TUNE", "TUN", 0.28),   // MACH 0
            mi("RAT", "RAT", 0.33),    // MACH 1
            mi("MOD.AMT", "MDA", 0.3), // MACH 2
            mi("MENV", "MEN", 0.15),   // MACH 3
            resv(),                    // MACH 4
            mi("BDEC", "BDC", 0.13),   // MACH 5
            mi("NDEC", "NDC", 0.209),  // MACH 6
            mi("NMIX", "NM", 0.62),    // MACH 7
            resv(),                    // FILT 0
            resv(),                    // FILT 1
            STRIP_CUT_INFO,            // FILT 2
            STRIP_RESO_INFO,           // FILT 3
            STRIP_ATK_INFO,            // FILT 4
            STRIP_HOLD_INFO,           // FILT 5
            STRIP_DEC_INFO,            // FILT 6
            resv(),                    // FILT 7
            MACH_INFO,                 // TRACK 0
            OUT_INFO,                  // TRACK 1
            PAN_INFO,                  // TRACK 2
            mi("LEVEL", "LVL", 0.7),   // TRACK 3
            SEND_DLY_INFO,             // TRACK 4
            SEND_RVB_INFO,             // TRACK 5
            resv(),                    // TRACK 6
            resv(),                    // TRACK 7
            resv(),                    // MOD 0
            resv(),                    // MOD 1
            LFO1_RATE_INFO,            // MOD 2
            LFO1_DEPTH_INFO,           // MOD 3
            LFO1_DEST_INFO,            // MOD 4
            LFO2_RATE_INFO,            // MOD 5
            LFO2_DEPTH_INFO,           // MOD 6
            LFO2_DEST_INFO,            // MOD 7
        ],
    },
    // 6: RS
    MachineInfo {
        macros: [
            mi("TUNE", "TUN", 0.4),   // MACH 0
            mi("DET", "DET", 0.25),   // MACH 1
            resv(),                   // MACH 2
            resv(),                   // MACH 3
            resv(),                   // MACH 4
            mi("DEC", "DEC", 0.3),    // MACH 5
            mi("NDEC", "NDC", 0.3),   // MACH 6
            mi("NLEV", "NLV", 0.4),   // MACH 7
            mi("HPF", "HPF", 0.3),    // FILT 0
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
            mi("LEVEL", "LVL", 0.75), // TRACK 3
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
        ],
    },
    // 7: CP
    MachineInfo {
        macros: [
            mi("TUNE", "TUN", 0.3),  // MACH 0
            mi("RATIO", "RTO", 0.5), // MACH 1
            resv(),                  // MACH 2
            resv(),                  // MACH 3
            resv(),                  // MACH 4
            mi("BDEC", "BDC", 0.2),  // MACH 5
            mi("NDEC", "NDC", 0.3),  // MACH 6
            mi("BAL", "BAL", 0.8),   // MACH 7
            mi("HPF", "HPF", 0.2),   // FILT 0
            mi("LPF", "LPF", 0.5),   // FILT 1
            STRIP_CUT_INFO,          // FILT 2
            STRIP_RESO_INFO,         // FILT 3
            STRIP_ATK_INFO,          // FILT 4
            STRIP_HOLD_INFO,         // FILT 5
            STRIP_DEC_INFO,          // FILT 6
            resv(),                  // FILT 7
            MACH_INFO,               // TRACK 0
            OUT_INFO,                // TRACK 1
            PAN_INFO,                // TRACK 2
            mi("LEVEL", "LVL", 0.7), // TRACK 3
            SEND_DLY_INFO,           // TRACK 4
            SEND_RVB_INFO,           // TRACK 5
            resv(),                  // TRACK 6
            resv(),                  // TRACK 7
            resv(),                  // MOD 0
            resv(),                  // MOD 1
            LFO1_RATE_INFO,          // MOD 2
            LFO1_DEPTH_INFO,         // MOD 3
            LFO1_DEST_INFO,          // MOD 4
            LFO2_RATE_INFO,          // MOD 5
            LFO2_DEPTH_INFO,         // MOD 6
            LFO2_DEST_INFO,          // MOD 7
        ],
    },
    // 8: Hat Classic
    MachineInfo {
        macros: [
            resv(),                  // MACH 0
            resv(),                  // MACH 1
            resv(),                  // MACH 2
            resv(),                  // MACH 3
            resv(),                  // MACH 4
            mi("DEC", "DEC", 0.092), // MACH 5
            resv(),                  // MACH 6
            resv(),                  // MACH 7
            mi("HPF", "HPF", 0.45),  // FILT 0
            mi("LPF", "LPF", 0.75),  // FILT 1
            STRIP_CUT_INFO,          // FILT 2
            STRIP_RESO_INFO,         // FILT 3
            STRIP_ATK_INFO,          // FILT 4
            STRIP_HOLD_INFO,         // FILT 5
            STRIP_DEC_INFO,          // FILT 6
            resv(),                  // FILT 7
            MACH_INFO,               // TRACK 0
            OUT_INFO,                // TRACK 1
            PAN_INFO,                // TRACK 2
            mi("LEVEL", "LVL", 0.4), // TRACK 3
            SEND_DLY_INFO,           // TRACK 4
            SEND_RVB_INFO,           // TRACK 5
            resv(),                  // TRACK 6
            resv(),                  // TRACK 7
            resv(),                  // MOD 0
            resv(),                  // MOD 1
            LFO1_RATE_INFO,          // MOD 2
            LFO1_DEPTH_INFO,         // MOD 3
            LFO1_DEST_INFO,          // MOD 4
            LFO2_RATE_INFO,          // MOD 5
            LFO2_DEPTH_INFO,         // MOD 6
            LFO2_DEST_INFO,          // MOD 7
        ],
    },
    // 9: HH Basic
    MachineInfo {
        macros: [
            mi("TUNE", "TUN", 0.3),  // MACH 0
            mi("TONE", "TON", 0.5),  // MACH 1
            resv(),                  // MACH 2
            resv(),                  // MACH 3
            resv(),                  // MACH 4
            mi("DEC", "DEC", 0.092), // MACH 5
            mi("TDEC", "TDC", 0.3),  // MACH 6
            mi("RST", "RST", 1.0),   // MACH 7
            mi("BPF", "BPF", 0.5),   // FILT 0
            resv(),                  // FILT 1
            STRIP_CUT_INFO,          // FILT 2
            STRIP_RESO_INFO,         // FILT 3
            STRIP_ATK_INFO,          // FILT 4
            STRIP_HOLD_INFO,         // FILT 5
            STRIP_DEC_INFO,          // FILT 6
            resv(),                  // FILT 7
            MACH_INFO,               // TRACK 0
            OUT_INFO,                // TRACK 1
            PAN_INFO,                // TRACK 2
            mi("LEVEL", "LVL", 0.8), // TRACK 3
            SEND_DLY_INFO,           // TRACK 4
            SEND_RVB_INFO,           // TRACK 5
            resv(),                  // TRACK 6
            resv(),                  // TRACK 7
            resv(),                  // MOD 0
            resv(),                  // MOD 1
            LFO1_RATE_INFO,          // MOD 2
            LFO1_DEPTH_INFO,         // MOD 3
            LFO1_DEST_INFO,          // MOD 4
            LFO2_RATE_INFO,          // MOD 5
            LFO2_DEPTH_INFO,         // MOD 6
            LFO2_DEST_INFO,          // MOD 7
        ],
    },
    // 10: CY Metallic
    MachineInfo {
        macros: [
            mi("TUNE", "TUN", 0.2),  // MACH 0
            mi("TONE", "TON", 0.3),  // MACH 1
            resv(),                  // MACH 2
            resv(),                  // MACH 3
            resv(),                  // MACH 4
            mi("DEC", "DEC", 0.3),   // MACH 5
            mi("TDEC", "TDC", 0.25), // MACH 6
            resv(),                  // MACH 7
            mi("NCOL", "NCL", 0.3),  // FILT 0
            resv(),                  // FILT 1
            STRIP_CUT_INFO,          // FILT 2
            STRIP_RESO_INFO,         // FILT 3
            STRIP_ATK_INFO,          // FILT 4
            STRIP_HOLD_INFO,         // FILT 5
            STRIP_DEC_INFO,          // FILT 6
            resv(),                  // FILT 7
            MACH_INFO,               // TRACK 0
            OUT_INFO,                // TRACK 1
            PAN_INFO,                // TRACK 2
            mi("LEVEL", "LVL", 0.5), // TRACK 3
            SEND_DLY_INFO,           // TRACK 4
            SEND_RVB_INFO,           // TRACK 5
            resv(),                  // TRACK 6
            resv(),                  // TRACK 7
            resv(),                  // MOD 0
            resv(),                  // MOD 1
            LFO1_RATE_INFO,          // MOD 2
            LFO1_DEPTH_INFO,         // MOD 3
            LFO1_DEST_INFO,          // MOD 4
            LFO2_RATE_INFO,          // MOD 5
            LFO2_DEPTH_INFO,         // MOD 6
            LFO2_DEST_INFO,          // MOD 7
        ],
    },
    // 11: CB Classic
    MachineInfo {
        macros: [
            mi("TUNE", "TUN", 0.4),   // MACH 0
            mi("DET", "DET", 0.86),   // MACH 1
            resv(),                   // MACH 2
            resv(),                   // MACH 3
            resv(),                   // MACH 4
            mi("DEC", "DEC", 0.15),   // MACH 5
            resv(),                   // MACH 6
            resv(),                   // MACH 7
            mi("BPF", "BPF", 0.35),   // FILT 0
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
            mi("LEVEL", "LVL", 0.55), // TRACK 3
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
        ],
    },
    // 12: SY Tone
    MachineInfo {
        macros: [
            mi("TUNE", "TUN", 0.5),    // MACH 0
            mi("RATIO", "RTO", 0.25),  // MACH 1
            mi("FDBK", "FDB", 0.2),    // MACH 2
            mi("MOD.AMT", "MDA", 0.4), // MACH 3
            mi("MENV", "MEN", 0.25),   // MACH 4
            mi("DEC", "DEC", 0.3),     // MACH 5
            resv(),                    // MACH 6
            resv(),                    // MACH 7
            resv(),                    // FILT 0
            resv(),                    // FILT 1
            STRIP_CUT_INFO,            // FILT 2
            STRIP_RESO_INFO,           // FILT 3
            STRIP_ATK_INFO,            // FILT 4
            STRIP_HOLD_INFO,           // FILT 5
            STRIP_DEC_INFO,            // FILT 6
            resv(),                    // FILT 7
            MACH_INFO,                 // TRACK 0
            OUT_INFO,                  // TRACK 1
            PAN_INFO,                  // TRACK 2
            mi("LEVEL", "LVL", 0.7),   // TRACK 3
            SEND_DLY_INFO,             // TRACK 4
            SEND_RVB_INFO,             // TRACK 5
            resv(),                    // TRACK 6
            resv(),                    // TRACK 7
            resv(),                    // MOD 0
            resv(),                    // MOD 1
            LFO1_RATE_INFO,            // MOD 2
            LFO1_DEPTH_INFO,           // MOD 3
            LFO1_DEST_INFO,            // MOD 4
            LFO2_RATE_INFO,            // MOD 5
            LFO2_DEPTH_INFO,           // MOD 6
            LFO2_DEST_INFO,            // MOD 7
        ],
    },
    // 13: Dub Siren
    MachineInfo {
        macros: [
            mi("TUNE", "TUN", 0.3),  // MACH 0
            mi("DEPTH", "DPT", 0.5), // MACH 1
            mi("RATE", "RTE", 0.2),  // MACH 2
            resv(),                  // MACH 3
            resv(),                  // MACH 4
            mi("DEC", "DEC", 0.255), // MACH 5
            resv(),                  // MACH 6
            mi("SHAPE", "SHP", 0.5), // MACH 7
            resv(),                  // FILT 0
            resv(),                  // FILT 1
            STRIP_CUT_INFO,          // FILT 2
            STRIP_RESO_INFO,         // FILT 3
            STRIP_ATK_INFO,          // FILT 4
            STRIP_HOLD_INFO,         // FILT 5
            STRIP_DEC_INFO,          // FILT 6
            resv(),                  // FILT 7
            MACH_INFO,               // TRACK 0
            OUT_INFO,                // TRACK 1
            PAN_INFO,                // TRACK 2
            mi("LEVEL", "LVL", 0.8), // TRACK 3
            SEND_DLY_INFO,           // TRACK 4
            SEND_RVB_INFO,           // TRACK 5
            resv(),                  // TRACK 6
            resv(),                  // TRACK 7
            resv(),                  // MOD 0
            resv(),                  // MOD 1
            LFO1_RATE_INFO,          // MOD 2
            LFO1_DEPTH_INFO,         // MOD 3
            LFO1_DEST_INFO,          // MOD 4
            LFO2_RATE_INFO,          // MOD 5
            LFO2_DEPTH_INFO,         // MOD 6
            LFO2_DEST_INFO,          // MOD 7
        ],
    },
    // 14: Sweep FX
    MachineInfo {
        macros: [
            mi("RATE", "RTE", 0.2),   // MACH 0
            mi("DEPTH", "DPT", 0.5),  // MACH 1
            mi("START", "STR", 0.45), // MACH 2
            resv(),                   // MACH 3
            resv(),                   // MACH 4
            mi("DEC", "DEC", 0.255),  // MACH 5
            resv(),                   // MACH 6
            mi("MODE", "MOD", 0.0),   // MACH 7
            resv(),                   // FILT 0
            mi("RESO", "RES", 0.2),   // FILT 1
            STRIP_CUT_INFO,           // FILT 2
            STRIP_RESO_INFO,          // FILT 3
            STRIP_ATK_INFO,           // FILT 4
            STRIP_HOLD_INFO,          // FILT 5
            STRIP_DEC_INFO,           // FILT 6
            resv(),                   // FILT 7
            MACH_INFO,                // TRACK 0
            OUT_INFO,                 // TRACK 1
            PAN_INFO,                 // TRACK 2
            mi("LEVEL", "LVL", 0.7),  // TRACK 3
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

    /// Close the gate on a sounding voice — the note-off path.
    ///
    /// Only the two sustained machines act on it. Every other machine is a
    /// one-shot whose `trigger` started a fixed decay, and a note-off arriving
    /// after the attack has passed must not shorten the hit, so their
    /// `release` is deliberately inert. That distinction is the whole reason
    /// this is a per-machine method rather than a `reset`.
    pub fn release(&mut self) {
        match self {
            Self::DubSiren(m) => m.release(),
            Self::SweepFx(m) => m.release(),
            _ => {}
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
