//! The Launch Control XL 3 UI state machine. No I/O.
//!
//! This module is the part worth protecting. It holds the screens, the
//! hold-versus-tap dispatch, the mapping between a physical
//! `(screen, row, column)` and a `(group, index)` NRPN address, and the
//! repaint model — and it does not know that midir, SysEx or the Launch
//! Control XL 3 exist. Phase 3 of #13 moves it to `core` for the firmware
//! to run; it is written to that crate's rules already (no `alloc`, no
//! `std`, fixed arrays, `f32`) so the move is a move rather than a rewrite.
//!
//! # Shape
//!
//! The removed Monome grid surface (`core/src/grid.rs`, in git history
//! before `e438c57`) proved the shape and it is reused here: an
//! `enum Screen`, a `HOLD_MS` threshold, a held-control record that
//! remembers *which screen was active at press*, and tap-versus-hold
//! dispatch on release. What is different is the surface under it, and that
//! there is now only one of them — this is built for the XL 3 rather than
//! factored for a second consumer that no longer exists.
//!
//! # Columns are tracks
//!
//! The hardware groups itself into eight vertical strips of three encoders,
//! a fader and two buttons. The resting screen follows that: column `c` is
//! track `c` throughout, its fader is that track's level, and its top button
//! selects and auditions it. Only the modal group screens break the
//! correspondence, because there the 24 encoders are 24 parameters of the
//! *selected* track.
//!
//! # Why there is a shadow of the parameter values
//!
//! The driver is the only writer of parameters while it runs, so it can
//! render the screen from its own copy and the engine needs no reverse
//! channel (see #13, "Feedback needs no reverse channel from the audio
//! thread"). [`Surface`] keeps that copy, applies to it exactly what it
//! expects the engine's NRPN router to apply, and
//! `shadow_tracks_the_engine` in this module's tests drives a real
//! `DrumEngine` through the same inputs and asserts the two agree.
//!
//! The one state change that happens underneath the driver is a machine
//! change — and `MACH` is itself a parameter, `(TRACK, 0)`, so the surface
//! sees it go past and reloads the new machine's table and defaults.

use drum_engine::machines::{ParamGroup, ParamId, ParamInfo, ParamKind, ParamRoute};
use drum_engine::midi::{MidiEvent, CHROMATIC_REFERENCE_NOTE, NRPN_MAX};

/// Encoder rows.
pub const ROWS: usize = 3;
/// Columns — faders, buttons per row, and tracks the resting screen shows.
pub const COLUMNS: usize = 8;
/// Encoders on the surface.
pub const ENCODERS: usize = ROWS * COLUMNS;

/// How long a group button must be held before its release counts as the
/// end of a modal rather than as a tap.
///
/// 300 ms is the grid's number, inherited rather than derived: open
/// question 2 of #13 is whether a button under a finger that is already
/// reaching for an encoder wants less. It is a `pub const` so that
/// retuning it is a one-line change with a test to hold it honest.
pub const HOLD_MS: u32 = 300;

/// Note a track-select press auditions with. Middle C — the chromatic
/// reference, so a track sounds at its configured pitch rather than
/// transposed.
pub const AUDITION_NOTE: u8 = CHROMATIC_REFERENCE_NOTE;

/// Velocity a track-select audition uses.
pub const AUDITION_VELOCITY: f32 = 100.0 / 127.0;

/// Most engine events one surface input can produce. A track select
/// produces two: the selection is internal, the audition is a note.
const MAX_EVENTS: usize = 2;

// ---------------------------------------------------------------------------
// Device description
// ---------------------------------------------------------------------------

/// What the surface needs to know about a device, as data.
///
/// Function pointers rather than a trait so the surface is not generic over
/// the engine: it never touches one. The driver builds this from
/// `DeviceModel` + `SlotId`, which every device already implements.
#[derive(Clone, Copy)]
pub struct Catalogue<const N: usize> {
    /// Number of machines in the device's catalogue.
    pub count: usize,
    /// Parameter table for machine `i`.
    pub table: fn(usize) -> &'static [ParamInfo],
    /// Factory parameter values for machine `i` — what a machine change
    /// resets the track to.
    pub defaults: fn(usize) -> [f32; N],
    /// Human-readable name of machine `i`, for the machine selector's
    /// readout.
    pub label: fn(usize) -> &'static str,
}

// ---------------------------------------------------------------------------
// Screens
// ---------------------------------------------------------------------------

/// What the 24 encoders currently are.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Screen {
    /// The resting screen. Column `c` is track `c`; the three encoders in
    /// the column are that track's macros.
    ///
    /// Until #14 lands there are no macros, and the rows fall back to the
    /// `TRACK` group — the one group every machine has, and the one whose
    /// members (level, pan, sends, output) are what you want under your
    /// hands across a whole kit. Everything else on this surface works
    /// unchanged either way.
    Macros,
    /// A parameter group of the *selected* track, as an index into
    /// [`ParamGroup::ADDRESSABLE`].
    Group(u8),
}

/// The screen's name, as structured data.
///
/// Formatting it is the driver's job: this module has no allocator and the
/// device's screen is the driver's business anyway.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Title {
    /// The resting screen, and which page of it.
    Macros {
        /// 0-based page.
        page: u8,
        /// Total pages, at least 1.
        pages: u8,
    },
    /// A group screen.
    Group {
        /// Which group.
        group: ParamGroup,
        /// 0-based page.
        page: u8,
        /// Total pages, at least 1.
        pages: u8,
    },
}

/// A record of a group button that is currently down.
#[derive(Clone, Copy, Debug)]
struct Held {
    /// Which group button, as an [`ParamGroup::ADDRESSABLE`] index.
    group: u8,
    /// When it went down.
    at_ms: u32,
    /// The screen that was active at press, restored if this turns out to
    /// be a hold rather than a tap.
    restore: Screen,
}

// ---------------------------------------------------------------------------
// Addresses
// ---------------------------------------------------------------------------

/// A parameter the surface can edit: a track, and an address within it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Addr {
    /// Track index, which is also the MIDI channel.
    pub track: u8,
    /// The `(group, index)` address, exactly as NRPN carries it.
    pub id: ParamId,
}

/// What a parameter's current value means, for the per-control readout.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Reading {
    /// A continuous sweep, `0.0..=1.0`.
    Continuous(f32),
    /// A bipolar control, `-1.0..=1.0` — the normalised value drawn
    /// outward from its centre.
    Bipolar(f32),
    /// A choice: option `option` of `count`.
    Discrete {
        /// Selected option, `0..count`.
        option: u8,
        /// Number of options.
        count: u8,
    },
}

/// Everything the per-control temporary display needs for one parameter.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Readout {
    /// The parameter's full name — `STRIP.CUT`, not `CC 77`.
    pub name: &'static str,
    /// Its value, in terms of what the value means.
    pub reading: Reading,
    /// For the machine selector only: the name of the machine the current
    /// option selects, which is more use than "7/15".
    pub machine_label: Option<&'static str>,
}

/// A 7-bit RGB colour, as the controller's colour SysEx carries it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Rgb {
    /// Red, `0..=127`.
    pub r: u8,
    /// Green, `0..=127`.
    pub g: u8,
    /// Blue, `0..=127`.
    pub b: u8,
}

impl Rgb {
    /// An unlit LED.
    pub const DARK: Rgb = Rgb { r: 0, g: 0, b: 0 };

    const fn new(r: u8, g: u8, b: u8) -> Rgb {
        Rgb { r, g, b }
    }

    /// The same hue at `num/den` of the brightness — how an inactive
    /// control shows what it *would* be without competing with the active
    /// one.
    pub const fn scaled(self, num: u8, den: u8) -> Rgb {
        Rgb {
            r: (self.r * num) / den,
            g: (self.g * num) / den,
            b: (self.b * num) / den,
        }
    }
}

/// One colour per [`ParamGroup::ADDRESSABLE`] entry, in that order.
///
/// The surface's colour is what tells you which state you are in before you
/// read anything, so these need to be distinguishable at a glance rather
/// than pretty: the eight are spread around the hue circle and no two
/// adjacent groups share one. Components are 7-bit because the colour SysEx
/// is.
pub const GROUP_COLOURS: [Rgb; 8] = [
    Rgb::new(127, 127, 127), // TRACK  — white
    Rgb::new(127, 40, 0),    // VOICE  — orange
    Rgb::new(0, 90, 127),    // FILTER — cyan-blue
    Rgb::new(127, 0, 90),    // SHAPER — magenta
    Rgb::new(0, 127, 40),    // AMP    — green
    Rgb::new(90, 0, 127),    // LFO    — violet
    Rgb::new(127, 110, 0),   // ENV    — amber
    Rgb::new(0, 127, 110),   // MATRIX — teal
];

/// Brightness of a control that is live but not the active one.
const DIM: (u8, u8) = (1, 5);

/// Colour of the selected track's button.
const TRACK_SELECTED: Rgb = Rgb::new(127, 127, 127);

// ---------------------------------------------------------------------------
// Inputs and outcomes
// ---------------------------------------------------------------------------

/// A button the surface responds to.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Button {
    /// Upper button row: select (and audition) the track in this column.
    Track(u8),
    /// Lower button row: a parameter group, as an
    /// [`ParamGroup::ADDRESSABLE`] index.
    Group(u8),
    /// Page ▲.
    PageUp,
    /// Page ▼.
    PageDown,
}

/// One thing that happened on the surface.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Input {
    /// An encoder turned by `delta` detents.
    Encoder {
        /// 0 (top) to 2 (bottom).
        row: u8,
        /// 0 (left) to 7.
        column: u8,
        /// Signed detents.
        delta: i8,
    },
    /// A fader moved to a 7-bit absolute position.
    Fader {
        /// 0 (left) to 7.
        column: u8,
        /// `0..=127`.
        value: u8,
    },
    /// A button went down or came up.
    Button {
        /// Which button.
        button: Button,
        /// `true` on press.
        down: bool,
    },
    /// Shift went down or came up.
    Shift(bool),
    /// An encoder was touched or released.
    ///
    /// Used for one thing: an encoder that is mid-turn keeps editing the
    /// parameter the turn started on, even if a modal changes what that
    /// encoder would otherwise mean (#13, open question 3). Without touch
    /// events the surface still works — it just resolves every detent
    /// afresh.
    EncoderTouch {
        /// 0 (top) to 2 (bottom).
        row: u8,
        /// 0 (left) to 7.
        column: u8,
        /// `true` on touch.
        touched: bool,
    },
}

/// What an input produced.
pub struct Outcome {
    events: [Option<MidiEvent>; MAX_EVENTS],
    /// True when the surface's appearance changed and needs repainting.
    pub repaint: bool,
    /// The parameter an encoder turn landed on, for the temporary display.
    pub touched: Option<Addr>,
}

impl Outcome {
    const EMPTY: Outcome = Outcome {
        events: [None; MAX_EVENTS],
        repaint: false,
        touched: None,
    };

    fn push(&mut self, ev: MidiEvent) {
        for slot in self.events.iter_mut() {
            if slot.is_none() {
                *slot = Some(ev);
                return;
            }
        }
        debug_assert!(false, "MAX_EVENTS too small for one surface input");
    }

    /// The engine events this input produced, in order.
    pub fn events(&self) -> impl Iterator<Item = MidiEvent> + '_ {
        self.events.iter().flatten().copied()
    }
}

// ---------------------------------------------------------------------------
// The surface
// ---------------------------------------------------------------------------

/// The UI state machine.
///
/// `N` is the device's parameter count per track. There is no track-count
/// parameter: the surface is eight columns wide because the hardware is,
/// and a device with fewer tracks than that leaves the extra columns dark.
pub struct Surface<const N: usize> {
    catalogue: Catalogue<N>,
    /// Tracks the device actually has, at most [`COLUMNS`].
    tracks: usize,
    screen: Screen,
    selected: u8,
    page: u8,
    shift: bool,
    held: Option<Held>,
    /// Per encoder, the address a turn in progress is anchored to.
    anchor: [Option<Addr>; ENCODERS],
    /// Catalogue index of the machine loaded on each track.
    machine: [usize; COLUMNS],
    /// The driver's copy of every parameter's target value.
    values: [[f32; N]; COLUMNS],
}

impl<const N: usize> Surface<N> {
    /// Build a surface over a device's current state.
    ///
    /// `tracks` is one `(machine index, parameter values)` pair per track,
    /// read off the engine before it moves to the audio thread. The values
    /// have to be read rather than assumed: a device's starting kit is not
    /// obliged to be the machines' factory defaults — `DrumEngine::new`
    /// tweaks two parameters on its way out — and a shadow seeded from
    /// defaults would disagree with the engine from the first block.
    ///
    /// More than [`COLUMNS`] tracks is not an error: the surface cannot
    /// reach past its eighth column, and saying so here is better than
    /// refusing to start.
    pub fn new(catalogue: Catalogue<N>, tracks_in: &[(usize, [f32; N])]) -> Self {
        let tracks = tracks_in.len().min(COLUMNS);
        let mut machine = [0usize; COLUMNS];
        let mut values = [[0.0f32; N]; COLUMNS];
        for (t, (id, params)) in tracks_in.iter().enumerate().take(tracks) {
            machine[t] = *id;
            values[t] = *params;
        }
        Surface {
            catalogue,
            tracks,
            screen: Screen::Macros,
            selected: 0,
            page: 0,
            shift: false,
            held: None,
            anchor: [None; ENCODERS],
            machine,
            values,
        }
    }

    /// Tracks the device has.
    // The read side of the state machine. Several of these are reached
    // only from tests today and from the firmware in phase 3; they are the
    // API that makes the module testable without hardware, which is the
    // whole reason it is separate from the driver.
    #[allow(dead_code)]
    pub fn tracks(&self) -> usize {
        self.tracks
    }

    /// The track the modal screens act on.
    #[allow(dead_code)]
    pub fn selected_track(&self) -> u8 {
        self.selected
    }

    /// The current screen.
    #[allow(dead_code)]
    pub fn screen(&self) -> Screen {
        self.screen
    }

    /// Current page within the screen.
    #[allow(dead_code)]
    pub fn page(&self) -> u8 {
        self.page
    }

    /// The driver's shadow of one parameter's value.
    #[allow(dead_code)]
    pub fn value(&self, track: usize, flat: usize) -> f32 {
        self.values[track][flat]
    }

    /// Catalogue index of the machine on `track`.
    #[allow(dead_code)]
    pub fn machine(&self, track: usize) -> usize {
        self.machine[track]
    }

    /// The parameter table of the machine on `track`.
    pub fn table(&self, track: usize) -> &'static [ParamInfo] {
        (self.catalogue.table)(self.machine[track])
    }

    // -- input ------------------------------------------------------------

    /// Apply one surface input.
    ///
    /// `now_ms` is a monotonic millisecond clock; it is only read for
    /// hold-versus-tap, so its epoch does not matter.
    pub fn handle(&mut self, input: Input, now_ms: u32) -> Outcome {
        match input {
            Input::Encoder { row, column, delta } => self.on_encoder(row, column, delta),
            Input::Fader { column, value } => self.on_fader(column, value),
            Input::Button { button, down } => self.on_button(button, down, now_ms),
            Input::Shift(down) => {
                self.shift = down;
                Outcome::EMPTY
            }
            Input::EncoderTouch {
                row,
                column,
                touched,
            } => {
                let i = encoder_slot(row, column);
                if touched {
                    // Anchor the turn to whatever this encoder means right
                    // now, so a modal opening mid-turn does not move the
                    // target under the finger.
                    self.anchor[i] = self.encoder_addr(row, column);
                } else {
                    self.anchor[i] = None;
                }
                Outcome::EMPTY
            }
        }
    }

    fn on_encoder(&mut self, row: u8, column: u8, delta: i8) -> Outcome {
        let mut out = Outcome::EMPTY;
        if row as usize >= ROWS || column as usize >= COLUMNS {
            return out;
        }
        let slot = encoder_slot(row, column);
        // A turn already in progress keeps its target; otherwise resolve
        // what this encoder means now.
        let Some(addr) = self.anchor[slot].or_else(|| self.encoder_addr(row, column)) else {
            return out;
        };
        let step = delta as i16;
        self.apply_step(addr, step);
        out.push(MidiEvent::NrpnStep {
            channel: addr.track,
            group: addr.id.group as u8,
            index: addr.id.index,
            step,
        });
        out.touched = Some(addr);
        // A machine change rewrites the screen's contents, and so does any
        // move of a value the screen is showing.
        out.repaint = true;
        out
    }

    fn on_fader(&mut self, column: u8, value: u8) -> Outcome {
        let mut out = Outcome::EMPTY;
        let Some(addr) = self.fader_addr(column) else {
            return out;
        };
        // 7 bits widened to 14 by replicating the MSB, so a fader at 127
        // reaches full scale exactly. 128 steps is the right resolution for
        // a track level and the reason faders drive levels and nothing that
        // needs tuning (#13, "the wire is 7-bit and that is fine").
        let raw = ((value as u16) << 7) | value as u16;
        self.apply_absolute(addr, raw);
        out.push(MidiEvent::Nrpn {
            channel: addr.track,
            group: addr.id.group as u8,
            index: addr.id.index,
            value: raw,
        });
        out.touched = Some(addr);
        out.repaint = true;
        out
    }

    fn on_button(&mut self, button: Button, down: bool, now_ms: u32) -> Outcome {
        let mut out = Outcome::EMPTY;
        match button {
            Button::Track(c) => {
                if !down || c as usize >= self.tracks {
                    return out;
                }
                self.selected = c;
                // Select-by-hit: picking a track sounds it, which is how
                // you pick a track on a drum machine. Shift selects
                // silently.
                if !self.shift {
                    out.push(MidiEvent::NoteOn {
                        channel: c,
                        note: AUDITION_NOTE,
                        velocity: AUDITION_VELOCITY,
                    });
                }
                // A different track can have a different machine, so the
                // group screen's page may no longer exist.
                self.clamp_page();
                out.repaint = true;
            }
            Button::Group(g) => {
                if g as usize >= ParamGroup::ADDRESSABLE.len() {
                    return out;
                }
                if down {
                    // A group with no rows on the loaded machine is not
                    // selectable: landing there would strand the screen on
                    // an address that does not exist.
                    if !self.group_is_reachable(g) {
                        return out;
                    }
                    self.held = Some(Held {
                        group: g,
                        at_ms: now_ms,
                        restore: self.screen,
                    });
                    // The modal opens on *press*, not after HOLD_MS. A
                    // threshold here would give every hold a window in
                    // which the encoders still meant the old thing.
                    self.screen = Screen::Group(g);
                    self.page = 0;
                    out.repaint = true;
                } else {
                    let Some(held) = self.held.take() else {
                        return out;
                    };
                    if held.group != g {
                        // Release of a button that is not the anchor; the
                        // anchor is gone either way, so put it back.
                        self.held = Some(held);
                        return out;
                    }
                    if now_ms.saturating_sub(held.at_ms) >= HOLD_MS {
                        // A hold: the modal collapses and the screen the
                        // finger came from comes back.
                        self.screen = held.restore;
                        self.clamp_page();
                        out.repaint = true;
                    }
                    // A tap latches: the screen set on press simply stays.
                }
            }
            Button::PageUp | Button::PageDown => {
                if !down {
                    return out;
                }
                let pages = self.pages();
                let next = if matches!(button, Button::PageUp) {
                    self.page.saturating_sub(1)
                } else {
                    (self.page + 1).min(pages.saturating_sub(1))
                };
                if next != self.page {
                    self.page = next;
                    out.repaint = true;
                }
            }
        }
        out
    }

    // -- addressing --------------------------------------------------------

    /// What encoder `(row, column)` edits on the current screen, or `None`
    /// if that encoder is dark.
    pub fn encoder_addr(&self, row: u8, column: u8) -> Option<Addr> {
        if row as usize >= ROWS || column as usize >= COLUMNS {
            return None;
        }
        let (track, group, index) = match self.screen {
            // Columns are tracks: encoder (row, c) is macro `row` of track
            // `c`. Falling back to the TRACK group until #14, the three
            // rows are three consecutive TRACK parameters and Page moves
            // the window.
            Screen::Macros => {
                if column as usize >= self.tracks {
                    return None;
                }
                let index = self.page as usize * ROWS + row as usize;
                (column, ParamGroup::Track, index)
            }
            // A group screen is 24 parameters of the selected track, in
            // reading order, with Page for the rest.
            Screen::Group(g) => {
                let group = ParamGroup::ADDRESSABLE[g as usize];
                let index =
                    self.page as usize * ENCODERS + row as usize * COLUMNS + column as usize;
                (self.selected, group, index)
            }
        };
        if index > u8::MAX as usize || index >= group_len(self.table(track as usize), group) {
            return None;
        }
        Some(Addr {
            track,
            id: ParamId {
                group,
                index: index as u8,
            },
        })
    }

    /// What the fader under `column` edits: that track's level, always.
    ///
    /// Derived from the table rather than named by flat index — the level
    /// is the row whose route is [`ParamRoute::Level`], and its `(group,
    /// index)` address is read back out of the table the same way
    /// `resolve` would find it.
    pub fn fader_addr(&self, column: u8) -> Option<Addr> {
        if column as usize >= self.tracks {
            return None;
        }
        let table = self.table(column as usize);
        let flat = table.iter().position(|i| i.route == ParamRoute::Level)?;
        Some(Addr {
            track: column,
            id: address_of(table, flat)?,
        })
    }

    /// Whether group `g` (an [`ParamGroup::ADDRESSABLE`] index) has any
    /// parameters on the selected track's machine.
    pub fn group_is_reachable(&self, g: u8) -> bool {
        let Some(&group) = ParamGroup::ADDRESSABLE.get(g as usize) else {
            return false;
        };
        group_len(self.table(self.selected as usize), group) > 0
    }

    /// Pages the current screen has, at least 1.
    pub fn pages(&self) -> u8 {
        let (len, per_page) = match self.screen {
            Screen::Macros => (
                group_len(self.table(self.selected as usize), ParamGroup::Track),
                ROWS,
            ),
            Screen::Group(g) => (
                group_len(
                    self.table(self.selected as usize),
                    ParamGroup::ADDRESSABLE[g as usize],
                ),
                ENCODERS,
            ),
        };
        (len.div_ceil(per_page).max(1)).min(u8::MAX as usize) as u8
    }

    fn clamp_page(&mut self) {
        let last = self.pages().saturating_sub(1);
        if self.page > last {
            self.page = last;
        }
        // A group that vanished under a machine change cannot stay up.
        if let Screen::Group(g) = self.screen {
            if !self.group_is_reachable(g) {
                self.screen = Screen::Macros;
                self.page = 0;
            }
        }
    }

    // -- the shadow --------------------------------------------------------

    /// Flat index of an address on `track`, or `None` if it does not
    /// resolve there.
    fn flat(&self, addr: Addr) -> Option<usize> {
        drum_engine::machines::resolve(self.table(addr.track as usize), addr.id)
    }

    /// Mirror an absolute NRPN write into the shadow.
    fn apply_absolute(&mut self, addr: Addr, raw: u16) {
        let Some(flat) = self.flat(addr) else { return };
        let info = self.table(addr.track as usize)[flat];
        let v = nrpn_value(&info, raw);
        self.store(addr.track as usize, flat, info, v);
    }

    /// Mirror an NRPN increment/decrement into the shadow.
    ///
    /// The arithmetic is `apply_nrpn_step`'s, deliberately: a discrete row
    /// reads only the sign and moves one option, everything else takes the
    /// step in 14-bit LSBs off the parameter's *target*.
    fn apply_step(&mut self, addr: Addr, step: i16) {
        let Some(flat) = self.flat(addr) else { return };
        let info = self.table(addr.track as usize)[flat];
        let current = self.values[addr.track as usize][flat];
        let v = if let Some(count) = info.option_count() {
            let steps = (count - 1) as i32;
            let k = (current * steps as f32 + 0.5) as i32 + (step.signum() as i32);
            k.clamp(0, steps) as f32 / steps as f32
        } else {
            let raw = (current * NRPN_MAX as f32 + 0.5) as i32 + step as i32;
            raw.clamp(0, NRPN_MAX as i32) as f32 / NRPN_MAX as f32
        };
        self.store(addr.track as usize, flat, info, v);
    }

    /// Write one value into the shadow.
    ///
    /// The shadow models `Track::param_target` — the value a relative step
    /// is taken from — rather than the live smoothed parameter, because
    /// that is what the arithmetic above reads.
    ///
    /// Only the machine selector needs more than a store. The output
    /// selector looks as if it might, because `Track::set_param` quantises
    /// it onto an `OutPair` before storing, but the value arriving here is
    /// already on that grid: both paths into the shadow snap a
    /// [`ParamKind::Discrete`] row onto `k / (count - 1)` first, and the
    /// engine's own quantiser is a no-op on a value already there.
    /// `out_round_trips_through_the_shadow` holds that.
    fn store(&mut self, track: usize, flat: usize, info: ParamInfo, v: f32) {
        if info.route == ParamRoute::Machine {
            self.load_machine(track, v);
            return;
        }
        self.values[track][flat] = v;
    }

    /// Mirror `Track::load_machine` into the shadow.
    ///
    /// `MACH` is a parameter, `(TRACK, 0)`, so the driver sees a machine
    /// change go past and reloads from it — the one piece of state that
    /// would otherwise move underneath the shadow (#13, "Feedback needs no
    /// reverse channel").
    ///
    /// What the engine does on a swap is not simply "load the defaults":
    /// pan and the sends are track-level and survive, and `MACH` itself is
    /// rewritten to mirror the machine that was actually loaded rather
    /// than keeping the value that was written. Both are reproduced here
    /// by *route*, not by flat index, so this stays device-agnostic.
    fn load_machine(&mut self, track: usize, written: f32) {
        let count = self.catalogue.count;
        if count <= 1 {
            return;
        }
        // The engine truncates here rather than rounding
        // (`Track::set_param`, the `Machine` route), and a shadow that
        // rounded would drift by one machine at some catalogue sizes.
        let i = (written * (count - 1) as f32) as usize;
        if i >= count || i == self.machine[track] {
            return;
        }
        let previous = self.values[track];
        let old_table = self.table(track);
        let mut next = loaded_params(&self.catalogue, i);
        // Pan and the sends are track-level and survive the swap. Matched
        // by route rather than by flat index, so this stays device-
        // agnostic.
        for (flat, row) in old_table.iter().enumerate() {
            if matches!(row.route, ParamRoute::Pan | ParamRoute::Send(_)) {
                next[flat] = previous[flat];
            }
        }
        self.machine[track] = i;
        self.values[track] = next;
        // The new machine has different parameters, so no turn in progress
        // is still aimed at anything meaningful.
        self.anchor = [None; ENCODERS];
        self.clamp_page();
    }

    // -- rendering ---------------------------------------------------------

    /// The current screen's title.
    pub fn title(&self) -> Title {
        let pages = self.pages();
        match self.screen {
            Screen::Macros => Title::Macros {
                page: self.page,
                pages,
            },
            Screen::Group(g) => Title::Group {
                group: ParamGroup::ADDRESSABLE[g as usize],
                page: self.page,
                pages,
            },
        }
    }

    /// The short name under each encoder, in reading order, or `None` where
    /// the encoder is dark.
    pub fn encoder_names(&self) -> [Option<&'static str>; ENCODERS] {
        let mut out = [None; ENCODERS];
        for row in 0..ROWS as u8 {
            for column in 0..COLUMNS as u8 {
                let Some(addr) = self.encoder_addr(row, column) else {
                    continue;
                };
                let Some(flat) = self.flat(addr) else {
                    continue;
                };
                out[encoder_slot(row, column)] = Some(self.table(addr.track as usize)[flat].abbrev);
            }
        }
        out
    }

    /// Encoder LED colours, in reading order.
    ///
    /// An encoder takes the colour of the group it currently represents, so
    /// the surface's colour says which state you are in before you read
    /// anything. An encoder with no reachable address is dark.
    pub fn encoder_leds(&self) -> [Rgb; ENCODERS] {
        let mut out = [Rgb::DARK; ENCODERS];
        for row in 0..ROWS as u8 {
            for column in 0..COLUMNS as u8 {
                let Some(addr) = self.encoder_addr(row, column) else {
                    continue;
                };
                out[encoder_slot(row, column)] = group_colour(addr.id.group);
            }
        }
        out
    }

    /// Track-button LED colours. The selected track is bright; a column
    /// with no track behind it is dark.
    pub fn track_leds(&self) -> [Rgb; COLUMNS] {
        let mut out = [Rgb::DARK; COLUMNS];
        for (c, led) in out.iter_mut().enumerate() {
            if c >= self.tracks {
                continue;
            }
            *led = if c as u8 == self.selected {
                TRACK_SELECTED
            } else {
                TRACK_SELECTED.scaled(DIM.0, DIM.1)
            };
        }
        out
    }

    /// Group-button LED colours. A group the loaded machine has no
    /// parameters in is dark and does nothing; the group currently on
    /// screen is at full brightness.
    pub fn group_leds(&self) -> [Rgb; COLUMNS] {
        let mut out = [Rgb::DARK; COLUMNS];
        for (g, led) in out.iter_mut().enumerate() {
            if !self.group_is_reachable(g as u8) {
                continue;
            }
            let colour = GROUP_COLOURS[g];
            *led = if self.screen == Screen::Group(g as u8) {
                colour
            } else {
                colour.scaled(DIM.0, DIM.1)
            };
        }
        out
    }

    /// Name and value of one address, for the per-control temporary
    /// display — so a turn reads `STRIP.CUT 62%` rather than `CC 77 79`.
    pub fn readout(&self, addr: Addr) -> Option<Readout> {
        let flat = self.flat(addr)?;
        let info = self.table(addr.track as usize)[flat];
        let v = self.values[addr.track as usize][flat];
        let (reading, machine_label) = match info.kind {
            ParamKind::Discrete { count } if count > 1 => {
                let option = ((v * (count - 1) as f32) + 0.5) as u8;
                let option = option.min(count - 1);
                let label = if info.route == ParamRoute::Machine {
                    Some((self.catalogue.label)(option as usize))
                } else {
                    None
                };
                (Reading::Discrete { option, count }, label)
            }
            ParamKind::Bipolar => (Reading::Bipolar(v * 2.0 - 1.0), None),
            _ => (Reading::Continuous(v), None),
        };
        Some(Readout {
            name: info.name,
            reading,
            machine_label,
        })
    }
}

// ---------------------------------------------------------------------------
// Free functions
// ---------------------------------------------------------------------------

/// Reading-order slot of an encoder.
pub const fn encoder_slot(row: u8, column: u8) -> usize {
    row as usize * COLUMNS + column as usize
}

/// The colour a group paints its controls.
pub fn group_colour(group: ParamGroup) -> Rgb {
    ParamGroup::ADDRESSABLE
        .iter()
        .position(|&g| g == group)
        .map(|i| GROUP_COLOURS[i])
        .unwrap_or(Rgb::DARK)
}

/// How many rows a table places in `group`.
fn group_len(table: &[ParamInfo], group: ParamGroup) -> usize {
    drum_engine::machines::group_len(table, group)
}

/// The `(group, index)` address of a flat slot — `resolve` run backwards.
fn address_of(table: &[ParamInfo], flat: usize) -> Option<ParamId> {
    let group = table.get(flat)?.group;
    if group == ParamGroup::Reserved {
        return None;
    }
    let index = table[..flat].iter().filter(|i| i.group == group).count();
    Some(ParamId {
        group,
        index: index as u8,
    })
}

/// A track's parameter values just after machine `i` is loaded onto it.
///
/// The machine's factory defaults, plus the one correction `load_machine`
/// makes to them: `MACH` mirrors the machine that was loaded rather than
/// keeping the table's own default. Without that, a kit whose tracks are
/// not all machine 0 starts with a shadow that disagrees with the engine
/// about which machine is loaded — which the next relative step on the
/// selector would turn into a jump.
fn loaded_params<const N: usize>(catalogue: &Catalogue<N>, i: usize) -> [f32; N] {
    let mut values = (catalogue.defaults)(i);
    let divisor = catalogue.count.saturating_sub(1).max(1) as f32;
    for (flat, row) in (catalogue.table)(i).iter().enumerate() {
        if row.route == ParamRoute::Machine {
            values[flat] = i as f32 / divisor;
        }
    }
    values
}

/// Normalise a raw 14-bit value for one parameter — `midi::nrpn_value`,
/// which is private to the engine's router.
fn nrpn_value(info: &ParamInfo, raw: u16) -> f32 {
    match info.kind {
        ParamKind::Discrete { count } if count > 1 => {
            let steps = (count - 1) as u32;
            let k = (raw as u32 * steps + NRPN_MAX as u32 / 2) / NRPN_MAX as u32;
            k.min(steps) as f32 / steps as f32
        }
        _ => raw.min(NRPN_MAX) as f32 / NRPN_MAX as f32,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use drum_engine::machines::{MachineId, NUM_PARAMS, SLOT_LEVEL, SLOT_MACHINE};
    use drum_engine::midi::handle_midi;
    use drum_engine::{DeviceEngine, DeviceModel, DrumEngine, SlotId};

    const PARAMS: usize = NUM_PARAMS;

    /// A surface over the drum device, with the kit a fresh `DrumEngine`
    /// loads — so a test can drive both and compare.
    fn surface() -> (Surface<PARAMS>, DrumEngine) {
        let engine = DrumEngine::new();
        let kit = crate::lcxl3::snapshot::<_, PARAMS>(&engine);
        let s = Surface::new(crate::lcxl3::catalogue_for::<MachineId, PARAMS>(), &kit);
        (s, engine)
    }

    fn press(s: &mut Surface<PARAMS>, button: Button, at: u32) -> Outcome {
        s.handle(Input::Button { button, down: true }, at)
    }

    fn release(s: &mut Surface<PARAMS>, button: Button, at: u32) -> Outcome {
        s.handle(
            Input::Button {
                button,
                down: false,
            },
            at,
        )
    }

    /// Index of a group in `ADDRESSABLE`, which is also its button column.
    fn gcol(group: ParamGroup) -> u8 {
        ParamGroup::ADDRESSABLE
            .iter()
            .position(|&g| g == group)
            .unwrap() as u8
    }

    // -- the resting screen -------------------------------------------------

    /// The hardware groups itself into vertical strips and the UI must not
    /// fight it: on the resting screen, column `c` is track `c`.
    #[test]
    fn on_the_resting_screen_columns_are_tracks() {
        let (s, _) = surface();
        assert_eq!(s.screen(), Screen::Macros);
        for column in 0..COLUMNS as u8 {
            for row in 0..ROWS as u8 {
                let addr = s.encoder_addr(row, column).expect("TRACK row exists");
                assert_eq!(addr.track, column, "column {column} must be track {column}");
                assert_eq!(addr.id.group, ParamGroup::Track);
                assert_eq!(addr.id.index, row);
            }
        }
    }

    /// The fader under a column is that column's track level, found by
    /// route rather than by a hard-coded flat index.
    #[test]
    fn faders_are_track_levels() {
        let (s, _) = surface();
        for column in 0..COLUMNS as u8 {
            let addr = s.fader_addr(column).expect("every machine has a level");
            assert_eq!(addr.track, column);
            let flat = drum_engine::machines::resolve(s.table(column as usize), addr.id);
            assert_eq!(flat, Some(SLOT_LEVEL));
        }
    }

    /// A fader stays on the level even when a group screen is up — the
    /// faders are the one thing that does not change meaning.
    #[test]
    fn faders_do_not_follow_the_screen() {
        let (mut s, _) = surface();
        let before = s.fader_addr(3);
        press(&mut s, Button::Group(gcol(ParamGroup::Voice)), 0);
        assert_eq!(s.fader_addr(3), before);
    }

    // -- group screens ------------------------------------------------------

    /// A group screen is 24 parameters of the *selected* track in reading
    /// order — not of the column's track.
    #[test]
    fn a_group_screen_is_the_selected_tracks_parameters_in_reading_order() {
        let (mut s, _) = surface();
        press(&mut s, Button::Track(5), 0);
        let g = gcol(ParamGroup::Voice);
        press(&mut s, Button::Group(g), 10);
        let mut seen = Vec::new();
        for row in 0..ROWS as u8 {
            for column in 0..COLUMNS as u8 {
                if let Some(addr) = s.encoder_addr(row, column) {
                    assert_eq!(addr.track, 5, "a group screen follows the selection");
                    assert_eq!(addr.id.group, ParamGroup::Voice);
                    seen.push(addr.id.index);
                }
            }
        }
        let expected: Vec<u8> = (0..seen.len() as u8).collect();
        assert_eq!(seen, expected, "reading order, no holes");
    }

    /// Past the end of a group the encoders are dark rather than wrapped
    /// or clamped onto the last parameter.
    #[test]
    fn encoders_past_the_end_of_a_group_are_dark() {
        let (mut s, _) = surface();
        let g = gcol(ParamGroup::Voice);
        press(&mut s, Button::Group(g), 0);
        let len = drum_engine::machines::group_len(s.table(0), ParamGroup::Voice);
        assert!(
            len < ENCODERS,
            "this test needs a group smaller than a page"
        );
        let leds = s.encoder_leds();
        let names = s.encoder_names();
        for slot in len..ENCODERS {
            let (row, column) = ((slot / COLUMNS) as u8, (slot % COLUMNS) as u8);
            assert_eq!(s.encoder_addr(row, column), None);
            assert_eq!(leds[slot], Rgb::DARK);
            assert_eq!(names[slot], None);
        }
    }

    /// "A group with nothing in it is not selectable." A drum machine has
    /// no MATRIX rows, so that button is dark and does nothing.
    #[test]
    fn an_empty_group_is_dark_and_not_selectable() {
        let (mut s, _) = surface();
        let g = gcol(ParamGroup::Matrix);
        assert!(!s.group_is_reachable(g));
        assert_eq!(s.group_leds()[g as usize], Rgb::DARK);
        let out = press(&mut s, Button::Group(g), 0);
        assert_eq!(s.screen(), Screen::Macros, "the press must do nothing");
        assert!(!out.repaint);
    }

    /// Every group the machine *does* have is selectable and lit.
    #[test]
    fn groups_the_machine_has_are_lit() {
        let (s, _) = surface();
        for (g, &group) in ParamGroup::ADDRESSABLE.iter().enumerate() {
            let len = drum_engine::machines::group_len(s.table(0), group);
            assert_eq!(
                s.group_is_reachable(g as u8),
                len > 0,
                "{} has {len} rows",
                group.name()
            );
            assert_eq!(s.group_leds()[g] == Rgb::DARK, len == 0);
        }
    }

    // -- hold versus tap ----------------------------------------------------

    /// Hold: the modal opens on press and collapses on release, putting
    /// back the screen the finger came from.
    #[test]
    fn holding_a_group_button_is_a_modal() {
        let (mut s, _) = surface();
        let g = gcol(ParamGroup::Filter);
        press(&mut s, Button::Group(g), 1_000);
        assert_eq!(s.screen(), Screen::Group(g), "the modal opens on press");
        let out = release(&mut s, Button::Group(g), 1_000 + HOLD_MS);
        assert_eq!(s.screen(), Screen::Macros, "release restores the screen");
        assert!(out.repaint);
    }

    /// Tap: the same press, released sooner, latches instead.
    #[test]
    fn tapping_a_group_button_latches_it() {
        let (mut s, _) = surface();
        let g = gcol(ParamGroup::Filter);
        press(&mut s, Button::Group(g), 1_000);
        release(&mut s, Button::Group(g), 1_000 + HOLD_MS - 1);
        assert_eq!(s.screen(), Screen::Group(g), "a tap latches");
    }

    /// A hold started from a latched group returns to that group, not to
    /// the resting screen: the record is of the screen at press.
    #[test]
    fn a_hold_restores_whatever_was_latched_before_it() {
        let (mut s, _) = surface();
        let filter = gcol(ParamGroup::Filter);
        let amp = gcol(ParamGroup::Amp);
        press(&mut s, Button::Group(filter), 0);
        release(&mut s, Button::Group(filter), 10); // tap: latch FILTER
        press(&mut s, Button::Group(amp), 100);
        release(&mut s, Button::Group(amp), 100 + HOLD_MS); // hold: collapse
        assert_eq!(s.screen(), Screen::Group(filter));
    }

    /// The modal must not wait for the hold threshold before changing what
    /// the encoders do, or every hold would start with a window in which
    /// they still meant the old thing.
    #[test]
    fn the_modal_takes_effect_immediately_on_press() {
        let (mut s, _) = surface();
        let g = gcol(ParamGroup::Filter);
        press(&mut s, Button::Group(g), 0);
        let addr = s.encoder_addr(0, 0).unwrap();
        assert_eq!(addr.id.group, ParamGroup::Filter);
    }

    // -- track select -------------------------------------------------------

    /// Select-by-hit: picking a track sounds it.
    #[test]
    fn track_select_auditions_the_track() {
        let (mut s, _) = surface();
        let out = press(&mut s, Button::Track(4), 0);
        assert_eq!(s.selected_track(), 4);
        let events: Vec<_> = out.events().collect();
        assert_eq!(
            events,
            vec![MidiEvent::NoteOn {
                channel: 4,
                note: AUDITION_NOTE,
                velocity: AUDITION_VELOCITY,
            }]
        );
        assert!(out.repaint);
    }

    /// Shift selects without sounding.
    #[test]
    fn shift_selects_without_sounding() {
        let (mut s, _) = surface();
        s.handle(Input::Shift(true), 0);
        let out = press(&mut s, Button::Track(2), 0);
        assert_eq!(s.selected_track(), 2);
        assert_eq!(out.events().count(), 0);
    }

    /// The selected track's button is the bright one.
    #[test]
    fn the_selected_track_is_the_bright_button() {
        let (mut s, _) = surface();
        press(&mut s, Button::Track(6), 0);
        let leds = s.track_leds();
        assert_eq!(leds[6], TRACK_SELECTED);
        for (c, led) in leds.iter().enumerate() {
            if c != 6 {
                assert_ne!(*led, TRACK_SELECTED, "only one track is selected");
                assert_ne!(*led, Rgb::DARK, "the others are dim, not off");
            }
        }
    }

    // -- paging -------------------------------------------------------------

    #[test]
    fn page_moves_the_window_and_clamps_at_both_ends() {
        let (mut s, _) = surface();
        // The resting screen shows three TRACK rows at a time.
        let len = drum_engine::machines::group_len(s.table(0), ParamGroup::Track);
        let pages = len.div_ceil(ROWS) as u8;
        assert_eq!(s.pages(), pages);
        assert!(
            pages > 1,
            "this test needs TRACK to span more than one page"
        );

        press(&mut s, Button::PageUp, 0);
        assert_eq!(s.page(), 0, "page up at the top does nothing");

        press(&mut s, Button::PageDown, 1);
        assert_eq!(s.page(), 1);
        assert_eq!(s.encoder_addr(0, 0).unwrap().id.index, ROWS as u8);

        for i in 0..10 {
            press(&mut s, Button::PageDown, 10 + i);
        }
        assert_eq!(s.page(), pages - 1, "page down clamps at the last page");
    }

    /// A single-page group reports one page, not zero — the title would
    /// otherwise read "1/0".
    #[test]
    fn a_group_always_has_at_least_one_page() {
        let (mut s, _) = surface();
        press(&mut s, Button::Group(gcol(ParamGroup::Amp)), 0);
        assert!(s.pages() >= 1);
    }

    // -- machine changes ----------------------------------------------------

    /// `MACH` is a parameter, so the surface sees a machine change go past
    /// and reloads the new machine's table and defaults. This is the one
    /// case where state would otherwise move underneath the shadow.
    #[test]
    fn a_machine_change_reloads_the_table_and_the_defaults() {
        let (mut s, _) = surface();
        let before = s.machine(0);
        // MACH is (TRACK, 0): the first encoder of column 0 on the resting
        // screen.
        let out = s.handle(
            Input::Encoder {
                row: 0,
                column: 0,
                delta: 1,
            },
            0,
        );
        assert_eq!(s.machine(0), before + 1, "one detent is one machine");
        assert!(out.repaint);
        let expected = MachineId::from_index(before + 1).unwrap();
        assert_eq!(s.table(0).as_ptr(), expected.param_table().as_ptr());
        assert_eq!(
            s.value(0, SLOT_LEVEL),
            expected.default_params()[SLOT_LEVEL]
        );
    }

    /// Switching to a machine without the group on screen cannot strand
    /// the screen on an address that no longer exists.
    #[test]
    fn a_group_that_vanishes_drops_the_screen() {
        let (mut s, _) = surface();
        // Latch a group, then force the selected track onto a machine that
        // has no rows in it. Walk the catalogue to find such a pair rather
        // than hard-coding one, so adding a machine cannot silently make
        // this test vacuous.
        let Some((from, to, group)) = ParamGroup::ADDRESSABLE.iter().find_map(|&group| {
            let from = (0..MachineId::count())
                .find(|&i| group_len(MachineId::from_index(i).unwrap().param_table(), group) > 0)?;
            let to = (0..MachineId::count()).find(|&i| {
                group_len(MachineId::from_index(i).unwrap().param_table(), group) == 0
            })?;
            Some((from, to, group))
        }) else {
            // Every machine has every group: nothing to test, and the
            // reachability rule is unreachable rather than wrong.
            return;
        };

        set_machine(&mut s, 0, from);
        press(&mut s, Button::Track(0), 0);
        press(&mut s, Button::Group(gcol(group)), 0);
        release(&mut s, Button::Group(gcol(group)), 1); // tap: latch
        assert_eq!(s.screen(), Screen::Group(gcol(group)));

        set_machine(&mut s, 0, to);
        assert_eq!(
            s.screen(),
            Screen::Macros,
            "{} vanished from the machine; the screen cannot stay on it",
            group.name()
        );
    }

    /// Drive the machine selector to a catalogue index by absolute write.
    fn set_machine(s: &mut Surface<PARAMS>, track: u8, index: usize) {
        let count = MachineId::count();
        let v = index as f32 / (count - 1) as f32;
        let raw = (v * NRPN_MAX as f32 + 0.5) as u16;
        s.apply_absolute(
            Addr {
                track,
                id: ParamId {
                    group: ParamGroup::Track,
                    index: 0,
                },
            },
            raw,
        );
        assert_eq!(s.machine(track as usize), index, "machine write landed");
    }

    // -- turn in progress ---------------------------------------------------

    /// #13, open question 3: if a group button goes down while an encoder
    /// is moving, the next detent would otherwise mean something different.
    /// A turn in progress keeps its target until the encoder stops.
    #[test]
    fn a_turn_in_progress_keeps_its_target_across_a_modal() {
        let (mut s, _) = surface();
        press(&mut s, Button::Track(1), 0);
        // Row 1 of column 1: reachable on the resting screen and on the
        // FILTER screen both, so the test is about the anchor rather than
        // about an encoder going dark.
        let anchored = s.encoder_addr(0, 1).expect("resting screen row 1");
        s.handle(
            Input::EncoderTouch {
                row: 0,
                column: 1,
                touched: true,
            },
            0,
        );
        // A modal opens under the finger.
        press(&mut s, Button::Group(gcol(ParamGroup::Filter)), 10);
        let out = s.handle(
            Input::Encoder {
                row: 0,
                column: 1,
                delta: 1,
            },
            20,
        );
        let ev = out.events().next().unwrap();
        let MidiEvent::NrpnStep { group, index, .. } = ev else {
            panic!("expected a step, got {ev:?}");
        };
        assert_eq!(
            (group, index),
            (anchored.id.group as u8, anchored.id.index),
            "the turn kept the address it started on"
        );

        // Once the finger lifts, the encoder means what the screen says.
        s.handle(
            Input::EncoderTouch {
                row: 0,
                column: 1,
                touched: false,
            },
            30,
        );
        let out = s.handle(
            Input::Encoder {
                row: 0,
                column: 1,
                delta: 1,
            },
            40,
        );
        let MidiEvent::NrpnStep { group, .. } = out.events().next().unwrap() else {
            panic!("expected a step");
        };
        assert_eq!(group, ParamGroup::Filter as u8);
    }

    /// Without touch events the surface still works; it just resolves every
    /// detent afresh.
    #[test]
    fn without_touch_events_a_turn_follows_the_screen() {
        let (mut s, _) = surface();
        press(&mut s, Button::Group(gcol(ParamGroup::Filter)), 0);
        let out = s.handle(
            Input::Encoder {
                row: 0,
                column: 0,
                delta: 1,
            },
            10,
        );
        let MidiEvent::NrpnStep { group, .. } = out.events().next().unwrap() else {
            panic!("expected a step");
        };
        assert_eq!(group, ParamGroup::Filter as u8);
    }

    // -- events on the wire -------------------------------------------------

    /// A turn becomes an NRPN increment, a fader an NRPN data entry. The
    /// surface emits engine-facing events and nothing XL 3-shaped.
    #[test]
    fn encoders_emit_steps_and_faders_emit_values() {
        let (mut s, _) = surface();
        let out = s.handle(
            Input::Encoder {
                row: 1,
                column: 2,
                delta: -3,
            },
            0,
        );
        let addr = Addr {
            track: 2,
            id: ParamId {
                group: ParamGroup::Track,
                index: 1,
            },
        };
        assert_eq!(
            out.events().collect::<Vec<_>>(),
            vec![MidiEvent::NrpnStep {
                channel: addr.track,
                group: ParamGroup::Track as u8,
                index: 1,
                step: -3,
            }]
        );

        let out = s.handle(
            Input::Fader {
                column: 3,
                value: 127,
            },
            0,
        );
        let MidiEvent::Nrpn { channel, value, .. } = out.events().next().unwrap() else {
            panic!("a fader is an absolute write");
        };
        assert_eq!(channel, 3);
        assert_eq!(value, NRPN_MAX, "127 must reach full scale exactly");
    }

    /// An encoder with no address emits nothing at all, rather than a step
    /// at an address the engine will drop.
    #[test]
    fn a_dark_encoder_emits_nothing() {
        let (mut s, _) = surface();
        press(&mut s, Button::Group(gcol(ParamGroup::Voice)), 0);
        let len = drum_engine::machines::group_len(s.table(0), ParamGroup::Voice);
        let slot = len; // first dark encoder
        let out = s.handle(
            Input::Encoder {
                row: (slot / COLUMNS) as u8,
                column: (slot % COLUMNS) as u8,
                delta: 1,
            },
            0,
        );
        assert_eq!(out.events().count(), 0);
    }

    // -- colour -------------------------------------------------------------

    /// "Encoder LEDs take the colour of the group they currently
    /// represent" — so the surface's colour says which state you are in
    /// before you read anything.
    #[test]
    fn encoders_take_the_colour_of_their_group() {
        let (mut s, _) = surface();
        for (g, &group) in ParamGroup::ADDRESSABLE.iter().enumerate() {
            if !s.group_is_reachable(g as u8) {
                continue;
            }
            press(&mut s, Button::Group(g as u8), 0);
            release(&mut s, Button::Group(g as u8), 1);
            assert_eq!(
                s.encoder_leds()[0],
                GROUP_COLOURS[g],
                "{} must paint its own colour",
                group.name()
            );
            assert_eq!(s.group_leds()[g], GROUP_COLOURS[g], "and its button");
        }
    }

    /// Group colours have to be told apart at a glance, which means no two
    /// may be equal.
    #[test]
    fn group_colours_are_distinct() {
        for (i, a) in GROUP_COLOURS.iter().enumerate() {
            for b in &GROUP_COLOURS[i + 1..] {
                assert_ne!(a, b);
            }
            assert_ne!(*a, Rgb::DARK, "dark means unreachable, not a group");
        }
    }

    /// Every component must survive a 7-bit SysEx data byte.
    #[test]
    fn group_colours_are_seven_bit() {
        for c in GROUP_COLOURS {
            assert!(c.r <= 127 && c.g <= 127 && c.b <= 127, "{c:?}");
        }
    }

    // -- repaint ------------------------------------------------------------

    /// "Every transition repaints." A state change that does not ask for
    /// one leaves the surface lying about its mode.
    #[test]
    fn every_transition_asks_for_a_repaint() {
        let (mut s, _) = surface();
        assert!(press(&mut s, Button::Track(3), 0).repaint, "track select");
        let g = gcol(ParamGroup::Filter);
        assert!(press(&mut s, Button::Group(g), 10).repaint, "modal open");
        assert!(
            release(&mut s, Button::Group(g), 10 + HOLD_MS).repaint,
            "modal collapse"
        );
        assert!(press(&mut s, Button::PageDown, 500).repaint, "page");
        assert!(
            s.handle(
                Input::Encoder {
                    row: 0,
                    column: 0,
                    delta: 1
                },
                600
            )
            .repaint,
            "a value the screen shows moved"
        );
    }

    // -- readout ------------------------------------------------------------

    /// The per-control display reads the parameter's name and a value in
    /// its own terms — `STRIP.CUT 62%`, not `CC 77 79`.
    #[test]
    fn the_readout_names_the_parameter_and_its_value() {
        let (s, _) = surface();
        let addr = s.fader_addr(0).unwrap();
        let r = s.readout(addr).unwrap();
        assert_eq!(r.name, s.table(0)[SLOT_LEVEL].name);
        assert!(matches!(r.reading, Reading::Continuous(_)));
    }

    /// The machine selector reads as the machine's name, which is more use
    /// than "7 of 15".
    #[test]
    fn the_machine_selector_reads_as_a_machine_name() {
        let (s, _) = surface();
        let addr = Addr {
            track: 0,
            id: ParamId {
                group: ParamGroup::Track,
                index: 0,
            },
        };
        let r = s.readout(addr).unwrap();
        assert_eq!(r.name, s.table(0)[SLOT_MACHINE].name);
        assert_eq!(
            r.machine_label,
            Some(MachineId::from_index(s.machine(0)).unwrap().label())
        );
    }

    /// A bipolar parameter reads outward from its centre.
    #[test]
    fn a_bipolar_parameter_reads_signed() {
        let (s, _) = surface();
        let table = s.table(0);
        let flat = table
            .iter()
            .position(|i| i.kind == ParamKind::Bipolar)
            .expect("the drum device has a bipolar row (PAN)");
        let id = address_of(table, flat).unwrap();
        let r = s.readout(Addr { track: 0, id }).unwrap();
        // Default pan is centre.
        assert!(
            matches!(r.reading, Reading::Bipolar(v) if v.abs() < 1e-6),
            "{:?}",
            r.reading
        );
    }

    // -- the shadow ---------------------------------------------------------

    /// The driver renders from its own copy of the parameter values rather
    /// than from a reverse channel out of the audio thread (#13). That is
    /// only sound if the copy agrees with the engine, so: drive both
    /// through the same sequence of surface inputs and compare every
    /// parameter of every track.
    #[test]
    fn shadow_tracks_the_engine() {
        let (mut s, mut engine) = surface();

        let script: Vec<Input> = vec![
            Input::Fader {
                column: 0,
                value: 100,
            },
            Input::Fader {
                column: 7,
                value: 3,
            },
            Input::Button {
                button: Button::Track(2),
                down: true,
            },
            Input::Button {
                button: Button::Group(gcol(ParamGroup::Filter)),
                down: true,
            },
            Input::Encoder {
                row: 0,
                column: 0,
                delta: 40,
            },
            Input::Encoder {
                row: 0,
                column: 1,
                delta: -7,
            },
            Input::Encoder {
                row: 1,
                column: 0,
                delta: 1,
            },
            Input::Button {
                button: Button::Group(gcol(ParamGroup::Filter)),
                down: false,
            },
            // Back to the resting screen and onto the machine selector of
            // track 0 — the one write that reloads a whole track.
            Input::Button {
                button: Button::Group(gcol(ParamGroup::Filter)),
                down: true,
            },
            Input::Button {
                button: Button::Group(gcol(ParamGroup::Filter)),
                down: false,
            },
            Input::Encoder {
                row: 0,
                column: 0,
                delta: 3,
            },
            Input::Encoder {
                row: 2,
                column: 4,
                delta: -2,
            },
            Input::Fader {
                column: 4,
                value: 64,
            },
        ];

        let mut now = 0u32;
        for input in script {
            // A hold, so the group button sequences above collapse rather
            // than latch where the script expects it.
            now += HOLD_MS;
            let out = s.handle(input, now);
            for ev in out.events() {
                handle_midi(&mut engine, ev);
            }
        }

        for track in 0..engine.tracks().len() {
            assert_eq!(
                s.machine(track),
                engine.tracks()[track].id().index(),
                "track {track}: machine"
            );
            for flat in 0..PARAMS {
                let shadow = s.value(track, flat);
                let actual = engine.tracks()[track].param_target(flat);
                assert!(
                    (shadow - actual).abs() < 1e-6,
                    "track {track} param {flat}: shadow {shadow} vs engine {actual}"
                );
            }
        }
    }

    /// One detent on a discrete parameter moves one option, not one LSB —
    /// the engine reads only the sign, and the shadow must too.
    #[test]
    fn a_detent_on_a_discrete_parameter_moves_one_option() {
        let (mut s, mut engine) = surface();
        let before = engine.tracks()[0].id().index();
        for _ in 0..3 {
            let out = s.handle(
                Input::Encoder {
                    row: 0,
                    column: 0,
                    delta: 1,
                },
                0,
            );
            for ev in out.events() {
                handle_midi(&mut engine, ev);
            }
        }
        assert_eq!(engine.tracks()[0].id().index(), before + 3);
        assert_eq!(s.machine(0), before + 3);
    }

    /// The output selector is quantised by the engine on its way in, so
    /// the shadow has to land on the same grid or a second detent would
    /// step from somewhere the engine is not.
    #[test]
    fn out_round_trips_through_the_shadow() {
        let (mut s, mut engine) = surface();
        let table = s.table(0);
        let Some(flat) = table.iter().position(|i| i.route == ParamRoute::Out) else {
            return; // this device has no output selector
        };
        let id = address_of(table, flat).unwrap();
        let addr = Addr { track: 0, id };

        for _ in 0..5 {
            s.apply_step(addr, 1);
            handle_midi(
                &mut engine,
                MidiEvent::NrpnStep {
                    channel: 0,
                    group: id.group as u8,
                    index: id.index,
                    step: 1,
                },
            );
            assert!(
                (s.value(0, flat) - engine.tracks()[0].param_target(flat)).abs() < 1e-6,
                "shadow {} vs engine {}",
                s.value(0, flat),
                engine.tracks()[0].param_target(flat)
            );
        }
        assert_ne!(s.value(0, flat), 0.0, "the selector actually moved");
    }

    /// A step is handed to the engine unmodified: acceleration is the
    /// controller's job, and the engine keeps no per-parameter timing
    /// state (`DESIGN.md`, "The control surface is NRPN").
    #[test]
    fn the_step_magnitude_passes_straight_through() {
        let (mut s, _) = surface();
        press(&mut s, Button::Group(gcol(ParamGroup::Filter)), 0);
        for delta in [1i8, 6, -11, 64, -64] {
            let out = s.handle(
                Input::Encoder {
                    row: 0,
                    column: 0,
                    delta,
                },
                10,
            );
            let MidiEvent::NrpnStep { step, .. } = out.events().next().unwrap() else {
                panic!("expected a step");
            };
            assert_eq!(step, delta as i16);
        }
    }
}
