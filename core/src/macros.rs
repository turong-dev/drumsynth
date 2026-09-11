//! Macro/CC slot system.
//!
//! Every device machine exposes [`NUM_MACROS`] normalised (0..1) macro knobs
//! laid out as [`NUM_BANKS`] banks of [`MACROS_PER_BANK`], mirroring the
//! Syntakt PITCH/FILTER/AMP/MOD pages. The flat slot index is
//! `bank * 8 + index` ([`macro_index`]); MIDI CC is `CC_TRACK_BASE + flat`
//! (20-based), so slot 0 maps to CC 20 and slot 31 maps to CC 51. A fixed
//! layout lets the same macro be a knob on one machine and a
//! different-but-equivalent knob on another. Slots a machine does not use are
//! `RESV` (default 0.0) and ignored. The slot index order is stable across
//! versions — firmware CC mappings depend on it.
//!
//! | Bank | Slots | Group                 |
//! |------|-------|-----------------------|
//! | 0    | 0–7   | MACH (tune, sweep, FM, decay, shape) |
//! | 1    | 8–15  | FILT (machine filter + strip SVF + strip AHD env) |
//! | 2    | 16–23 | TRACK (machine select, output, pan, level, sends) |
//! | 3    | 24–31 | MOD (FM mod amount/env + LFOs) |

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
/// each machine assigns meaning via its metadata table and `set_macros`.
pub const BANK_MACH: usize = 0;
/// FILT bank: machine-internal filter (numbered) + strip SVF + strip AHD env.
pub const BANK_FILT: usize = 1;
/// TRACK bank: track-level routing and mixing (machine select, output, pan, level, sends).
pub const BANK_TRACK: usize = 2;
/// MOD bank: LFOs only.
pub const BANK_MOD: usize = 3;

// MACH slots (bank 0). CC 20 + flat. Numbered — each machine lays out its
// own parameters here. The name/abbrev/default for each slot is defined
// per-machine in the device's metadata table.
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
/// Track-routed: drives the device strip's cutoff, not the machine.
pub const SLOT_STRIP_CUT: usize = macro_index(BANK_FILT, 2);
/// FILT bank: per-track strip SVF resonance (0.5..20 Q). CC 31.
/// Track-routed: drives the device strip's resonance, not the machine.
pub const SLOT_STRIP_RESO: usize = macro_index(BANK_FILT, 3);
/// FILT bank: per-track strip AHD attack (0..1 s). CC 32.
/// Track-routed: drives the device strip's attack.
pub const SLOT_STRIP_ATK: usize = macro_index(BANK_FILT, 4);
/// FILT bank: per-track strip AHD hold (0..10 s). CC 33.
/// Track-routed: drives the device strip's hold.
pub const SLOT_STRIP_HOLD: usize = macro_index(BANK_FILT, 5);
/// FILT bank: per-track strip AHD decay (0.01..10 s). CC 34.
/// Track-routed: drives the device strip's decay.
pub const SLOT_STRIP_DEC: usize = macro_index(BANK_FILT, 6);

// TRACK slots (bank 2). CC 20 + flat.
/// TRACK bank: machine selector. CC 36. Track-routed: swaps the machine on
/// this track (quantised by the device catalog).
pub const SLOT_MACHINE: usize = macro_index(BANK_TRACK, 0);
/// TRACK bank: per-track output routing. 0..1 quantised over the device's
/// output-pair enum. CC 37. Track-routed.
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
/// MOD bank: LFO 1 destination (quantised over the device's modulation
/// destination enum). CC 48. Track-routed.
pub const SLOT_LFO1_DEST: usize = macro_index(BANK_MOD, 4);
/// MOD bank: LFO 2 rate. Same split mapping as [`SLOT_LFO1_RATE`]. CC 49.
/// Track-routed.
pub const SLOT_LFO2_RATE: usize = macro_index(BANK_MOD, 5);
/// MOD bank: LFO 2 depth (0..1). CC 50. Track-routed.
pub const SLOT_LFO2_DEPTH: usize = macro_index(BANK_MOD, 6);
/// MOD bank: LFO 2 destination (quantised over the device's modulation
/// destination enum). CC 51. Track-routed.
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
/// display form (`"TUN"`). `default` is the canonical factory value used by
/// a device's default-macro function — same value on host and target so a
/// rendered WAV lines up bit-for-bit against hardware.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct MacroInfo {
    /// Human label for display surfaces.
    pub name: &'static str,
    /// Short form for OLED / serial.
    pub abbrev: &'static str,
    /// Canonical factory macro value.
    pub default: f32,
}

/// Build a [`MacroInfo`] literal.
pub const fn mi(name: &'static str, abbrev: &'static str, default: f32) -> MacroInfo {
    MacroInfo {
        name,
        abbrev,
        default,
    }
}

/// A reserved slot: no knob, canonical default 0.0, ignored by the voice.
pub const fn resv() -> MacroInfo {
    mi("RESV", "RSV", 0.0)
}

// ---- Track-routed macro info ----
// Same on every device: defined once, referenced by every machine metadata row.

/// MACH selector (TRACK 0). CC 36.
pub const MACH_INFO: MacroInfo = mi("MACH", "MCH", 0.0);
/// Strip SVF cutoff (FILT 2). CC 30. Default 1.0 = 20 kHz (fully open).
pub const STRIP_CUT_INFO: MacroInfo = mi("STRIP.CUT", "SCUT", 1.0);
/// Strip SVF resonance (FILT 3). CC 31. Default ~0.01 = Butterworth Q.
pub const STRIP_RESO_INFO: MacroInfo = mi("STRIP.RESO", "SRES", 0.01);
/// Strip AHD attack (FILT 4). CC 32. Default 0.0 = instant.
pub const STRIP_ATK_INFO: MacroInfo = mi("STRIP.ATK", "SATK", 0.0);
/// Strip AHD hold (FILT 5). CC 33. Default 1.0 = 10 s.
pub const STRIP_HOLD_INFO: MacroInfo = mi("STRIP.HOLD", "SHLD", 1.0);
/// Strip AHD decay (FILT 6). CC 34. Default 1.0 = 10 s.
pub const STRIP_DEC_INFO: MacroInfo = mi("STRIP.DEC", "SDEC", 1.0);
/// Strip pan (TRACK 2). CC 38. Default 0.5 = centre.
pub const PAN_INFO: MacroInfo = mi("PAN", "PAN", 0.5);
/// Delay send (TRACK 4). CC 40.
pub const SEND_DLY_INFO: MacroInfo = mi("SEND.DLY", "SDY", 0.0);
/// Reverb send (TRACK 5). CC 41.
pub const SEND_RVB_INFO: MacroInfo = mi("SEND.RVB", "SRV", 0.0);
/// Output routing (TRACK 1). CC 37. Default 0.0 = Master.
pub const OUT_INFO: MacroInfo = mi("OUT", "OUT", 0.0);
/// LFO 1 rate (MOD 2). CC 46. 0..0.5 slow, 0.5..1 fast.
pub const LFO1_RATE_INFO: MacroInfo = mi("LFO1.RATE", "L1R", 0.0);
/// LFO 1 depth (MOD 3). CC 47.
pub const LFO1_DEPTH_INFO: MacroInfo = mi("LFO1.DEP", "L1D", 0.0);
/// LFO 1 destination (MOD 4). CC 48. Default 0.0 = Macro(0).
pub const LFO1_DEST_INFO: MacroInfo = mi("LFO1.DST", "L1S", 0.0);
/// LFO 2 rate (MOD 5). CC 49. Same split mapping as [`LFO1_RATE_INFO`].
pub const LFO2_RATE_INFO: MacroInfo = mi("LFO2.RATE", "L2R", 0.0);
/// LFO 2 depth (MOD 6). CC 50.
pub const LFO2_DEPTH_INFO: MacroInfo = mi("LFO2.DEP", "L2D", 0.0);
/// LFO 2 destination (MOD 7). CC 51. Default 0.0 = Macro(0).
pub const LFO2_DEST_INFO: MacroInfo = mi("LFO2.DST", "L2S", 0.0);
