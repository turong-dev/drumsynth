//! Parameter system: the flat array, and the `(group, index)` address over it.
//!
//! Every device exposes its own count of normalised (0..1) parameters, its
//! `NUM_PARAMS`, which may not exceed [`MAX_PARAMS`]. The current devices lay
//! theirs out as [`NUM_BANKS`] banks of [`PARAMS_PER_BANK`], mirroring the
//! Syntakt PITCH/FILTER/AMP/MOD pages. The flat slot index is
//! `bank * 8 + index` ([`param_index`]). A fixed layout lets the same
//! parameter be a knob on one machine and a different-but-equivalent knob on
//! another. Slots a machine does not use are `RESV` (default 0.0) and
//! ignored.
//!
//! | Bank | Slots | Contents |
//! |------|-------|-----------------------|
//! | 0    | 0–7   | MACH (tune, sweep, FM, decay, shape) |
//! | 1    | 8–15  | FILT (machine filter + strip SVF + strip AHD env) |
//! | 2    | 16–23 | TRACK (machine select, output, pan, level, sends) |
//! | 3    | 24–31 | MOD (FM mod amount/env + LFOs) |
//!
//! **The flat index is internal.** It was the wire ABI while the control
//! surface was `CC 20 + flat`; that map is gone (phase F of #1), and NRPN
//! `(group, index)` is the only external address. The banks survive as the
//! layout convention a machine's `set_params` reads, nothing more, and
//! reordering them is now a refactor rather than a break.
//!
//! Each row also carries a [`ParamRoute`]: where the value goes when the
//! parameter is set. [`Track::set_param`](crate::track::Track::set_param)
//! dispatches on it instead of comparing flat indices, so a device states its
//! routing in its own table — mi-drum's strip/shaper rows are
//! [`ParamRoute::Slot`] because it runs that chain inside the voice, while
//! the drum device's same positions route to the core strip.
//!
//! ## Groups: the external address
//!
//! Each row carries a [`ParamGroup`] — what the parameter *is*:
//! [`Voice`](ParamGroup::Voice), [`Filter`](ParamGroup::Filter),
//! [`Shaper`](ParamGroup::Shaper), [`Lfo`](ParamGroup::Lfo), and so on. A
//! [`ParamId`] is `{ group, index }`, where `index` is the row's ordinal
//! among rows carrying that group, and [`resolve`] walks the table to find
//! it. NRPN carries that pair: MSB selects the group, LSB the index (see
//! [`crate::midi`]).
//!
//! Groups and banks are independent. A group is a label on a parameter, not a
//! reserved range, so it has no declared size and a device adds a parameter
//! by appending a row. mi-drum's SHAPER rows are scattered at flat 8, 9, 13,
//! 14, 15, 22 and 23, and nothing needs them otherwise: contiguity would only
//! buy an O(1) base-offset lookup, and the lookup is control rate.
//!
//! Resolution is therefore a linear scan. It runs when an NRPN message
//! arrives — never on the audio path — and 33 rows is ~33 comparisons per
//! message.
//!
//! [`ParamGroup::Reserved`] rows are skipped while counting ordinals and
//! never resolve at all, so a slot a machine does not use stays a hole in the
//! address space rather than a target a controller can reach.
//!
//! ## Kinds
//!
//! [`ParamKind`] says what the 0..1 value *means*: a continuous sweep, a
//! bipolar control centred at 0.5, or a choice among `count` discrete
//! options. It is display and quantisation data, not routing: the grid picks
//! a widget from it, the cheat-sheet prints it, and
//! [`apply_nrpn`](crate::midi::apply_nrpn) snaps a discrete parameter onto an
//! exact option instead of landing between two of them.

/// Most param knobs a device may expose.
///
/// The track's pending-smoothing mask holds one bit per param in
/// [`PARAM_MASK_WORDS`] words, so this is its ceiling. A device with more
/// params fails to build the first time its track is instantiated.
pub const MAX_PARAMS: usize = 256;

/// Words in a track's pending-smoothing mask: one bit per param, up to
/// [`MAX_PARAMS`].
pub const PARAM_MASK_WORDS: usize = MAX_PARAMS / 32;

/// Number of param banks, mirroring the Syntakt PITCH/FILTER/AMP/MOD pages.
pub const NUM_BANKS: usize = 4;

/// Params per bank. Each bank maps to a 0–7 group on the Syntakt UI. Four
/// banks of eight is the 32-slot layout both devices use today.
pub const PARAMS_PER_BANK: usize = 8;

/// Flat slot index for a param, from its bank and in-bank position.
///
/// `bank * PARAMS_PER_BANK + index`. Internal: the external address is
/// `(group, index)` — see [`ParamId`].
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

// MACH slots (bank 0). Numbered — each machine lays out its
// own parameters here. The name/abbrev/default for each slot is defined
// per-machine in the device's metadata table.
/// MACH bank slot 0. Machine-specific.
pub const SLOT_MACH_0: usize = param_index(BANK_MACH, 0);
/// MACH bank slot 1. Machine-specific.
pub const SLOT_MACH_1: usize = param_index(BANK_MACH, 1);
/// MACH bank slot 2. Machine-specific.
pub const SLOT_MACH_2: usize = param_index(BANK_MACH, 2);
/// MACH bank slot 3. Machine-specific.
pub const SLOT_MACH_3: usize = param_index(BANK_MACH, 3);
/// MACH bank slot 4. Machine-specific.
pub const SLOT_MACH_4: usize = param_index(BANK_MACH, 4);
/// MACH bank slot 5. Machine-specific.
pub const SLOT_MACH_5: usize = param_index(BANK_MACH, 5);
/// MACH bank slot 6. Machine-specific.
pub const SLOT_MACH_6: usize = param_index(BANK_MACH, 6);
/// MACH bank slot 7. Machine-specific.
pub const SLOT_MACH_7: usize = param_index(BANK_MACH, 7);

// FILT slots (bank 1).
/// FILT bank: machine-internal filter slot 0. Numbered — meaning
/// varies per machine (HPF, BPF, etc.).
pub const SLOT_FILT_0: usize = param_index(BANK_FILT, 0);
/// FILT bank: machine-internal filter slot 1. Numbered — meaning
/// varies per machine (LPF, resonance, Q, etc.).
pub const SLOT_FILT_1: usize = param_index(BANK_FILT, 1);
/// FILT bank: per-track strip SVF cutoff (log-mapped 20 Hz..20 kHz).
/// Track-routed: drives the device strip's cutoff, not the machine.
pub const SLOT_STRIP_CUT: usize = param_index(BANK_FILT, 2);
/// FILT bank: per-track strip SVF resonance (0.5..20 Q).
/// Track-routed: drives the device strip's resonance, not the machine.
pub const SLOT_STRIP_RESO: usize = param_index(BANK_FILT, 3);
/// FILT bank: per-track strip AHD attack (0..1 s).
/// Track-routed: drives the device strip's attack.
pub const SLOT_STRIP_ATK: usize = param_index(BANK_FILT, 4);
/// FILT bank: per-track strip AHD hold (0..10 s).
/// Track-routed: drives the device strip's hold.
pub const SLOT_STRIP_HOLD: usize = param_index(BANK_FILT, 5);
/// FILT bank: per-track strip AHD decay (0.01..10 s).
/// Track-routed: drives the device strip's decay.
pub const SLOT_STRIP_DEC: usize = param_index(BANK_FILT, 6);

// TRACK slots (bank 2).
/// TRACK bank: machine selector. Track-routed: swaps the machine on
/// this track (quantised by the device catalog).
pub const SLOT_MACHINE: usize = param_index(BANK_TRACK, 0);
/// TRACK bank: per-track output routing. 0..1 quantised over the device's
/// output-pair enum. Track-routed.
pub const SLOT_OUT: usize = param_index(BANK_TRACK, 1);
/// TRACK bank: per-track panning. Track-routed.
pub const SLOT_PAN: usize = param_index(BANK_TRACK, 2);
/// TRACK bank: per-track output level. Track-routed.
pub const SLOT_LEVEL: usize = param_index(BANK_TRACK, 3);
/// TRACK bank: delay send (track-routed). Zero this (and
/// [`SLOT_SEND_REVERB`]) to take the send-FX bus out of the picture when
/// isolating a single track's CPU cost.
pub const SLOT_SEND_DELAY: usize = param_index(BANK_TRACK, 4);
/// TRACK bank: reverb send (track-routed). See [`SLOT_SEND_DELAY`].
pub const SLOT_SEND_REVERB: usize = param_index(BANK_TRACK, 5);

// MOD slots (bank 3). LFOs only.
/// MOD bank: LFO 1 rate. 0..0.5 = slow range (0.1..10 Hz), 0.5..1 = fast
/// range (1..100 Hz), log-mapped within each half. Track-routed.
pub const SLOT_LFO1_RATE: usize = param_index(BANK_MOD, 2);
/// MOD bank: LFO 1 depth (0..1). Track-routed.
pub const SLOT_LFO1_DEPTH: usize = param_index(BANK_MOD, 3);
/// MOD bank: LFO 1 destination (quantised over the device's modulation
/// destination enum). Track-routed.
pub const SLOT_LFO1_DEST: usize = param_index(BANK_MOD, 4);
/// MOD bank: LFO 2 rate. Same split mapping as [`SLOT_LFO1_RATE`].
/// Track-routed.
pub const SLOT_LFO2_RATE: usize = param_index(BANK_MOD, 5);
/// MOD bank: LFO 2 depth (0..1). Track-routed.
pub const SLOT_LFO2_DEPTH: usize = param_index(BANK_MOD, 6);
/// MOD bank: LFO 2 destination (quantised over the device's modulation
/// destination enum). Track-routed.
pub const SLOT_LFO2_DEST: usize = param_index(BANK_MOD, 7);

/// Which group a param belongs to.
///
/// The label on a parameter, not a reserved range: a group has no declared
/// size and adding one means adding a row. It is what an NRPN address keys
/// on — MSB selects the group, LSB selects the param within it — so the
/// identifiers are wire values and must not be renumbered.
///
/// A param's group is independent of its [`ParamRoute`]. Route says where the
/// value goes in the engine; group says what it is called on the wire and on
/// a control surface. mi-drum's shaper rows, for example, are group
/// [`Shaper`](ParamGroup::Shaper) and route [`Slot`](ParamRoute::Slot).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[repr(u8)]
pub enum ParamGroup {
    /// Track-level routing and mixing: machine select, output, pan, level,
    /// sends.
    Track = 0,
    /// Machine-specific synthesis controls. Per machine: Peaks takes four,
    /// Plaits nine (`VOICE` is what makes that difference addressable rather
    /// than a hole in the map).
    Voice = 1,
    /// A filter, machine-internal or strip: cutoff, resonance, FM.
    Filter = 2,
    /// The waveshaping stage: algorithm, timbre, drive, mix, and its
    /// oscillator and I/O taps.
    Shaper = 3,
    /// Amplitude envelope — the strip's AHD on the drum device. mi-drum runs
    /// its envelope inside the voice, so it has no AMP rows.
    Amp = 4,
    /// LFOs: rate, depth, and per-target depths.
    Lfo = 5,
    /// Envelopes other than the amplitude one: attack, decay, and per-target
    /// depths.
    Env = 6,
    /// Reserved for the mod matrix. Not part of this work.
    Matrix = 7,
    /// Channel-independent: master gain, FX bus. Not addressable per track.
    Global = 127,
    /// A [`resv`] row. Never matches a [`ParamId`], so a reserved slot cannot
    /// be addressed — which is the point of it being reserved.
    Reserved = 255,
}

impl ParamGroup {
    /// Map a wire group value onto a [`ParamGroup`], or `None` if this build
    /// has no identifier for it.
    ///
    /// The discriminants are wire values (the enum is `repr(u8)` with
    /// explicit numbers), so the byte arriving on CC 99 lands here. A
    /// controller selecting a group that does not exist is asking for a
    /// parameter this build cannot have; the caller drops the message
    /// rather than guessing at the nearest group. `Reserved` is deliberately
    /// unreachable: a reserved slot is a hole in the address space, not a
    /// target.
    pub const fn from_u8(v: u8) -> Option<Self> {
        match v {
            0 => Some(ParamGroup::Track),
            1 => Some(ParamGroup::Voice),
            2 => Some(ParamGroup::Filter),
            3 => Some(ParamGroup::Shaper),
            4 => Some(ParamGroup::Amp),
            5 => Some(ParamGroup::Lfo),
            6 => Some(ParamGroup::Env),
            7 => Some(ParamGroup::Matrix),
            127 => Some(ParamGroup::Global),
            _ => None,
        }
    }

    /// Short display name, as a control surface and the cheat-sheet print it.
    ///
    /// The same spelling the NRPN documentation uses, so a name read off a
    /// grid tab or a monitor line is the name to look up.
    pub const fn name(self) -> &'static str {
        match self {
            ParamGroup::Track => "TRACK",
            ParamGroup::Voice => "VOICE",
            ParamGroup::Filter => "FILTER",
            ParamGroup::Shaper => "SHAPER",
            ParamGroup::Amp => "AMP",
            ParamGroup::Lfo => "LFO",
            ParamGroup::Env => "ENV",
            ParamGroup::Matrix => "MATRIX",
            ParamGroup::Global => "GLOBAL",
            ParamGroup::Reserved => "RESV",
        }
    }

    /// Every group a device may place a parameter in, in display order.
    ///
    /// [`Global`](ParamGroup::Global) is channel-independent and
    /// [`Reserved`](ParamGroup::Reserved) is a hole, so neither is here:
    /// this is the list a per-track control surface pages through.
    pub const ADDRESSABLE: [ParamGroup; 8] = [
        ParamGroup::Track,
        ParamGroup::Voice,
        ParamGroup::Filter,
        ParamGroup::Shaper,
        ParamGroup::Amp,
        ParamGroup::Lfo,
        ParamGroup::Env,
        ParamGroup::Matrix,
    ];
}

/// What a parameter's normalised `0..1` value means.
///
/// Display and quantisation data, independent of [`ParamRoute`]: the grid
/// picks a widget from it, the cheat-sheet prints it, and
/// [`apply_nrpn`](crate::midi::apply_nrpn) uses
/// [`Discrete`](ParamKind::Discrete) to snap a 14-bit value onto an exact
/// option rather than a float that lands between two of them.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ParamKind {
    /// A continuous sweep over `0..1`. The default, and what most rows are.
    Continuous,
    /// Centred at 0.5, meaningful in both directions — pan, and anything
    /// else a control surface should draw outward from the middle.
    Bipolar,
    /// A choice among `count` options, evenly spaced over `0..1`: option `k`
    /// is the value `k / (count - 1)`.
    ///
    /// `count` is the engine's own option count for the parameter, so a
    /// controller can address an option exactly instead of landing near it.
    Discrete {
        /// Number of options, `1..=255`. A `count` of 0 or 1 has one
        /// position and nothing to choose.
        count: u8,
    },
}

/// An address: a group, and a position within it.
///
/// The external address space for a param, and what NRPN carries. Resolution
/// is a scan of the device's table for the `index`-th row in `group` — see
/// [`resolve`].
///
/// `index` is the entry's ordinal among entries carrying that group, so it is
/// stable under appending a param to a *different* group and shifts only when
/// a param is inserted earlier within its own group.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ParamId {
    /// The group being addressed.
    pub group: ParamGroup,
    /// Position within the group.
    pub index: u8,
}

/// Resolve a [`ParamId`] against a device's parameter table.
///
/// Returns the flat index, or `None` if the group has no such member.
///
/// [`ParamGroup::Reserved`] rows are skipped while counting ordinals *and*
/// the group itself never resolves: a reserved slot is a hole in the address
/// space, not a target a controller can reach.
///
/// A linear scan rather than a base-offset table. The lookup runs when an
/// NRPN message arrives, which is control rate: 33 rows is ~33 comparisons
/// per message, against an M7 at 600 MHz. A `GROUP_BASE` table would make
/// this O(1) and require every group's rows to be contiguous in the flat
/// array for a cost that rounds to zero.
pub const fn resolve(table: &[ParamInfo], id: ParamId) -> Option<usize> {
    if id.group as u8 == ParamGroup::Reserved as u8 {
        return None;
    }
    let mut seen = 0usize;
    let mut i = 0usize;
    while i < table.len() {
        // `as u8` rather than `==`: `PartialEq` is not const-callable, and
        // the enum is `repr(u8)` so the discriminants are the wire values.
        if table[i].group as u8 == id.group as u8 {
            if seen == id.index as usize {
                return Some(i);
            }
            seen += 1;
        }
        i += 1;
    }
    None
}

/// How many rows a device's table places in `group`.
///
/// The number of valid `index` values for that group: a [`ParamId`] with
/// `index < group_len(table, group)` resolves, and one at or above it does
/// not. [`ParamGroup::Reserved`] rows are not counted, matching
/// [`resolve`].
pub const fn group_len(table: &[ParamInfo], group: ParamGroup) -> usize {
    if group as u8 == ParamGroup::Reserved as u8 {
        return 0;
    }
    let mut n = 0usize;
    let mut i = 0usize;
    while i < table.len() {
        if table[i].group as u8 == group as u8 {
            n += 1;
        }
        i += 1;
    }
    n
}

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
/// rendered WAV lines up bit-for-bit against hardware. `group` says what the
/// param is on the wire and on a control surface (see [`ParamGroup`]).
/// `kind` says what its `0..1` value means (see [`ParamKind`]). `route` says
/// where the value goes when the param is set (see [`ParamRoute`]).
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct ParamInfo {
    /// Human label for display surfaces.
    pub name: &'static str,
    /// Short form for OLED / serial.
    pub abbrev: &'static str,
    /// Canonical factory param value.
    pub default: f32,
    /// Which group the param belongs to.
    pub group: ParamGroup,
    /// What the normalised value means.
    pub kind: ParamKind,
    /// Where the value goes when the param is set.
    pub route: ParamRoute,
}

/// Build a [`ParamInfo`] literal for a machine row.
///
/// The group is a required argument rather than a default: a wrong group
/// means the param cannot be reached by its NRPN address, and nothing but
/// being forced to state it at each row catches that. The default route is
/// [`ParamRoute::Slot`] — the voice consumes the value in `set_params` —
/// which is what every machine-specific row wants. The default kind is
/// [`ParamKind::Continuous`]. Rows that are neither chain
/// [`ParamInfo::with_route`] / [`ParamInfo::with_kind`].
pub const fn mi(
    name: &'static str,
    abbrev: &'static str,
    default: f32,
    group: ParamGroup,
) -> ParamInfo {
    ParamInfo {
        name,
        abbrev,
        default,
        group,
        kind: ParamKind::Continuous,
        route: ParamRoute::Slot,
    }
}

impl ParamInfo {
    /// Override the route of a [`mi`] literal, usable in `const` context.
    pub const fn with_route(mut self, route: ParamRoute) -> ParamInfo {
        self.route = route;
        self
    }

    /// Override the kind of a [`mi`] literal, usable in `const` context.
    pub const fn with_kind(mut self, kind: ParamKind) -> ParamInfo {
        self.kind = kind;
        self
    }

    /// Number of discrete options, or `None` for a continuous parameter.
    ///
    /// `Some(n)` means the value grid is `k / (n - 1)` for `k` in `0..n`.
    pub const fn option_count(self) -> Option<u8> {
        match self.kind {
            ParamKind::Discrete { count } if count > 1 => Some(count),
            _ => None,
        }
    }
}

/// A reserved slot: no knob, canonical default 0.0, ignored by the voice.
///
/// Routes as [`ParamRoute::Slot`]: a set lands in the array and is forwarded,
/// and the voice ignores what it does not use — same as it always behaved.
/// Grouped [`Reserved`](ParamGroup::Reserved), so [`resolve`] never matches
/// it and a reserved slot cannot be addressed.
pub const fn resv() -> ParamInfo {
    mi("RESV", "RSV", 0.0, ParamGroup::Reserved)
}

// ---- Track-routed param info ----
// Same on every device: defined once, referenced by every machine metadata row.
// Their routes are what `Track::set_param` dispatches on.

/// MACH selector (TRACK 0), for a device whose catalogue holds `count`
/// machines.
///
/// `count` is an argument rather than a constant because it is the one piece
/// of this row that is per-device: the drum catalogue has 15 entries and
/// mi-drum's has 28. Carrying it in [`ParamKind::Discrete`] is what lets a
/// control surface draw the selector, and NRPN address a machine exactly,
/// without either of them knowing the device.
pub const fn mach_info(count: u8) -> ParamInfo {
    mi("MACH", "MCH", 0.0, ParamGroup::Track)
        .with_kind(ParamKind::Discrete { count })
        .with_route(ParamRoute::Machine)
}
/// Strip SVF cutoff (FILT 2). Default 1.0 = 20 kHz (fully open).
pub const STRIP_CUT_INFO: ParamInfo = mi("STRIP.CUT", "SCUT", 1.0, ParamGroup::Filter)
    .with_route(ParamRoute::Strip(StripParam::Cutoff));
/// Strip SVF resonance (FILT 3). Default ~0.01 = Butterworth Q.
pub const STRIP_RESO_INFO: ParamInfo = mi("STRIP.RESO", "SRES", 0.01, ParamGroup::Filter)
    .with_route(ParamRoute::Strip(StripParam::Reso));
/// Strip AHD attack (FILT 4). Default 0.0 = instant.
pub const STRIP_ATK_INFO: ParamInfo =
    mi("STRIP.ATK", "SATK", 0.0, ParamGroup::Amp).with_route(ParamRoute::Strip(StripParam::Attack));
/// Strip AHD hold (FILT 5). Default 1.0 = 10 s.
pub const STRIP_HOLD_INFO: ParamInfo =
    mi("STRIP.HOLD", "SHLD", 1.0, ParamGroup::Amp).with_route(ParamRoute::Strip(StripParam::Hold));
/// Strip AHD decay (FILT 6). Default 1.0 = 10 s.
pub const STRIP_DEC_INFO: ParamInfo =
    mi("STRIP.DEC", "SDEC", 1.0, ParamGroup::Amp).with_route(ParamRoute::Strip(StripParam::Decay));
/// Strip pan (TRACK 2). Default 0.5 = centre.
pub const PAN_INFO: ParamInfo = mi("PAN", "PAN", 0.5, ParamGroup::Track)
    .with_kind(ParamKind::Bipolar)
    .with_route(ParamRoute::Pan);
/// Delay send (TRACK 4).
pub const SEND_DLY_INFO: ParamInfo =
    mi("SEND.DLY", "SDY", 0.0, ParamGroup::Track).with_route(ParamRoute::Send(SendBus::Delay));
/// Reverb send (TRACK 5).
pub const SEND_RVB_INFO: ParamInfo =
    mi("SEND.RVB", "SRV", 0.0, ParamGroup::Track).with_route(ParamRoute::Send(SendBus::Reverb));
/// Output routing (TRACK 1). Default 0.0 = Master.
pub const OUT_INFO: ParamInfo = mi("OUT", "OUT", 0.0, ParamGroup::Track)
    .with_kind(ParamKind::Discrete {
        count: crate::OutPair::COUNT as u8,
    })
    .with_route(ParamRoute::Out);
/// LFO 1 rate (MOD 2). 0..0.5 slow, 0.5..1 fast.
pub const LFO1_RATE_INFO: ParamInfo =
    mi("LFO1.RATE", "L1R", 0.0, ParamGroup::Lfo).with_route(ParamRoute::Lfo(LfoParam::Lfo1Rate));
/// LFO 1 depth (MOD 3).
pub const LFO1_DEPTH_INFO: ParamInfo =
    mi("LFO1.DEP", "L1D", 0.0, ParamGroup::Lfo).with_route(ParamRoute::Lfo(LfoParam::Lfo1Depth));
/// LFO 1 destination (MOD 4). Default 0.0 = Param(0).
pub const LFO1_DEST_INFO: ParamInfo = mi("LFO1.DST", "L1S", 0.0, ParamGroup::Lfo)
    .with_kind(ParamKind::Discrete {
        count: crate::dsp::ModDest::COUNT as u8,
    })
    .with_route(ParamRoute::Lfo(LfoParam::Lfo1Dest));
/// LFO 2 rate (MOD 5). Same split mapping as [`LFO1_RATE_INFO`].
pub const LFO2_RATE_INFO: ParamInfo =
    mi("LFO2.RATE", "L2R", 0.0, ParamGroup::Lfo).with_route(ParamRoute::Lfo(LfoParam::Lfo2Rate));
/// LFO 2 depth (MOD 6).
pub const LFO2_DEPTH_INFO: ParamInfo =
    mi("LFO2.DEP", "L2D", 0.0, ParamGroup::Lfo).with_route(ParamRoute::Lfo(LfoParam::Lfo2Depth));
/// LFO 2 destination (MOD 7). Default 0.0 = Param(0).
pub const LFO2_DEST_INFO: ParamInfo = mi("LFO2.DST", "L2S", 0.0, ParamGroup::Lfo)
    .with_kind(ParamKind::Discrete {
        count: crate::dsp::ModDest::COUNT as u8,
    })
    .with_route(ParamRoute::Lfo(LfoParam::Lfo2Dest));

/// Track output level (TRACK 3). Per-machine default.
///
/// Routed as [`ParamRoute::Level`]: stored and forwarded, and the voice reads
/// it from the param array to scale its output — not intercepted like the
/// strip or modulation params. The separate route exists so control surfaces
/// can tell a track-level param from a machine one.
pub const fn level_info(default: f32) -> ParamInfo {
    mi("LEVEL", "LVL", default, ParamGroup::Track).with_route(ParamRoute::Level)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn machine_rows_route_to_the_voice() {
        assert_eq!(
            mi("TUNE", "TUN", 0.5, ParamGroup::Voice).route,
            ParamRoute::Slot
        );
        assert_eq!(resv().route, ParamRoute::Slot);
        assert_eq!(
            mi("TUNE", "TUN", 0.5, ParamGroup::Voice)
                .with_route(ParamRoute::Pan)
                .route,
            ParamRoute::Pan
        );
    }

    #[test]
    fn discriminants_are_the_wire_values() {
        // `from_u8` matches these numbers as literals; the match is what
        // keeps them wire values, so renumbering an identifier must not
        // pass silently.
        assert_eq!(ParamGroup::Track as u8, 0);
        assert_eq!(ParamGroup::Voice as u8, 1);
        assert_eq!(ParamGroup::Filter as u8, 2);
        assert_eq!(ParamGroup::Shaper as u8, 3);
        assert_eq!(ParamGroup::Amp as u8, 4);
        assert_eq!(ParamGroup::Lfo as u8, 5);
        assert_eq!(ParamGroup::Env as u8, 6);
        assert_eq!(ParamGroup::Matrix as u8, 7);
        assert_eq!(ParamGroup::Global as u8, 127);
        assert_eq!(ParamGroup::Reserved as u8, 255);
    }

    #[test]
    fn from_u8_covers_the_known_groups_only() {
        assert_eq!(ParamGroup::from_u8(0), Some(ParamGroup::Track));
        assert_eq!(ParamGroup::from_u8(3), Some(ParamGroup::Shaper));
        assert_eq!(ParamGroup::from_u8(7), Some(ParamGroup::Matrix));
        assert_eq!(ParamGroup::from_u8(127), Some(ParamGroup::Global));
        // Between Matrix and Global, and anything above Global but below
        // Reserved, is a group this build has no identifier for.
        assert_eq!(ParamGroup::from_u8(8), None);
        assert_eq!(ParamGroup::from_u8(126), None);
        assert_eq!(ParamGroup::from_u8(128), None);
        assert_eq!(
            ParamGroup::from_u8(255),
            None,
            "Reserved is a row label, not a selectable group"
        );
    }

    #[test]
    fn reserved_rows_cannot_be_addressed() {
        // `resv()` groups itself Reserved, so no ParamId ever resolves to it:
        // a reserved slot stays a hole in the address space, not a target.
        let table = [
            mi("TUNE", "TUN", 0.5, ParamGroup::Voice),
            resv(),
            mi("HARM", "HRM", 0.5, ParamGroup::Voice),
        ];
        assert_eq!(
            resolve(
                &table,
                ParamId {
                    group: ParamGroup::Voice,
                    index: 0
                }
            ),
            Some(0)
        );
        // Index 1 skips the reserved row and finds HARM at 2: the ordinal is
        // counted over rows in the group, and Reserved is not in the group.
        assert_eq!(
            resolve(
                &table,
                ParamId {
                    group: ParamGroup::Voice,
                    index: 1
                }
            ),
            Some(2)
        );
        assert_eq!(
            resolve(
                &table,
                ParamId {
                    group: ParamGroup::Voice,
                    index: 2
                }
            ),
            None
        );
        assert_eq!(
            resolve(
                &table,
                ParamId {
                    group: ParamGroup::Reserved,
                    index: 0
                }
            ),
            None
        );
    }

    #[test]
    fn resolve_walks_a_device_table_by_group() {
        // The shape mi-drum's table has today: SHAPER rows scattered across
        // the flat array (8, 9, 13, 14, 15, 22, 23). Contiguity is not
        // required — the scan finds them regardless.
        let table = [
            mi("TUNE", "TUN", 0.5, ParamGroup::Voice), // 0
            mi("HARM", "HRM", 0.5, ParamGroup::Voice), // 1
            mi("SAL", "SAL", 0.0, ParamGroup::Shaper), // 2
            mi("STM", "STM", 0.5, ParamGroup::Shaper), // 3
            resv(),                                    // 4
            mi("SDR", "SDR", 0.2, ParamGroup::Shaper), // 5
            mi("MCH", "MCH", 0.0, ParamGroup::Track),  // 6
            mi("SIN", "SIN", 0.5, ParamGroup::Shaper), // 7
        ];
        assert_eq!(
            resolve(
                &table,
                ParamId {
                    group: ParamGroup::Shaper,
                    index: 0
                }
            ),
            Some(2)
        );
        assert_eq!(
            resolve(
                &table,
                ParamId {
                    group: ParamGroup::Shaper,
                    index: 2
                }
            ),
            Some(5)
        );
        assert_eq!(
            resolve(
                &table,
                ParamId {
                    group: ParamGroup::Shaper,
                    index: 3
                }
            ),
            Some(7)
        );
        assert_eq!(
            resolve(
                &table,
                ParamId {
                    group: ParamGroup::Shaper,
                    index: 4
                }
            ),
            None
        );
        assert_eq!(
            resolve(
                &table,
                ParamId {
                    group: ParamGroup::Track,
                    index: 0
                }
            ),
            Some(6)
        );
        assert_eq!(
            resolve(
                &table,
                ParamId {
                    group: ParamGroup::Amp,
                    index: 0
                }
            ),
            None
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
        assert_eq!(mach_info(15).route, ParamRoute::Machine);
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
