//! Monome Grid / midigrid control surface for drumsynth.
//!
//! This module is `no_std`/`no_alloc`. It receives MIDI Note On/Off events on
//! channel 16 (the `midigrid` control channel), translates pad presses into
//! engine parameter edits and triggers, and emits LED feedback as channel-16
//! Note On velocities.
//!
//! The UI has two primary screens plus temporary fine-tune overlays:
//!
//! * **Mixer** — 8 bottom-oriented vertical faders for track levels, with
//!   track-select and audition pads on the right half.
//! * **Parameter** — 6 rows of horizontal widgets for the selected track,
//!   with 4 parameter pages.
//! * **Fine-tune** — holding a fader pad for `HOLD_MS` replaces the relevant
//!   row/column with a high-resolution strip.

use crate::machines::{
    MacroInfo, MachineId, NUM_MACROS, SLOT_LFO1_DEPTH, SLOT_LFO1_DEST, SLOT_LFO1_RATE,
    SLOT_LFO2_DEPTH, SLOT_LFO2_DEST, SLOT_LFO2_RATE, SLOT_LEVEL, SLOT_MACHINE, SLOT_OUT,
    SLOT_PAN, SLOT_SEND_DELAY, SLOT_SEND_REVERB, SLOT_STRIP_ATK, SLOT_STRIP_CUT, SLOT_STRIP_DEC,
    SLOT_STRIP_HOLD, SLOT_STRIP_RESO,
};
use crate::{DrumEngine, Track};

/// MIDI channel for `midigrid` (0-indexed; labelled Ch16 on the wire).
pub const MIDIGRID_CHANNEL: u8 = 15;

/// Grid width in pads.
pub const WIDTH: usize = 16;
/// Grid height in pads.
pub const HEIGHT: usize = 8;
/// Total number of pads on the grid.
pub const NUM_PADS: usize = WIDTH * HEIGHT;

/// A pad press must last this long before it becomes a fine-tune overlay.
const HOLD_MS: u32 = 300;

/// Default fixed velocity for audition / trigger pads (0..=127).
const DEFAULT_VELOCITY: u8 = 100;

/// MIDI note number for a chromatic, un-transposed trigger.
const TRIGGER_NOTE: u8 = 60;

// ---------------------------------------------------------------------------
// Public types
// ---------------------------------------------------------------------------

/// A parsed input event from the grid.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum GridEvent {
    /// Finger came down on a pad.
    PadDown {
        /// Column, 0..=15.
        x: u8,
        /// Row, 0..=7.
        y: u8,
    },
    /// Finger lifted from a pad.
    PadUp {
        /// Column, 0..=15.
        x: u8,
        /// Row, 0..=7.
        y: u8,
    },
}

/// Incremental parser for `midigrid` Note On/Off events on Ch16.
///
/// Handles running status and Note-On-with-zero-velocity-as-release, which
/// the drum engine's main MIDI parser deliberately drops.
#[derive(Default)]
pub struct GridParser {
    status: u8,
    data: [u8; 2],
    index: usize,
}

impl GridParser {
    /// New parser, listening to Ch16.
    pub const fn new() -> Self {
        Self {
            status: 0,
            data: [0; 2],
            index: 0,
        }
    }

    /// Push one raw MIDI byte. Returns a grid event when a complete message
    /// has arrived.
    pub fn push(&mut self, byte: u8) -> Option<GridEvent> {
        if byte >= 0xF8 {
            // System real-time — ignore, do not disturb running status.
            return None;
        }

        if byte >= 0x80 {
            if byte >= 0xF0 {
                // System common cancels running status.
                self.status = 0;
                self.index = 0;
                return None;
            }
            self.status = byte;
            self.index = 0;
            return None;
        }

        if self.status == 0 {
            return None;
        }

        self.data[self.index] = byte;
        self.index += 1;

        let expected = match self.status & 0xF0 {
            0xC0 | 0xD0 => 1,
            _ => 2,
        };

        if self.index < expected {
            return None;
        }
        self.index = 0;

        // Only channel 16.
        if (self.status & 0x0F) != MIDIGRID_CHANNEL {
            return None;
        }

        self.decode()
    }

    fn decode(&self) -> Option<GridEvent> {
        let note = self.data[0];
        let x = note % 16;
        let y = note / 16;
        if y >= HEIGHT as u8 {
            return None;
        }

        match self.status & 0xF0 {
            0x90 => {
                let velocity = self.data[1];
                if velocity == 0 {
                    Some(GridEvent::PadUp { x, y })
                } else {
                    Some(GridEvent::PadDown { x, y })
                }
            }
            0x80 => Some(GridEvent::PadUp { x, y }),
            _ => None,
        }
    }
}

/// A widget primitive for a 16-pad parameter row.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum WidgetKind {
    /// 0..127 horizontal bar, left-to-right.
    UnipolarFader,
    /// -64..+63 centre-zero fader.
    BipolarPan,
    /// `count` discrete options laid out as equal clusters.
    EnumSelector {
        /// Number of discrete options, 1..=16.
        count: u8,
    },
    /// 16 independent on/off flags.
    BitmaskToggle,
    /// 8-pad bottom-oriented vertical fader (Screen 1 mixer only).
    VerticalFader,
}

/// What a widget edits.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum ParamTarget {
    /// One of the 32 normalised macros, indexed by slot.
    Macro(usize),
    /// The active track's choke mask (`StripParams::choke_mask`).
    ChokeMask,
}

/// Descriptor for one parameter row.
#[derive(Clone, Copy, Debug)]
pub struct Widget {
    /// How to draw and interact with this row.
    pub kind: WidgetKind,
    /// Short label (3–4 chars) for the row.
    pub label: &'static str,
    /// Which engine parameter this row edits.
    pub target: ParamTarget,
}

/// The Monome grid control surface state machine.
pub struct Grid {
    screen: Screen,
    /// Active track for the parameter screen, 0..=7.
    selected_track: u8,
    /// Active parameter page, 0..=3.
    page: u8,
    /// Anchor pad for the current gesture, if any.
    held: Option<HeldPad>,
    /// Bitmask of all currently held pads.
    held_mask: u128,
    /// Fixed velocity used by audition / trigger pads.
    global_velocity: u8,
    /// Last LED values sent, used for dirty-tracking.
    leds: [u8; NUM_PADS],
    /// True when the LED buffer no longer matches the model. Set by input
    /// events and by hold-state transitions; cleared by a successful render.
    dirty: bool,
    /// Fine-tune state at the last render, so a held pad crossing the hold
    /// threshold triggers a redraw without re-rendering on every idle loop.
    in_fine_tune_last: bool,
}

#[derive(Clone, Copy, Debug, PartialEq)]
enum Screen {
    Mixer,
    Parameter,
}

#[derive(Clone, Copy, Debug)]
struct HeldPad {
    x: u8,
    y: u8,
    down_at_ms: u32,
    screen: Screen,
    track: u8,
    page: u8,
}

impl HeldPad {
    /// True if this pad has been held long enough to invoke fine-tune.
    fn is_hold(&self, now_ms: u32) -> bool {
        now_ms.saturating_sub(self.down_at_ms) >= HOLD_MS
    }
}

// ---------------------------------------------------------------------------
// Public API
// ---------------------------------------------------------------------------

impl Grid {
    /// Construct a fresh grid state machine.
    pub const fn new() -> Self {
        Self {
            screen: Screen::Mixer,
            selected_track: 0,
            page: 0,
            held: None,
            held_mask: 0,
            global_velocity: DEFAULT_VELOCITY,
            leds: [0u8; NUM_PADS],
            dirty: true,
            in_fine_tune_last: false,
        }
    }

    /// Process one grid input event, updating the engine as needed.
    ///
    /// LED feedback is **not** emitted here; call [`Self::render`] afterwards
    /// to push any changed LEDs through the supplied callback.
    pub fn process_event(&mut self, event: GridEvent, now_ms: u32, engine: &mut DrumEngine) {
        self.dirty = true;
        match event {
            GridEvent::PadDown { x, y } => self.on_pad_down(x, y, now_ms, engine),
            GridEvent::PadUp { x, y } => self.on_pad_up(x, y, now_ms, engine),
        }
    }

    /// Render the current grid state and emit LED updates for any pads that
    /// changed since the last render.
    ///
    /// `send` receives `(channel, note_number, velocity)` triples and returns
    /// `true` when the packet was accepted. If it returns `false`, the LED
    /// state is kept dirty so the next render pass retries.
    pub fn render<F>(&mut self, engine: &DrumEngine, now_ms: u32, mut send: F)
    where
        F: FnMut(u8, u8, u8) -> bool,
    {
        let in_fine_tune = match self.held {
            Some(h) => h.is_hold(now_ms),
            None => false,
        };

        // Skip the expensive LED recompute when nothing has changed and no
        // held pad has crossed the hold threshold since the last render.
        if !self.dirty && in_fine_tune == self.in_fine_tune_last {
            return;
        }
        self.in_fine_tune_last = in_fine_tune;
        self.dirty = false;

        let mut next = [0u8; NUM_PADS];

        if in_fine_tune {
            render_fine_tune(self, engine, &mut next);
        } else {
            match self.screen {
                Screen::Mixer => render_mixer(self, engine, &mut next),
                Screen::Parameter => render_parameter(self, engine, &mut next),
            }
        }

        let mut send_failed = false;
        for i in 0..NUM_PADS {
            if next[i] != self.leds[i] {
                let x = (i % WIDTH) as u8;
                let y = (i / WIDTH) as u8;
                if send(MIDIGRID_CHANNEL, y * 16 + x, next[i]) {
                    self.leds[i] = next[i];
                } else {
                    send_failed = true;
                }
            }
        }
        self.dirty = send_failed;
    }
}

// ---------------------------------------------------------------------------
// Event handling
// ---------------------------------------------------------------------------

impl Grid {
    fn on_pad_down(&mut self, x: u8, y: u8, now_ms: u32, engine: &mut DrumEngine) {
        let idx = pad_index(x, y);
        let was_empty = self.held_mask == 0;
        self.held_mask |= 1u128 << idx;

        if was_empty {
            // First finger down: this is the anchor for the gesture.
            self.held = Some(HeldPad {
                x,
                y,
                down_at_ms: now_ms,
                screen: self.screen,
                track: self.selected_track,
                page: self.page,
            });
            return;
        }

        // Additional finger while a gesture is already active: treat as a
        // fine-tune drag if we are past the hold threshold.
        if let Some(held) = self.held {
            if held.is_hold(now_ms) {
                self.apply_fine_tune_drag(x, y, engine);
            }
        }
    }

    fn on_pad_up(&mut self, x: u8, y: u8, now_ms: u32, engine: &mut DrumEngine) {
        let idx = pad_index(x, y);
        self.held_mask &= !(1u128 << idx);

        if self.held_mask != 0 {
            // Other fingers still down; keep the anchor alive.
            return;
        }

        let Some(held) = self.held.take() else {
            return;
        };

        if held.is_hold(now_ms) {
            // Held long enough to have entered fine-tune; release just
            // collapses the overlay. The value was already set during drag
            // or when the overlay first opened.
            return;
        }

        // Short tap: dispatch based on the screen that was active at press.
        match held.screen {
            Screen::Mixer => self.handle_mixer_tap(held.x, held.y, engine),
            Screen::Parameter => self.handle_parameter_tap(held.x, held.y, engine),
        }
    }

    fn handle_mixer_tap(&mut self, x: u8, y: u8, engine: &mut DrumEngine) {
        if x < 8 {
            // Vertical fader tap: jump LEVEL to the coarse value for this row.
            let track = x as usize;
            let value = vertical_fader_value(y);
            let vel = value as f32 / 127.0;
            engine.tracks[track].set_macro(SLOT_LEVEL, vel);
            return;
        }

        if y == 0 {
            // Track select (S1–S8).
            let track = x - 8;
            if track < crate::TRACKS as u8 {
                self.selected_track = track;
                self.screen = Screen::Parameter;
            }
            return;
        }

        if y == 7 {
            // Audition / trigger pads (A1–A8).
            let track = x - 8;
            if track < crate::TRACKS as u8 {
                let vel = self.global_velocity as f32 / 127.0;
                engine.trigger_channel(track, TRIGGER_NOTE, vel);
            }
        }
    }

    fn handle_parameter_tap(&mut self, x: u8, y: u8, engine: &mut DrumEngine) {
        if y == 0 {
            if x == 0 {
                // Back to mixer.
                self.screen = Screen::Mixer;
                return;
            }
            if x >= 12 && x <= 15 {
                // Page tab.
                self.page = x - 12;
                return;
            }
            return;
        }

        if y == 7 {
            // Audition strip.
            let vel = self.global_velocity as f32 / 127.0;
            engine.trigger_channel(self.selected_track, TRIGGER_NOTE, vel);
            return;
        }

        let row = y as usize - 1;
        let track = self.selected_track as usize;
        let machine = engine.tracks[track].id();
        let Some(widget) = page_widget(self.page, row, machine) else {
            return;
        };

        match widget.kind {
            WidgetKind::UnipolarFader | WidgetKind::BipolarPan => {
                // Quick tap jumps to the coarse base value for this column.
                let value = x * 8;
                write_target(&widget.target, &mut engine.tracks[track], value);
            }
        WidgetKind::EnumSelector { count } => {
            let option = enum_option_for_col(count, x);
            let value = if count <= 1 {
                0
            } else {
                (option as u16 * 127 / (count as u16 - 1)) as u8
            };
            write_target(&widget.target, &mut engine.tracks[track], value);
        }
            WidgetKind::BitmaskToggle => {
                let mask = read_target(&widget.target, &engine.tracks[track]);
                let new_mask = mask ^ (1 << x);
                write_target(&widget.target, &mut engine.tracks[track], new_mask);
            }
            WidgetKind::VerticalFader => {
                // Not used on the parameter screen.
            }
        }
    }

    fn apply_fine_tune_drag(&mut self, x: u8, y: u8, engine: &mut DrumEngine) {
        let Some(held) = self.held else { return };

        match held.screen {
            Screen::Mixer => {
                // Horizontal fine-tune strip in the held row.
                if y != held.y {
                    return;
                }
                let base = vertical_fader_value(held.y);
                let offset = x.min(15);
                let value = (base as u16 + offset as u16).min(127) as u8;
                let track = held.x as usize;
                engine.tracks[track].set_macro(SLOT_LEVEL, value as f32 / 127.0);
            }
            Screen::Parameter => {
                // Vertical fine-tune strip in the held column.
                if x != held.x {
                    return;
                }
                let row = held.y as usize - 1;
                let track_idx = held.track as usize;
                let machine = engine.tracks[track_idx].id();
                let Some(widget) = page_widget(held.page, row, machine) else {
                    return;
                };
                if !widget_supports_fine_tune(widget.kind) {
                    return;
                }
                let base = held.x * 8;
                // Top row (y=0) = +7, bottom row (y=7) = +0.
                let offset = 7u8.saturating_sub(y).min(7);
                let value = (base as u16 + offset as u16).min(127) as u8;
                write_target(&widget.target, &mut engine.tracks[track_idx], value);
            }
        }
    }
}

fn widget_supports_fine_tune(kind: WidgetKind) -> bool {
    matches!(kind, WidgetKind::UnipolarFader | WidgetKind::BipolarPan)
}

// ---------------------------------------------------------------------------
// Parameter binding
// ---------------------------------------------------------------------------

fn read_target(target: &ParamTarget, track: &Track) -> u8 {
    match target {
        ParamTarget::Macro(idx) => (track.base_macros[*idx].clamp(0.0, 1.0) * 127.0 + 0.5) as u8,
        ParamTarget::ChokeMask => track.strip.choke_mask,
    }
}

fn write_target(target: &ParamTarget, track: &mut Track, value: u8) {
    let v = value.min(127);
    match target {
        ParamTarget::Macro(idx) => {
            // Discrete selectors should snap instantly; continuous faders use
            // the same immediate set for responsive UI feedback.
            track.set_macro(*idx, v as f32 / 127.0);
        }
        ParamTarget::ChokeMask => {
            let mut strip = track.strip;
            strip.choke_mask = v;
            track.set_strip(&strip);
        }
    }
}

/// Build the widget descriptor for a given page/row and machine.
fn page_widget(page: u8, row: usize, machine: MachineId) -> Option<Widget> {
    match page {
        0 => mixer_page_widget(row),
        1 => strip_page_widget(row),
        2 => lfo_page_widget(row),
        3 => machine_page_widget(row, machine),
        _ => None,
    }
}

fn mixer_page_widget(row: usize) -> Option<Widget> {
    Some(match row {
        0 => Widget {
            kind: WidgetKind::UnipolarFader,
            label: "LVL",
            target: ParamTarget::Macro(SLOT_LEVEL),
        },
        1 => Widget {
            kind: WidgetKind::BipolarPan,
            label: "PAN",
            target: ParamTarget::Macro(SLOT_PAN),
        },
        2 => Widget {
            kind: WidgetKind::UnipolarFader,
            label: "DLY",
            target: ParamTarget::Macro(SLOT_SEND_DELAY),
        },
        3 => Widget {
            kind: WidgetKind::UnipolarFader,
            label: "RVB",
            target: ParamTarget::Macro(SLOT_SEND_REVERB),
        },
        4 => Widget {
            kind: WidgetKind::EnumSelector { count: 4 },
            label: "OUT",
            target: ParamTarget::Macro(SLOT_OUT),
        },
        5 => Widget {
            kind: WidgetKind::BitmaskToggle,
            label: "CHK",
            target: ParamTarget::ChokeMask,
        },
        _ => return None,
    })
}

fn strip_page_widget(row: usize) -> Option<Widget> {
    Some(match row {
        0 => Widget {
            kind: WidgetKind::UnipolarFader,
            label: "CUT",
            target: ParamTarget::Macro(SLOT_STRIP_CUT),
        },
        1 => Widget {
            kind: WidgetKind::UnipolarFader,
            label: "RES",
            target: ParamTarget::Macro(SLOT_STRIP_RESO),
        },
        2 => Widget {
            kind: WidgetKind::UnipolarFader,
            label: "ATK",
            target: ParamTarget::Macro(SLOT_STRIP_ATK),
        },
        3 => Widget {
            kind: WidgetKind::UnipolarFader,
            label: "HLD",
            target: ParamTarget::Macro(SLOT_STRIP_HOLD),
        },
        4 => Widget {
            kind: WidgetKind::UnipolarFader,
            label: "DEC",
            target: ParamTarget::Macro(SLOT_STRIP_DEC),
        },
        _ => return None,
    })
}

fn lfo_page_widget(row: usize) -> Option<Widget> {
    Some(match row {
        0 => Widget {
            kind: WidgetKind::UnipolarFader,
            label: "L1R",
            target: ParamTarget::Macro(SLOT_LFO1_RATE),
        },
        1 => Widget {
            kind: WidgetKind::UnipolarFader,
            label: "L1D",
            target: ParamTarget::Macro(SLOT_LFO1_DEPTH),
        },
        2 => Widget {
            kind: WidgetKind::EnumSelector { count: 16 },
            label: "L1S",
            target: ParamTarget::Macro(SLOT_LFO1_DEST),
        },
        3 => Widget {
            kind: WidgetKind::UnipolarFader,
            label: "L2R",
            target: ParamTarget::Macro(SLOT_LFO2_RATE),
        },
        4 => Widget {
            kind: WidgetKind::UnipolarFader,
            label: "L2D",
            target: ParamTarget::Macro(SLOT_LFO2_DEPTH),
        },
        5 => Widget {
            kind: WidgetKind::EnumSelector { count: 16 },
            label: "L2S",
            target: ParamTarget::Macro(SLOT_LFO2_DEST),
        },
        _ => return None,
    })
}

fn machine_page_widget(row: usize, machine: MachineId) -> Option<Widget> {
    let info: &[MacroInfo; NUM_MACROS] = &machine.macros();

    if row == 0 {
        return Some(Widget {
            kind: WidgetKind::EnumSelector {
                count: MachineId::COUNT as u8,
            },
            label: "MCH",
            target: ParamTarget::Macro(SLOT_MACHINE),
        });
    }

    // Fill rows 1..=5 with the first non-RESV machine-internal macros in
    // slots 0..=9 (MACH bank + FILT 0/1, excluding track-routed strip slots).
    let mut found = 0;
    for slot in 0..=9 {
        if info[slot].name == "RESV" {
            continue;
        }
        found += 1;
        if found == row {
            return Some(Widget {
                kind: WidgetKind::UnipolarFader,
                label: info[slot].abbrev,
                target: ParamTarget::Macro(slot),
            });
        }
    }

    None
}

// ---------------------------------------------------------------------------
// Rendering
// ---------------------------------------------------------------------------

fn render_mixer(grid: &Grid, engine: &DrumEngine, next: &mut [u8; NUM_PADS]) {
    // Left half: 8 vertical level faders.
    for track in 0..crate::TRACKS {
        let value = read_target(&ParamTarget::Macro(SLOT_LEVEL), &engine.tracks[track]);
        render_vertical_fader(track, value, next);
    }

    // Row 0, cols 8–15: track select buttons.
    for x in 8..16 {
        let vel = if x - 8 == grid.selected_track && grid.screen == Screen::Parameter {
            127
        } else {
            16
        };
        set_pad(next, x, 0, vel);
    }

    // Row 7, cols 8–15: audition pads.
    for x in 8..16 {
        set_pad(next, x, 7, 16);
    }
}

fn render_parameter(grid: &Grid, engine: &DrumEngine, next: &mut [u8; NUM_PADS]) {
    // Back button.
    set_pad(next, 0, 0, 16);

    // Page tabs, cols 12–15.
    for x in 12..16 {
        let page = x - 12;
        let vel = if page == grid.page { 127 } else { 16 };
        set_pad(next, x, 0, vel);
    }

    // Parameter rows.
    let track_idx = grid.selected_track as usize;
    let machine = engine.tracks[track_idx].id();
    for row in 0..6 {
        if let Some(widget) = page_widget(grid.page, row, machine) {
            let value = read_target(&widget.target, &engine.tracks[track_idx]);
            render_widget_row(row + 1, &widget, value, next);
        }
    }

    // Audition strip, row 7.
    for x in 0..16 {
        set_pad(next, x, 7, 16);
    }
}

fn render_fine_tune(grid: &Grid, engine: &DrumEngine, next: &mut [u8; NUM_PADS]) {
    let Some(held) = grid.held else { return };

    match held.screen {
        Screen::Mixer => {
            // Vertical fader held: horizontal strip in the held row.
            let base = vertical_fader_value(held.y);
            let current = read_target(&ParamTarget::Macro(SLOT_LEVEL), &engine.tracks[held.x as usize]);
            let offset = (current as i16 - base as i16).clamp(0, 15) as u8;

            // Dim the fader columns to keep context.
            for track in 0..crate::TRACKS {
                let value = read_target(&ParamTarget::Macro(SLOT_LEVEL), &engine.tracks[track]);
                render_vertical_fader(track, value, next);
                for y in 0..HEIGHT as u8 {
                    if y != held.y {
                        dim_pad(next, track as u8, y);
                    }
                }
            }

            // Horizontal fine-tune strip in the held row.
            for x in 0..16 {
                let vel = if x <= offset { 127 } else { 0 };
                set_pad(next, x, held.y, vel);
            }
        }
        Screen::Parameter => {
            // Horizontal fader held: vertical strip in the held column.
            let row = held.y as usize - 1;
            let track_idx = held.track as usize;
            let machine = engine.tracks[track_idx].id();
            let Some(widget) = page_widget(held.page, row, machine) else {
                return;
            };
            let value = read_target(&widget.target, &engine.tracks[track_idx]);
            let base = held.x * 8;
            let offset = (value as i16 - base as i16).clamp(0, 7) as u8;

            // Dim the underlying parameter rows for context.
            for r in 0..6 {
                if let Some(w) = page_widget(held.page, r, machine) {
                    let v = read_target(&w.target, &engine.tracks[track_idx]);
                    render_widget_row(r + 1, &w, v, next);
                }
            }
            for x in 0..WIDTH as u8 {
                if x != held.x {
                    for y in 1..=6 {
                        dim_pad(next, x, y);
                    }
                }
            }

            // Vertical fine-tune strip in the held column.
            // Top row (y=0) = +7, bottom row (y=7) = +0.
            for y in 0..HEIGHT as u8 {
                let step = 7 - y;
                let vel = if step <= offset { 127 } else { 0 };
                set_pad(next, held.x, y, vel);
            }
        }
    }
}

fn render_widget_row(row: usize, widget: &Widget, value: u8, next: &mut [u8; NUM_PADS]) {
    match widget.kind {
        WidgetKind::UnipolarFader => render_unipolar(row, value, next),
        WidgetKind::BipolarPan => render_bipolar(row, value, next),
        WidgetKind::EnumSelector { count } => render_enum(row, count, value, next),
        WidgetKind::BitmaskToggle => render_bitmask(row, value as u16, next),
        WidgetKind::VerticalFader => {
            // Not used in parameter rows.
        }
    }
}

fn render_unipolar(row: usize, value: u8, next: &mut [u8; NUM_PADS]) {
    let active_col = (value / 8) as usize;
    let fine_offset = value % 8;
    let y = row as u8;
    for x in 0..WIDTH {
        let vel = if x < active_col {
            127
        } else if x == active_col {
            32 + fine_offset * 11
        } else {
            0
        };
        set_pad(next, x as u8, y, vel);
    }
}

fn render_bipolar(row: usize, value: u8, next: &mut [u8; NUM_PADS]) {
    // Centre at CC 64, between columns 7 and 8.
    let y = row as u8;
    let pan_offset = value as i16 - 64; // -64..=63

    if pan_offset == 0 {
        for x in 0..WIDTH {
            let vel = if x == 7 || x == 8 { 32 } else { 0 };
            set_pad(next, x as u8, y, vel);
        }
        return;
    }

    if pan_offset < 0 {
        // Fill leftward from column 7.
        let target_col = 7 + (pan_offset / 8); // pan_offset/8 is negative or zero
        let fine = (8 + (pan_offset % 8)) % 8;
        let edge_vel = 32 + (fine as u8 * 11);
        for x in 0..WIDTH {
            let col = x as i16;
            let vel = if col > target_col && col <= 7 {
                127
            } else if col == target_col {
                edge_vel
            } else {
                0
            };
            set_pad(next, x as u8, y, vel);
        }
    } else {
        // Fill rightward from column 8.
        let target_col = 8 + (pan_offset / 8);
        let fine = (pan_offset % 8) as u8;
        let edge_vel = 32 + (fine * 11);
        for x in 0..WIDTH {
            let col = x as i16;
            let vel = if col < target_col && col >= 8 {
                127
            } else if col == target_col {
                edge_vel
            } else {
                0
            };
            set_pad(next, x as u8, y, vel);
        }
    }
}

fn render_enum(row: usize, count: u8, value: u8, next: &mut [u8; NUM_PADS]) {
    let y = row as u8;
    let count = count.clamp(1, 16);
    let cluster_size = (WIDTH / count as usize).max(1);
    let active = if count <= 1 {
        0
    } else {
        ((value as u16 * (count as u16 - 1) + 63) / 127).min(count as u16 - 1) as usize
    };

    for x in 0..WIDTH {
        let cluster = x / cluster_size;
        let vel = if cluster == active {
            127
        } else if x % cluster_size == 0 || x % cluster_size == cluster_size - 1 {
            16
        } else {
            0
        };
        set_pad(next, x as u8, y, vel);
    }
}

fn render_bitmask(row: usize, mask: u16, next: &mut [u8; NUM_PADS]) {
    let y = row as u8;
    for x in 0..WIDTH {
        let vel = if (mask & (1 << x)) != 0 { 127 } else { 0 };
        set_pad(next, x as u8, y, vel);
    }
}

fn render_vertical_fader(col: usize, value: u8, next: &mut [u8; NUM_PADS]) {
    // Bottom-oriented: row 7 = 0, row 0 = 112.
    let active_row = 7 - (value / 16);
    let fine_offset = value % 16;
    let x = col as u8;

    for y in 0..HEIGHT as u8 {
        let vel = if y > active_row {
            // Below leading edge (closer to bottom): full.
            127
        } else if y == active_row {
            // Leading edge: varibright based on fine offset within 16-CC band.
            32 + fine_offset * 6
        } else {
            // Above leading edge: off.
            0
        };
        set_pad(next, x, y, vel);
    }
}

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

const fn pad_index(x: u8, y: u8) -> usize {
    y as usize * WIDTH + x as usize
}

fn set_pad(next: &mut [u8; NUM_PADS], x: u8, y: u8, vel: u8) {
    next[pad_index(x, y)] = vel;
}

fn dim_pad(next: &mut [u8; NUM_PADS], x: u8, y: u8) {
    let idx = pad_index(x, y);
    next[idx] = next[idx] / 8;
}

/// Coarse value for a vertical fader tap at row `y`.
/// Row 7 (bottom) = 0, row 0 (top) = 112.
fn vertical_fader_value(y: u8) -> u8 {
    7u8.saturating_sub(y).min(7) * 16
}

/// Which enum option column `x` falls into for `count` options.
fn enum_option_for_col(count: u8, x: u8) -> u8 {
    let count = count.clamp(1, 16);
    let cluster_size = (WIDTH / count as usize).max(1) as u8;
    (x / cluster_size).min(count - 1)
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use crate::DrumEngine;

    fn collect_events(grid: &mut Grid, engine: &mut DrumEngine, now: u32, out: &mut [(u8, u8, u8); 128]) -> usize {
        let mut n = 0;
        grid.render(engine, now, |ch, note, vel| {
            out[n] = (ch, note, vel);
            n += 1;
            true
        });
        n
    }

    #[test]
    fn parser_note_on_and_off() {
        let mut p = GridParser::new();
        // Note on Ch16, note 0 (x=0,y=0), velocity 100.
        assert_eq!(
            p.push(0x9F),
            None
        );
        assert_eq!(
            p.push(0x00),
            None
        );
        assert_eq!(
            p.push(0x64),
            Some(GridEvent::PadDown { x: 0, y: 0 })
        );

        // Note off for same pad.
        let mut p = GridParser::new();
        assert_eq!(p.push(0x8F), None);
        assert_eq!(p.push(0x00), None);
        assert_eq!(
            p.push(0x40),
            Some(GridEvent::PadUp { x: 0, y: 0 })
        );

        // Zero-velocity note-on is also a release.
        let mut p = GridParser::new();
        assert_eq!(p.push(0x9F), None);
        assert_eq!(p.push(0x10), None); // note 16 = x=0,y=1
        assert_eq!(
            p.push(0x00),
            Some(GridEvent::PadUp { x: 0, y: 1 })
        );
    }

    #[test]
    fn parser_ignores_other_channels() {
        let mut p = GridParser::new();
        assert_eq!(p.push(0x90), None); // Note on Ch1
        assert_eq!(p.push(0x00), None);
        assert_eq!(p.push(0x64), None);
    }

    #[test]
    fn mixer_select_jumps_to_parameter_screen() {
        let mut grid = Grid::new();
        let mut engine = DrumEngine::new();

        // Tap S3 (row 0, col 10).
        grid.process_event(GridEvent::PadDown { x: 10, y: 0 }, 0, &mut engine);
        grid.process_event(GridEvent::PadUp { x: 10, y: 0 }, 0, &mut engine);

        assert_eq!(grid.selected_track, 2);
        assert!(matches!(grid.screen, Screen::Parameter));
    }

    #[test]
    fn mixer_fader_sets_level() {
        let mut grid = Grid::new();
        let mut engine = DrumEngine::new();

        // Tap top of track 0 fader (row 0, col 0). Value = 7*16 = 112.
        grid.process_event(GridEvent::PadDown { x: 0, y: 0 }, 0, &mut engine);
        grid.process_event(GridEvent::PadUp { x: 0, y: 0 }, 0, &mut engine);

        let expected = 112.0 / 127.0;
        assert!(
            (engine.tracks[0].base_macros[SLOT_LEVEL] - expected).abs() < 1e-6,
            "level should jump to 112/127, got {}",
            engine.tracks[0].base_macros[SLOT_LEVEL]
        );
    }

    #[test]
    fn parameter_back_returns_to_mixer() {
        let mut grid = Grid::new();
        let mut engine = DrumEngine::new();

        grid.screen = Screen::Parameter;
        grid.selected_track = 3;

        grid.process_event(GridEvent::PadDown { x: 0, y: 0 }, 0, &mut engine);
        grid.process_event(GridEvent::PadUp { x: 0, y: 0 }, 0, &mut engine);

        assert!(matches!(grid.screen, Screen::Mixer));
    }

    #[test]
    fn parameter_enum_selector_sets_out() {
        let mut grid = Grid::new();
        let mut engine = DrumEngine::new();

        grid.screen = Screen::Parameter;
        grid.page = 0; // Mixer page, row 5 = OUT enum.
        grid.selected_track = 0;

        // Tap column 5 in row 5 (second cluster of 4, so Aux1).
        grid.process_event(GridEvent::PadDown { x: 5, y: 5 }, 0, &mut engine);
        grid.process_event(GridEvent::PadUp { x: 5, y: 5 }, 0, &mut engine);

        assert_eq!(engine.tracks[0].strip.out, crate::OutPair::Aux1);
    }

    #[test]
    fn bitmask_toggle_flips_choke_bit() {
        let mut grid = Grid::new();
        let mut engine = DrumEngine::new();

        grid.screen = Screen::Parameter;
        grid.page = 0; // Mixer page, row 6 = CHOKE bitmask.
        grid.selected_track = 0;

        // Toggle bit 3.
        grid.process_event(GridEvent::PadDown { x: 3, y: 6 }, 0, &mut engine);
        grid.process_event(GridEvent::PadUp { x: 3, y: 6 }, 0, &mut engine);

        assert_eq!(engine.tracks[0].strip.choke_mask & (1 << 3), 1 << 3);

        // Toggle again.
        grid.process_event(GridEvent::PadDown { x: 3, y: 6 }, 0, &mut engine);
        grid.process_event(GridEvent::PadUp { x: 3, y: 6 }, 0, &mut engine);

        assert_eq!(engine.tracks[0].strip.choke_mask & (1 << 3), 0);
    }

    #[test]
    fn horizontal_fine_tune_updates_value() {
        let mut grid = Grid::new();
        let mut engine = DrumEngine::new();

        grid.screen = Screen::Parameter;
        grid.page = 1; // Strip page, row 1 = STRIP.CUTOFF.
        grid.selected_track = 0;

        // Hold column 5 row 1 for long enough, then drag to row 3 in same column.
        grid.process_event(GridEvent::PadDown { x: 5, y: 1 }, 0, &mut engine);
        grid.process_event(GridEvent::PadDown { x: 5, y: 3 }, HOLD_MS + 1, &mut engine);
        grid.process_event(GridEvent::PadUp { x: 5, y: 3 }, HOLD_MS + 2, &mut engine);
        grid.process_event(GridEvent::PadUp { x: 5, y: 1 }, HOLD_MS + 3, &mut engine);

        // Base = 5*8 = 40. Row 3 offset = 7-3 = 4. Total = 44.
        let expected = 44.0 / 127.0;
        assert!(
            (engine.tracks[0].base_macros[SLOT_STRIP_CUT] - expected).abs() < 1e-6,
            "cutoff should be 44/127, got {}",
            engine.tracks[0].base_macros[SLOT_STRIP_CUT]
        );
    }

    #[test]
    fn vertical_fine_tune_updates_level() {
        let mut grid = Grid::new();
        let mut engine = DrumEngine::new();

        // Hold row 4 of track 0 fader, then drag to column 10 in same row.
        grid.process_event(GridEvent::PadDown { x: 0, y: 4 }, 0, &mut engine);
        grid.process_event(GridEvent::PadDown { x: 10, y: 4 }, HOLD_MS + 1, &mut engine);
        grid.process_event(GridEvent::PadUp { x: 10, y: 4 }, HOLD_MS + 2, &mut engine);
        grid.process_event(GridEvent::PadUp { x: 0, y: 4 }, HOLD_MS + 3, &mut engine);

        // Base = (7-4)*16 = 48. Offset = 10. Total = 58.
        let expected = 58.0 / 127.0;
        assert!(
            (engine.tracks[0].base_macros[SLOT_LEVEL] - expected).abs() < 1e-6,
            "level should be 58/127, got {}",
            engine.tracks[0].base_macros[SLOT_LEVEL]
        );
    }

    #[test]
    fn renderer_emits_channel_16_notes() {
        let mut grid = Grid::new();
        let mut engine = DrumEngine::new();

        let mut buf = [(0u8, 0u8, 0u8); 128];
        let n = collect_events(&mut grid, &mut engine, 0, &mut buf);

        // Mixer render should emit updates for the left faders and right buttons.
        assert!(n > 0);
        for i in 0..n {
            assert_eq!(buf[i].0, MIDIGRID_CHANNEL);
        }
    }

    #[test]
    fn enum_option_mapping_is_reversible() {
        // For every legal count and option, the value we write should map
        // back to the same option under the engine's quantization math.
        for count in [2u8, 3, 4, 5, 8, 15, 16] {
            for option in 0..count {
                let value = (option as u16 * 127 / (count as u16 - 1)) as u8;
                let displayed =
                    ((value as u16 * (count as u16 - 1) + 63) / 127).min(count as u16 - 1) as u8;
                assert_eq!(
                    displayed, option,
                    "count={count} option={option} value={value}"
                );
            }
        }
    }
}
