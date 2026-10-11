//! Param/CC slot system.
//!
//! Every device machine exposes [`NUM_PARAMS`] normalised (0..1) param knobs
//! laid out as [`NUM_BANKS`] banks of [`PARAMS_PER_BANK`], mirroring the
//! Syntakt PITCH/FILTER/AMP/MOD pages. The flat slot index is
//! `bank * 8 + index` ([`param_index`]); MIDI CC is `CC_TRACK_BASE + flat`
//! (20-based), so slot 0 maps to CC 20 and slot 31 maps to CC 51. A fixed
//! layout lets the same param be a knob on one machine and a
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
//!
//! Each row also carries a [`ParamRoute`]: where the value goes when the param
//! is set. [`Track::set_param`](crate::track::Track::set_param) dispatches on
//! it instead of comparing flat indices, so a device states its routing in its
//! own table — mi-drum's strip/shaper rows are [`ParamRoute::Slot`] because it
//! runs that chain inside the voice, while the drum device's same positions
//! route to the core strip.

/// How many param knobs every machine exposes.
///
/// Deliberately a `const` rather than per-machine so callers can size
/// arrays against the type, not against the largest variant.
pub const NUM_PARAMS: usize = 32;

/// Number of param banks, mirroring the Syntakt PITCH/FILTER/AMP/MOD pages.
pub const NUM_BANKS: usize = 4;

/// Params per bank. Each bank maps to a 0–7 group on the Syntakt UI.
pub const PARAMS_PER_BANK: usize = NUM_PARAMS / NUM_BANKS;

/// Flat slot index for a param, from its bank and in-bank position.
///
/// `bank * PARAMS_PER_BANK + index`; MIDI CC is `CC_TRACK_BASE + flat`.
pub const fn param_index(bank: usize, index: usize) -> usize {
    bank * PARAMS_PER_BANK + index
}

/// MACH bank: machine-specific synthesis controls. Numbered, not named —
/// each machine assigns meaning via its metadata table and `set_params`.
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
pub const SLOT_MACH_0: usize = param_index(BANK_MACH, 0);
/// MACH bank slot 1. CC 21. Machine-specific.
pub const SLOT_MACH_1: usize = param_index(BANK_MACH, 1);
/// MACH bank slot 2. CC 22. Machine-specific.
pub const SLOT_MACH_2: usize = param_index(BANK_MACH, 2);
/// MACH bank slot 3. CC 23. Machine-specific.
pub const SLOT_MACH_3: usize = param_index(BANK_MACH, 3);
/// MACH bank slot 4. CC 24. Machine-specific.
pub const SLOT_MACH_4: usize = param_index(BANK_MACH, 4);
/// MACH bank slot 5. CC 25. Machine-specific.
pub const SLOT_MACH_5: usize = param_index(BANK_MACH, 5);
/// MACH bank slot 6. CC 26. Machine-specific.
pub const SLOT_MACH_6: usize = param_index(BANK_MACH, 6);
/// MACH bank slot 7. CC 27. Machine-specific.
pub const SLOT_MACH_7: usize = param_index(BANK_MACH, 7);

// FILT slots (bank 1). CC 20 + flat.
/// FILT bank: machine-internal filter slot 0. CC 28. Numbered — meaning
/// varies per machine (HPF, BPF, etc.).
pub const SLOT_FILT_0: usize = param_index(BANK_FILT, 0);
/// FILT bank: machine-internal filter slot 1. CC 29. Numbered — meaning
/// varies per machine (LPF, resonance, Q, etc.).
pub const SLOT_FILT_1: usize = param_index(BANK_FILT, 1);
/// FILT bank: per-track strip SVF cutoff (log-mapped 20 Hz..20 kHz). CC 30.
/// Track-routed: drives the device strip's cutoff, not the machine.
pub const SLOT_STRIP_CUT: usize = param_index(BANK_FILT, 2);
/// FILT bank: per-track strip SVF resonance (0.5..20 Q). CC 31.
/// Track-routed: drives the device strip's resonance, not the machine.
pub const SLOT_STRIP_RESO: usize = param_index(BANK_FILT, 3);
/// FILT bank: per-track strip AHD attack (0..1 s). CC 32.
/// Track-routed: drives the device strip's attack.
pub const SLOT_STRIP_ATK: usize = param_index(BANK_FILT, 4);
/// FILT bank: per-track strip AHD hold (0..10 s). CC 33.
/// Track-routed: drives the device strip's hold.
pub const SLOT_STRIP_HOLD: usize = param_index(BANK_FILT, 5);
/// FILT bank: per-track strip AHD decay (0.01..10 s). CC 34.
/// Track-routed: drives the device strip's decay.
pub const SLOT_STRIP_DEC: usize = param_index(BANK_FILT, 6);

// TRACK slots (bank 2). CC 20 + flat.
/// TRACK bank: machine selector. CC 36. Track-routed: swaps the machine on
/// this track (quantised by the device catalog).
pub const SLOT_MACHINE: usize = param_index(BANK_TRACK, 0);
/// TRACK bank: per-track output routing. 0..1 quantised over the device's
/// output-pair enum. CC 37. Track-routed.
pub const SLOT_OUT: usize = param_index(BANK_TRACK, 1);
/// TRACK bank: per-track panning. CC 38. Track-routed.
pub const SLOT_PAN: usize = param_index(BANK_TRACK, 2);
/// TRACK bank: per-track output level. CC 39. Track-routed.
pub const SLOT_LEVEL: usize = param_index(BANK_TRACK, 3);
/// TRACK bank: delay send (track-routed). CC 40. Zero this (and
/// [`SLOT_SEND_REVERB`]) to take the send-FX bus out of the picture when
/// isolating a single track's CPU cost.
pub const SLOT_SEND_DELAY: usize = param_index(BANK_TRACK, 4);
/// TRACK bank: reverb send (track-routed). CC 41. See [`SLOT_SEND_DELAY`].
pub const SLOT_SEND_REVERB: usize = param_index(BANK_TRACK, 5);

// MOD slots (bank 3). CC 20 + flat. LFOs only.
/// MOD bank: LFO 1 rate. 0..0.5 = slow range (0.1..10 Hz), 0.5..1 = fast
/// range (1..100 Hz), log-mapped within each half. CC 46. Track-routed.
pub const SLOT_LFO1_RATE: usize = param_index(BANK_MOD, 2);
/// MOD bank: LFO 1 depth (0..1). CC 47. Track-routed.
pub const SLOT_LFO1_DEPTH: usize = param_index(BANK_MOD, 3);
/// MOD bank: LFO 1 destination (quantised over the device's modulation
/// destination enum). CC 48. Track-routed.
pub const SLOT_LFO1_DEST: usize = param_index(BANK_MOD, 4);
/// MOD bank: LFO 2 rate. Same split mapping as [`SLOT_LFO1_RATE`]. CC 49.
/// Track-routed.
pub const SLOT_LFO2_RATE: usize = param_index(BANK_MOD, 5);
/// MOD bank: LFO 2 depth (0..1). CC 50. Track-routed.
pub const SLOT_LFO2_DEPTH: usize = param_index(BANK_MOD, 6);
/// MOD bank: LFO 2 destination (quantised over the device's modulation
/// destination enum). CC 51. Track-routed.
pub const SLOT_LFO2_DEST: usize = param_index(BANK_MOD, 7);

/// Where a param's value goes when it is set.
///
/// Dispatch data for [`Track::set_param`](crate::track::Track::set_param),
/// which used to intercept flat slot indices in a hard-coded cascade. The
/// route lives on the param's table row instead, so each device states its own
/// routing: mi-drum's strip/shaper rows are [`Slot`](ParamRoute::Slot) because
/// it runs that chain inside the voice (`strip_bypass` is an audio-path
/// concept now, not a control-path one), while the drum device's same
/// positions route to the core strip.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ParamRoute {
    /// The voice consumes the value in `Slot::set_params`. Also what a
    /// reserved row routes as: nothing intercepts it, exactly as before.
    Slot,
    /// A core strip stage: stored, then the strip coefficients are applied.
    /// The value is *not* forwarded to the voice on this set.
    Strip(StripParam),
    /// A modulation-bus param: stored, then the LFO state is applied. Not
    /// forwarded on this set, same as `Strip`.
    Lfo(LfoParam),
    /// Bipolar strip pan: stored, then the cached pan gains are recomputed.
    Pan,
    /// A send-bus level: stored into the strip's aux level and the effective
    /// per-sample value.
    Send(SendBus),
    /// Track output level. Stored and forwarded, like `Slot`: the voice reads
    /// it from the param array. The distinct route exists so control surfaces
    /// can tell a track-level param from a machine one.
    Level,
    /// Output routing: quantised onto `OutPair`, stored canonically, jumps
    /// instantly — smoothing a discrete choice would route to a half-pair.
    Out,
    /// Machine selector: quantised onto the device catalogue and swapped in,
    /// which resets params to the new machine's defaults. Instant.
    Machine,
}

/// Which strip stage a [`ParamRoute::Strip`] param drives.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum StripParam {
    /// Strip SVF cutoff.
    Cutoff,
    /// Strip SVF resonance.
    Reso,
    /// Strip AHD attack.
    Attack,
    /// Strip AHD hold.
    Hold,
    /// Strip AHD decay.
    Decay,
}

/// Which modulation-bus param a [`ParamRoute::Lfo`] row drives.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum LfoParam {
    /// LFO 1 rate.
    Lfo1Rate,
    /// LFO 1 depth.
    Lfo1Depth,
    /// LFO 1 destination.
    Lfo1Dest,
    /// LFO 2 rate.
    Lfo2Rate,
    /// LFO 2 depth.
    Lfo2Depth,
    /// LFO 2 destination.
    Lfo2Dest,
}

/// Which send bus a [`ParamRoute::Send`] param drives.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SendBus {
    /// Delay send.
    Delay,
    /// Reverb send.
    Reverb,
}

/// Metadata for one param knob.
///
/// `name` is the human label (`"TUNE"`); `abbrev` is the short firmware
/// display form (`"TUN"`). `default` is the canonical factory value used by
/// a device's default-param function — same value on host and target so a
/// rendered WAV lines up bit-for-bit against hardware. `route` says where the
/// value goes when the param is set (see [`ParamRoute`]).
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct ParamInfo {
    /// Human label for display surfaces.
    pub name: &'static str,
    /// Short form for OLED / serial.
    pub abbrev: &'static str,
    /// Canonical factory param value.
    pub default: f32,
    /// Where the value goes when the param is set.
    pub route: ParamRoute,
}

/// Build a [`ParamInfo`] literal for a machine row.
///
/// The default route is [`ParamRoute::Slot`] — the voice consumes the value in
/// `set_params` — which is what every machine-specific row wants. Track-routed
/// rows chain [`with_route`].
pub const fn mi(name: &'static str, abbrev: &'static str, default: f32) -> ParamInfo {
    ParamInfo {
        name,
        abbrev,
        default,
        route: ParamRoute::Slot,
    }
}

impl ParamInfo {
    /// Override the route of a [`mi`] literal, usable in `const` context.
    pub const fn with_route(mut self, route: ParamRoute) -> ParamInfo {
        self.route = route;
        self
    }
}

/// A reserved slot: no knob, canonical default 0.0, ignored by the voice.
///
/// Routes as [`ParamRoute::Slot`]: a set lands in the array and is forwarded,
/// and the voice ignores what it does not use — same as it always behaved.
pub const fn resv() -> ParamInfo {
    mi("RESV", "RSV", 0.0)
}

// ---- Track-routed param info ----
// Same on every device: defined once, referenced by every machine metadata row.
// Their routes are what `Track::set_param` dispatches on.

/// MACH selector (TRACK 0). CC 36.
pub const MACH_INFO: ParamInfo = mi("MACH", "MCH", 0.0).with_route(ParamRoute::Machine);
/// Strip SVF cutoff (FILT 2). CC 30. Default 1.0 = 20 kHz (fully open).
pub const STRIP_CUT_INFO: ParamInfo =
    mi("STRIP.CUT", "SCUT", 1.0).with_route(ParamRoute::Strip(StripParam::Cutoff));
/// Strip SVF resonance (FILT 3). CC 31. Default ~0.01 = Butterworth Q.
pub const STRIP_RESO_INFO: ParamInfo =
    mi("STRIP.RESO", "SRES", 0.01).with_route(ParamRoute::Strip(StripParam::Reso));
/// Strip AHD attack (FILT 4). CC 32. Default 0.0 = instant.
pub const STRIP_ATK_INFO: ParamInfo =
    mi("STRIP.ATK", "SATK", 0.0).with_route(ParamRoute::Strip(StripParam::Attack));
/// Strip AHD hold (FILT 5). CC 33. Default 1.0 = 10 s.
pub const STRIP_HOLD_INFO: ParamInfo =
    mi("STRIP.HOLD", "SHLD", 1.0).with_route(ParamRoute::Strip(StripParam::Hold));
/// Strip AHD decay (FILT 6). CC 34. Default 1.0 = 10 s.
pub const STRIP_DEC_INFO: ParamInfo =
    mi("STRIP.DEC", "SDEC", 1.0).with_route(ParamRoute::Strip(StripParam::Decay));
/// Strip pan (TRACK 2). CC 38. Default 0.5 = centre.
pub const PAN_INFO: ParamInfo = mi("PAN", "PAN", 0.5).with_route(ParamRoute::Pan);
/// Delay send (TRACK 4). CC 40.
pub const SEND_DLY_INFO: ParamInfo =
    mi("SEND.DLY", "SDY", 0.0).with_route(ParamRoute::Send(SendBus::Delay));
/// Reverb send (TRACK 5). CC 41.
pub const SEND_RVB_INFO: ParamInfo =
    mi("SEND.RVB", "SRV", 0.0).with_route(ParamRoute::Send(SendBus::Reverb));
/// Output routing (TRACK 1). CC 37. Default 0.0 = Master.
pub const OUT_INFO: ParamInfo = mi("OUT", "OUT", 0.0).with_route(ParamRoute::Out);
/// LFO 1 rate (MOD 2). CC 46. 0..0.5 slow, 0.5..1 fast.
pub const LFO1_RATE_INFO: ParamInfo =
    mi("LFO1.RATE", "L1R", 0.0).with_route(ParamRoute::Lfo(LfoParam::Lfo1Rate));
/// LFO 1 depth (MOD 3). CC 47.
pub const LFO1_DEPTH_INFO: ParamInfo =
    mi("LFO1.DEP", "L1D", 0.0).with_route(ParamRoute::Lfo(LfoParam::Lfo1Depth));
/// LFO 1 destination (MOD 4). CC 48. Default 0.0 = Param(0).
pub const LFO1_DEST_INFO: ParamInfo =
    mi("LFO1.DST", "L1S", 0.0).with_route(ParamRoute::Lfo(LfoParam::Lfo1Dest));
/// LFO 2 rate (MOD 5). CC 49. Same split mapping as [`LFO1_RATE_INFO`].
pub const LFO2_RATE_INFO: ParamInfo =
    mi("LFO2.RATE", "L2R", 0.0).with_route(ParamRoute::Lfo(LfoParam::Lfo2Rate));
/// LFO 2 depth (MOD 6). CC 50.
pub const LFO2_DEPTH_INFO: ParamInfo =
    mi("LFO2.DEP", "L2D", 0.0).with_route(ParamRoute::Lfo(LfoParam::Lfo2Depth));
/// LFO 2 destination (MOD 7). CC 51. Default 0.0 = Param(0).
pub const LFO2_DEST_INFO: ParamInfo =
    mi("LFO2.DST", "L2S", 0.0).with_route(ParamRoute::Lfo(LfoParam::Lfo2Dest));

/// Track output level (TRACK 3). Per-machine default.
///
/// Routed as [`ParamRoute::Level`]: stored and forwarded, and the voice reads
/// it from the param array to scale its output — not intercepted like the
/// strip or modulation params. The separate route exists so control surfaces
/// can tell a track-level param from a machine one.
pub const fn level_info(default: f32) -> ParamInfo {
    mi("LEVEL", "LVL", default).with_route(ParamRoute::Level)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn machine_rows_route_to_the_voice() {
        assert_eq!(mi("TUNE", "TUN", 0.5).route, ParamRoute::Slot);
        assert_eq!(resv().route, ParamRoute::Slot);
        assert_eq!(
            mi("TUNE", "TUN", 0.5).with_route(ParamRoute::Pan).route,
            ParamRoute::Pan
        );
    }

    #[test]
    fn level_info_is_a_track_row() {
        let info = level_info(0.75);
        assert_eq!(info.name, "LEVEL");
        assert_eq!(info.abbrev, "LVL");
        assert_eq!(info.default, 0.75);
        assert_eq!(info.route, ParamRoute::Level);
    }

    #[test]
    fn shared_track_params_carry_their_routes() {
        assert_eq!(MACH_INFO.route, ParamRoute::Machine);
        assert_eq!(OUT_INFO.route, ParamRoute::Out);
        assert_eq!(PAN_INFO.route, ParamRoute::Pan);
        assert_eq!(SEND_DLY_INFO.route, ParamRoute::Send(SendBus::Delay));
        assert_eq!(SEND_RVB_INFO.route, ParamRoute::Send(SendBus::Reverb));
        assert_eq!(STRIP_CUT_INFO.route, ParamRoute::Strip(StripParam::Cutoff));
        assert_eq!(STRIP_RESO_INFO.route, ParamRoute::Strip(StripParam::Reso));
        assert_eq!(STRIP_ATK_INFO.route, ParamRoute::Strip(StripParam::Attack));
        assert_eq!(STRIP_HOLD_INFO.route, ParamRoute::Strip(StripParam::Hold));
        assert_eq!(STRIP_DEC_INFO.route, ParamRoute::Strip(StripParam::Decay));
        assert_eq!(LFO1_RATE_INFO.route, ParamRoute::Lfo(LfoParam::Lfo1Rate));
        assert_eq!(LFO1_DEPTH_INFO.route, ParamRoute::Lfo(LfoParam::Lfo1Depth));
        assert_eq!(LFO1_DEST_INFO.route, ParamRoute::Lfo(LfoParam::Lfo1Dest));
        assert_eq!(LFO2_RATE_INFO.route, ParamRoute::Lfo(LfoParam::Lfo2Rate));
        assert_eq!(LFO2_DEPTH_INFO.route, ParamRoute::Lfo(LfoParam::Lfo2Depth));
        assert_eq!(LFO2_DEST_INFO.route, ParamRoute::Lfo(LfoParam::Lfo2Dest));
    }
}
