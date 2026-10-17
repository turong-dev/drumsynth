//! Monome Grid / midigrid control surface for synth devices.
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
//!   addressed by [`ParamGroup`] and page.
//! * **Fine-tune** — holding a fader pad for `HOLD_MS` replaces the relevant
//!   row/column with a high-resolution strip.
//!
//! The grid is generic over any [`DeviceEngine`](crate::engine::DeviceEngine);
//! the only device-specific surface it needs is [`DeviceModel`] on the slot
//! identifier, which vends per-machine param metadata.
//!
//! # The parameter screen is derived, not hard-coded
//!
//! Rows come from the device's parameter table. The screen is addressed the
//! same way NRPN is — `(group, index)` — so the group selector picks a
//! [`ParamGroup`] and the row is the `page * 6 + row`-th parameter in it.
//! Nothing here names a flat slot index except [`SLOT_LEVEL`], which the
//! mixer's vertical faders need before a group has been chosen.
//!
//! Each row draws itself from the parameter's [`ParamKind`]: a continuous
//! sweep is a fader, a bipolar one fills outward from the centre, and a
//! discrete one lays its options out as clusters. A device adds a parameter
//! by adding a table row, and it appears on the grid.
//!
//! ## Reaching 33 parameters with 24 pads
//!
//! Six rows across four page tabs is 24 reachable parameters, and mi-drum
//! has 33. The fix is the **group selector** on the chrome row, columns
//! 1–8: one pad per [`ParamGroup::ADDRESSABLE`] entry, lit when the loaded
//! machine has parameters in that group. Groups are small — the largest is
//! Plaits' nine VOICE rows — so the four page tabs are nearly always one
//! page, and the selector is what actually moves between parameters.
//!
//! A pad for a group the machine does not use is dark and does nothing, so
//! switching from Plaits to Peaks (nine VOICE rows down to four) cannot
//! strand the screen on an address that no longer exists.
//!
//! The track's choke mask is not a parameter — it lives on the strip, not
//! in the table — so it is appended as one synthetic row after the TRACK
//! group's own rows.

use crate::engine::DeviceEngine;
use crate::params::{group_len, resolve, ParamGroup, ParamId, ParamInfo, ParamKind, SLOT_LEVEL};
use crate::slot::{DeviceModel, Slot};
use crate::track::Track;

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

/// Full grid LED state is re-rendered at least this often, even when no input
/// events occur. This lets a grid that was powered on or reconnected after the
/// device boot sync back up without requiring a specific power-on order.
pub const REFRESH_MS: u32 = 1000;

/// Minimum time between LED renders. Events that arrive faster are coalesced
/// into the next render. This paces USB bulk-IN traffic and prevents a run of
/// failed sends from starving the audio interrupt.
const RENDER_INTERVAL_MS: u32 = 2;

/// Maximum number of LED packets emitted in a single render call. Pacing the
/// USB bulk-IN traffic this way avoids saturating a small endpoint FIFO or the
/// grid's serial bridge. Any remaining pads retry on the next render (at most
/// `RENDER_INTERVAL_MS` later).
const MAX_SENDS_PER_RENDER: usize = 32;

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
/// the device engine's main MIDI parser deliberately drops.
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
    /// One of the normalised params, indexed by slot.
    Param(usize),
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
///
/// The state itself is device-independent; methods are generic over the
/// connected [`DeviceEngine`].
pub struct Grid {
    screen: Screen,
    /// Active track for the parameter screen, 0..=7.
    selected_track: u8,
    /// Active parameter group — an index into
    /// [`ParamGroup::ADDRESSABLE`], which is also its selector column
    /// offset.
    group: u8,
    /// Active page within the group, 0..=3. Six parameters per page.
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
    /// Timestamp of the last full LED render, used for periodic refresh.
    last_render_ms: u32,
    /// True when the periodic refresh fired; forces a full LED state dump on
    /// the next render so a reconnecting grid resyncs.
    refresh_pending: bool,
    /// During a forced refresh, the next pad index to send. `NUM_PADS as u8`
    /// means the refresh is complete.
    refresh_pos: u8,
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
    group: u8,
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

impl Default for Grid {
    fn default() -> Self {
        Self::new()
    }
}

impl Grid {
    /// Construct a fresh grid state machine.
    pub const fn new() -> Self {
        Self {
            screen: Screen::Mixer,
            selected_track: 0,
            // TRACK: level, pan, sends, output, choke — the rows the mixer
            // page held before the screen was derived from the table, and
            // the one group every machine has.
            group: 0,
            page: 0,
            held: None,
            held_mask: 0,
            global_velocity: DEFAULT_VELOCITY,
            leds: [0u8; NUM_PADS],
            dirty: true,
            in_fine_tune_last: false,
            // `u32::MAX` means "never rendered yet"; the first render bypasses
            // the rate limiter so boot feedback is immediate.
            last_render_ms: u32::MAX,
            refresh_pending: false,
            refresh_pos: NUM_PADS as u8,
        }
    }

    /// Process one grid input event, updating the engine as needed.
    ///
    /// LED feedback is **not** emitted here; call [`Self::render`] afterwards
    /// to push any changed LEDs through the supplied callback.
    pub fn process_event<E, const N: usize>(
        &mut self,
        event: GridEvent,
        now_ms: u32,
        engine: &mut E,
    ) where
        E: DeviceEngine<N>,
        <E::Slot as Slot<N>>::Id: DeviceModel<N>,
    {
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
    pub fn render<E, const N: usize, F>(&mut self, engine: &E, now_ms: u32, mut send: F)
    where
        E: DeviceEngine<N>,
        <E::Slot as Slot<N>>::Id: DeviceModel<N>,
        F: FnMut(u8, u8, u8) -> bool,
    {
        let in_fine_tune = match self.held {
            Some(h) => h.is_hold(now_ms),
            None => false,
        };

        // Periodic refresh: a grid that was connected after boot (or missed
        // messages) should still converge to the current state.
        if now_ms.saturating_sub(self.last_render_ms) >= REFRESH_MS {
            self.dirty = true;
            self.refresh_pending = true;
            self.refresh_pos = 0;
        }

        // Skip the expensive LED recompute when nothing has changed and no
        // held pad has crossed the hold threshold since the last render.
        if !self.dirty && in_fine_tune == self.in_fine_tune_last {
            return;
        }

        // Rate-limit renders so a run of failed USB sends (or a flood of input
        // events) cannot starve the audio interrupt. The first render after
        // boot bypasses this because `last_render_ms` starts at `u32::MAX`.
        let since_last = now_ms.saturating_sub(self.last_render_ms);
        if since_last < RENDER_INTERVAL_MS && self.last_render_ms != u32::MAX {
            return;
        }

        self.in_fine_tune_last = in_fine_tune;
        self.dirty = false;
        self.last_render_ms = now_ms;
        let refresh_active = self.refresh_pos < NUM_PADS as u8;

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
        let mut budget_exceeded = false;
        let mut sent_this_render = 0usize;

        if refresh_active {
            // Forced refresh: walk the pads in order and emit them in small
            // batches. This avoids swamping the USB bulk-IN endpoint / the
            // grid's serial bridge with 128 messages at once.
            let start = self.refresh_pos as usize;
            let end = (start + MAX_SENDS_PER_RENDER).min(NUM_PADS);
            for (i, &value) in next.iter().enumerate().take(end).skip(start) {
                let x = (i % WIDTH) as u8;
                let y = (i / WIDTH) as u8;
                if send(MIDIGRID_CHANNEL, y * 16 + x, value) {
                    self.leds[i] = value;
                    self.refresh_pos += 1;
                } else {
                    send_failed = true;
                    break;
                }
            }
            if self.refresh_pos >= NUM_PADS as u8 {
                self.refresh_pending = false;
            } else {
                budget_exceeded = true;
            }
        }

        // Outside (or after) a refresh, send any pads whose value changed.
        if !refresh_active || !budget_exceeded {
            for (i, (&next_val, led)) in next.iter().zip(self.leds.iter_mut()).enumerate() {
                if next_val == *led {
                    continue;
                }
                if sent_this_render >= MAX_SENDS_PER_RENDER {
                    budget_exceeded = true;
                    break;
                }
                let x = (i % WIDTH) as u8;
                let y = (i / WIDTH) as u8;
                if send(MIDIGRID_CHANNEL, y * 16 + x, next_val) {
                    *led = next_val;
                    sent_this_render += 1;
                } else {
                    send_failed = true;
                }
            }
        }

        self.dirty = send_failed || budget_exceeded;
    }
}

// ---------------------------------------------------------------------------
// Event handling
// ---------------------------------------------------------------------------

impl Grid {
    /// The group the selector is currently on.
    fn active_group(&self) -> ParamGroup {
        ParamGroup::ADDRESSABLE[(self.group as usize).min(ParamGroup::ADDRESSABLE.len() - 1)]
    }

    fn on_pad_down<E, const N: usize>(&mut self, x: u8, y: u8, now_ms: u32, engine: &mut E)
    where
        E: DeviceEngine<N>,
        <E::Slot as Slot<N>>::Id: DeviceModel<N>,
    {
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
                group: self.group,
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

    fn on_pad_up<E, const N: usize>(&mut self, x: u8, y: u8, now_ms: u32, engine: &mut E)
    where
        E: DeviceEngine<N>,
        <E::Slot as Slot<N>>::Id: DeviceModel<N>,
    {
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

    fn handle_mixer_tap<E, const N: usize>(&mut self, x: u8, y: u8, engine: &mut E)
    where
        E: DeviceEngine<N>,
        <E::Slot as Slot<N>>::Id: DeviceModel<N>,
    {
        let n_tracks = engine.tracks().len();
        if (x as usize) < n_tracks && x < 8 {
            // Vertical fader tap: jump LEVEL to the coarse value for this row.
            let track = x as usize;
            let value = vertical_fader_value(y);
            let vel = value as f32 / 127.0;
            engine.tracks_mut()[track].set_param(SLOT_LEVEL, vel);
            return;
        }

        if y == 0 {
            // Track select (S1–S8).
            let track = x - 8;
            if (track as usize) < n_tracks {
                self.selected_track = track;
                self.screen = Screen::Parameter;
            }
            return;
        }

        if y == 7 {
            // Audition / trigger pads (A1–A8).
            let track = x - 8;
            if (track as usize) < n_tracks {
                let vel = self.global_velocity as f32 / 127.0;
                engine.trigger_channel(track, TRIGGER_NOTE, vel);
            }
        }
    }

    fn handle_parameter_tap<E, const N: usize>(&mut self, x: u8, y: u8, engine: &mut E)
    where
        E: DeviceEngine<N>,
        <E::Slot as Slot<N>>::Id: DeviceModel<N>,
    {
        let track = self.selected_track as usize;
        let table = engine.tracks()[track].id().param_table();

        if y == 0 {
            if x == 0 {
                // Back to mixer.
                self.screen = Screen::Mixer;
                return;
            }
            if let Some(group) = group_for_col(x) {
                // Group selector. A group the loaded machine has no rows in
                // is not selectable: landing there would leave the screen
                // blank with no indication why.
                if screen_rows(table, group) > 0 {
                    self.group = x - GROUP_COL_FIRST;
                    self.page = 0;
                }
                return;
            }
            if (PAGE_COL_FIRST..PAGE_COL_FIRST + NUM_PAGES as u8).contains(&x) {
                // Page tab within the group. Ignored when the group has no
                // parameters that far in, for the same reason.
                let page = x - PAGE_COL_FIRST;
                if (page as usize) * ROWS_PER_PAGE < screen_rows(table, self.active_group()) {
                    self.page = page;
                }
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
        let Some(widget) = page_widget(table, self.active_group(), self.page, row) else {
            return;
        };

        match widget.kind {
            WidgetKind::UnipolarFader | WidgetKind::BipolarPan => {
                // Quick tap jumps to the coarse base value for this column.
                let value = x * 8;
                write_target(&widget.target, &mut engine.tracks_mut()[track], value);
            }
            WidgetKind::EnumSelector { count } => {
                let option = enum_option_for_col(count, x);
                let value = if count <= 1 {
                    0
                } else {
                    (option as u16 * 127 / (count as u16 - 1)) as u8
                };
                write_target(&widget.target, &mut engine.tracks_mut()[track], value);
            }
            WidgetKind::BitmaskToggle => {
                let mask = read_target(&widget.target, &engine.tracks()[track]);
                let new_mask = mask ^ (1 << x);
                write_target(&widget.target, &mut engine.tracks_mut()[track], new_mask);
            }
            WidgetKind::VerticalFader => {
                // Not used on the parameter screen.
            }
        }
    }

    fn apply_fine_tune_drag<E, const N: usize>(&mut self, x: u8, y: u8, engine: &mut E)
    where
        E: DeviceEngine<N>,
        <E::Slot as Slot<N>>::Id: DeviceModel<N>,
    {
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
                if track < engine.tracks().len() {
                    engine.tracks_mut()[track].set_param(SLOT_LEVEL, value as f32 / 127.0);
                }
            }
            Screen::Parameter => {
                // Vertical fine-tune strip in the held column.
                if x != held.x {
                    return;
                }
                let row = held.y as usize - 1;
                let track_idx = held.track as usize;
                let table = engine.tracks()[track_idx].id().param_table();
                let Some(widget) = page_widget(table, held_group(&held), held.page, row) else {
                    return;
                };
                if !widget_supports_fine_tune(widget.kind) {
                    return;
                }
                let base = held.x * 8;
                // Top row (y=0) = +7, bottom row (y=7) = +0.
                let offset = 7u8.saturating_sub(y).min(7);
                let value = (base as u16 + offset as u16).min(127) as u8;
                write_target(&widget.target, &mut engine.tracks_mut()[track_idx], value);
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

fn read_target<S, const N: usize>(target: &ParamTarget, track: &Track<S, N>) -> u8
where
    S: Slot<N>,
{
    match target {
        ParamTarget::Param(idx) => (track.base_params[*idx].clamp(0.0, 1.0) * 127.0 + 0.5) as u8,
        ParamTarget::ChokeMask => track.strip.choke_mask,
    }
}

fn write_target<S, const N: usize>(target: &ParamTarget, track: &mut Track<S, N>, value: u8)
where
    S: Slot<N>,
{
    let v = value.min(127);
    match target {
        ParamTarget::Param(idx) => {
            // Discrete selectors should snap instantly; continuous faders use
            // the same immediate set for responsive UI feedback.
            track.set_param(*idx, v as f32 / 127.0);
        }
        ParamTarget::ChokeMask => {
            let mut strip = track.strip;
            strip.choke_mask = v;
            track.set_strip(&strip);
        }
    }
}

/// Parameter rows on one page: grid rows 1..=6.
pub const ROWS_PER_PAGE: usize = 6;
/// Page tabs on the chrome row, columns 12..=15.
pub const NUM_PAGES: usize = 4;
/// First column of the group selector on the chrome row.
const GROUP_COL_FIRST: u8 = 1;
/// First column of the page tabs on the chrome row.
const PAGE_COL_FIRST: u8 = 12;

/// The group a gesture was anchored in, so a fine-tune overlay keeps
/// editing the parameter the finger landed on even if the selector moves.
fn held_group(held: &HeldPad) -> ParamGroup {
    ParamGroup::ADDRESSABLE[(held.group as usize).min(ParamGroup::ADDRESSABLE.len() - 1)]
}

/// The group a selector column addresses, or `None` for a column that is
/// not part of the selector.
fn group_for_col(x: u8) -> Option<ParamGroup> {
    let i = x.checked_sub(GROUP_COL_FIRST)? as usize;
    ParamGroup::ADDRESSABLE.get(i).copied()
}

/// How many rows the parameter screen has for a group: its table rows, plus
/// the synthetic choke row that rides along with TRACK.
fn screen_rows<const N: usize>(table: &[ParamInfo; N], group: ParamGroup) -> usize {
    let n = group_len(table, group);
    if group as u8 == ParamGroup::Track as u8 {
        n + 1
    } else {
        n
    }
}

/// The widget for one row of the parameter screen.
///
/// `ordinal` is the parameter's index within `group` — the same number NRPN
/// puts in its parameter-select LSB — reconstructed from the page tab and
/// the grid row. A row past the end of the group has no widget and renders
/// dark.
fn page_widget<const N: usize>(
    table: &[ParamInfo; N],
    group: ParamGroup,
    page: u8,
    row: usize,
) -> Option<Widget> {
    let ordinal = page as usize * ROWS_PER_PAGE + row;
    let index = u8::try_from(ordinal).ok()?;
    if let Some(flat) = resolve(table, ParamId { group, index }) {
        let info = table[flat];
        return Some(Widget {
            kind: widget_kind(info.kind),
            label: info.abbrev,
            target: ParamTarget::Param(flat),
        });
    }
    // One synthetic row after TRACK's own: the choke mask is a strip field,
    // not a parameter, so it has no table row and no NRPN address.
    if group as u8 == ParamGroup::Track as u8 && ordinal == group_len(table, ParamGroup::Track) {
        return Some(Widget {
            kind: WidgetKind::BitmaskToggle,
            label: "CHK",
            target: ParamTarget::ChokeMask,
        });
    }
    None
}

/// How a parameter of this kind is drawn and touched.
fn widget_kind(kind: ParamKind) -> WidgetKind {
    match kind {
        ParamKind::Continuous => WidgetKind::UnipolarFader,
        ParamKind::Bipolar => WidgetKind::BipolarPan,
        // The renderer clamps to 16 clusters; a catalogue larger than that
        // (mi-drum's 28 machines) is reachable in 16 steps, not one per
        // entry. Finer selection is what NRPN is for.
        ParamKind::Discrete { count } => WidgetKind::EnumSelector { count },
    }
}

// ---------------------------------------------------------------------------
// Rendering
// ---------------------------------------------------------------------------

/// Default dim brightness for inactive UI chrome (clearly visible on a
/// varibright grid, but not distracting).
const DIM: u8 = 48;
/// Full brightness for active values / selected controls.
const BRIGHT: u8 = 127;

fn render_mixer<E, const N: usize>(grid: &Grid, engine: &E, next: &mut [u8; NUM_PADS])
where
    E: DeviceEngine<N>,
    <E::Slot as Slot<N>>::Id: DeviceModel<N>,
{
    let n_tracks = engine.tracks().len();

    // Left half: up to 8 vertical level faders.
    for track in 0..n_tracks.min(8) {
        let value = read_target(&ParamTarget::Param(SLOT_LEVEL), &engine.tracks()[track]);
        render_vertical_fader(track, value, next);
    }

    // Row 0, cols 8–15: track select buttons.
    for x in 8..16 {
        let track = x - 8;
        let selected = track == grid.selected_track;
        let vel = if selected { BRIGHT } else { DIM };
        set_pad(next, x, 0, vel);
    }

    // Row 7, cols 8–15: audition pads.
    for x in 8..16 {
        set_pad(next, x, 7, DIM);
    }
}

fn render_parameter<E, const N: usize>(grid: &Grid, engine: &E, next: &mut [u8; NUM_PADS])
where
    E: DeviceEngine<N>,
    <E::Slot as Slot<N>>::Id: DeviceModel<N>,
{
    // Back button.
    set_pad(next, 0, 0, DIM);

    let track_idx = grid.selected_track as usize;
    let table = engine.tracks()[track_idx].id().param_table();
    let group = grid.active_group();

    // Group selector, cols 1–8. Dark for a group this machine has no
    // parameters in, so the lit pads are exactly the reachable ones.
    for (i, &g) in ParamGroup::ADDRESSABLE.iter().enumerate() {
        let rows = screen_rows(table, g);
        let vel = if rows == 0 {
            0
        } else if g as u8 == group as u8 {
            BRIGHT
        } else {
            DIM
        };
        set_pad(next, GROUP_COL_FIRST + i as u8, 0, vel);
    }

    // Page tabs, cols 12–15. Same rule: a tab with nothing on it is dark.
    let rows = screen_rows(table, group);
    for page in 0..NUM_PAGES {
        let vel = if page * ROWS_PER_PAGE >= rows {
            0
        } else if page as u8 == grid.page {
            BRIGHT
        } else {
            DIM
        };
        set_pad(next, PAGE_COL_FIRST + page as u8, 0, vel);
    }

    // Parameter rows.
    for row in 0..ROWS_PER_PAGE {
        if let Some(widget) = page_widget(table, group, grid.page, row) {
            let value = read_target(&widget.target, &engine.tracks()[track_idx]);
            render_widget_row(row + 1, &widget, value, next);
        }
    }

    // Audition strip, row 7.
    for x in 0..16 {
        set_pad(next, x, 7, DIM);
    }
}

fn render_fine_tune<E, const N: usize>(grid: &Grid, engine: &E, next: &mut [u8; NUM_PADS])
where
    E: DeviceEngine<N>,
    <E::Slot as Slot<N>>::Id: DeviceModel<N>,
{
    let Some(held) = grid.held else { return };

    match held.screen {
        Screen::Mixer => {
            // Vertical fader held: horizontal strip in the held row.
            let base = vertical_fader_value(held.y);
            let n_tracks = engine.tracks().len();
            if (held.x as usize) < n_tracks {
                let current = read_target(
                    &ParamTarget::Param(SLOT_LEVEL),
                    &engine.tracks()[held.x as usize],
                );
                let offset = (current as i16 - base as i16).clamp(0, 15) as u8;

                // Dim the fader columns to keep context.
                for track in 0..n_tracks.min(8) {
                    let value =
                        read_target(&ParamTarget::Param(SLOT_LEVEL), &engine.tracks()[track]);
                    render_vertical_fader(track, value, next);
                    for y in 0..HEIGHT as u8 {
                        if y != held.y {
                            dim_pad(next, track as u8, y);
                        }
                    }
                }

                // Horizontal fine-tune strip in the held row.
                for x in 0..16 {
                    let vel = if x <= offset { BRIGHT } else { 0 };
                    set_pad(next, x, held.y, vel);
                }
            }
        }
        Screen::Parameter => {
            // Horizontal fader held: vertical strip in the held column.
            let row = held.y as usize - 1;
            let track_idx = held.track as usize;
            let table = engine.tracks()[track_idx].id().param_table();
            let group = held_group(&held);
            let Some(widget) = page_widget(table, group, held.page, row) else {
                return;
            };
            let value = read_target(&widget.target, &engine.tracks()[track_idx]);
            let base = held.x * 8;
            let offset = (value as i16 - base as i16).clamp(0, 7) as u8;

            // Dim the underlying parameter rows for context.
            for r in 0..ROWS_PER_PAGE {
                if let Some(w) = page_widget(table, group, held.page, r) {
                    let v = read_target(&w.target, &engine.tracks()[track_idx]);
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
                let vel = if step <= offset { BRIGHT } else { 0 };
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
            BRIGHT
        } else if x == active_col {
            48 + fine_offset * 10
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
            let vel = if x == 7 || x == 8 { DIM } else { 0 };
            set_pad(next, x as u8, y, vel);
        }
        return;
    }

    if pan_offset < 0 {
        // Fill leftward from column 7.
        let target_col = 7 + (pan_offset / 8); // pan_offset/8 is negative or zero
        let fine = (8 + (pan_offset % 8)) % 8;
        let edge_vel = 48 + (fine as u8 * 10);
        for x in 0..WIDTH {
            let col = x as i16;
            let vel = if col > target_col && col <= 7 {
                BRIGHT
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
        let edge_vel = 48 + (fine * 10);
        for x in 0..WIDTH {
            let col = x as i16;
            let vel = if col < target_col && col >= 8 {
                BRIGHT
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
            BRIGHT
        } else if x % cluster_size == 0 || x % cluster_size == cluster_size - 1 {
            DIM
        } else {
            0
        };
        set_pad(next, x as u8, y, vel);
    }
}

fn render_bitmask(row: usize, mask: u16, next: &mut [u8; NUM_PADS]) {
    let y = row as u8;
    for x in 0..WIDTH {
        let vel = if (mask & (1 << x)) != 0 { BRIGHT } else { 0 };
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
            BRIGHT
        } else if y == active_row {
            // Leading edge: varibright based on fine offset within 16-CC band.
            // Bottom-most row at value 0 still gets a visible cursor.
            48 + fine_offset * 5
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
    next[idx] /= 8;
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
    use crate::engine::Engine;
    use crate::params::{
        level_info, mach_info, mi, resv, ParamInfo, ParamRoute, LFO1_DEPTH_INFO, LFO1_DEST_INFO,
        LFO1_RATE_INFO, OUT_INFO, PAN_INFO, SEND_DLY_INFO, SEND_RVB_INFO, SLOT_STRIP_CUT,
        STRIP_ATK_INFO, STRIP_CUT_INFO, STRIP_DEC_INFO, STRIP_HOLD_INFO, STRIP_RESO_INFO,
    };
    use crate::slot::{Slot, SlotId};

    /// The drum device's layout in miniature: the same banks, the same
    /// track-routed rows in the same places, and two machine-specific VOICE
    /// rows. Grouped, because the grid now addresses rows by group.
    static DUMMY_PARAMS: [ParamInfo; 32] = [
        mi("TUNE", "TUN", 0.5, ParamGroup::Voice), // MACH 0
        mi("DEC", "DEC", 0.5, ParamGroup::Voice),  // MACH 1
        resv(),                                    // MACH 2
        resv(),                                    // MACH 3
        resv(),                                    // MACH 4
        resv(),                                    // MACH 5
        resv(),                                    // MACH 6
        resv(),                                    // MACH 7
        resv(),                                    // FILT 0
        resv(),                                    // FILT 1
        STRIP_CUT_INFO,                            // FILT 2
        STRIP_RESO_INFO,                           // FILT 3
        STRIP_ATK_INFO,                            // FILT 4
        STRIP_HOLD_INFO,                           // FILT 5
        STRIP_DEC_INFO,                            // FILT 6
        resv(),                                    // FILT 7
        mach_info(2),                              // TRACK 0
        OUT_INFO,                                  // TRACK 1
        PAN_INFO,                                  // TRACK 2
        level_info(0.9),                           // TRACK 3
        SEND_DLY_INFO,                             // TRACK 4
        SEND_RVB_INFO,                             // TRACK 5
        resv(),                                    // TRACK 6
        resv(),                                    // TRACK 7
        resv(),                                    // MOD 0
        resv(),                                    // MOD 1
        LFO1_RATE_INFO,                            // MOD 2
        LFO1_DEPTH_INFO,                           // MOD 3
        LFO1_DEST_INFO,                            // MOD 4
        resv(),                                    // MOD 5
        resv(),                                    // MOD 6
        resv(),                                    // MOD 7
    ];

    /// Column of the group selector pad for `group`.
    fn group_col(group: ParamGroup) -> u8 {
        GROUP_COL_FIRST
            + ParamGroup::ADDRESSABLE
                .iter()
                .position(|g| *g as u8 == group as u8)
                .expect("group is addressable") as u8
    }

    /// Dummy slot for testing the generic grid without pulling in the drum
    /// catalogue. Uses the full 32-param layout so the grid's hard-coded slot
    /// constants are in range.
    #[derive(Clone, Copy, PartialEq, Eq, Debug)]
    struct DummyId(usize);

    impl SlotId<32> for DummyId {
        fn index(self) -> usize {
            self.0
        }
        fn from_index(i: usize) -> Option<Self> {
            if i < 2 {
                Some(DummyId(i))
            } else {
                None
            }
        }
        fn count() -> usize {
            2
        }
        fn default_params(self) -> [f32; 32] {
            [0.0; 32]
        }
    }

    impl DeviceModel<32> for DummyId {
        fn param_table(self) -> &'static [ParamInfo; 32] {
            &DUMMY_PARAMS
        }
        fn param_route(self, idx: usize) -> ParamRoute {
            match DUMMY_PARAMS.get(idx) {
                Some(info) => info.route,
                None => ParamRoute::Slot,
            }
        }
        fn label(self) -> &'static str {
            match self.0 {
                0 => "Dummy A",
                _ => "Dummy B",
            }
        }
    }

    struct DummySlot {
        active: bool,
        params: [f32; 32],
    }

    impl Slot<32> for DummySlot {
        type Id = DummyId;
        fn new(_id: Self::Id, params: &[f32; 32]) -> Self {
            Self {
                active: false,
                params: *params,
            }
        }
        fn id(&self) -> Self::Id {
            DummyId(0)
        }
        fn set_params(&mut self, params: &[f32; 32]) {
            self.params = *params;
        }
        fn trigger(&mut self, _velocity: f32) {
            self.active = true;
        }
        fn retune(&mut self, _semis: f32) {}
        fn reset(&mut self) {
            self.active = false;
        }
        fn is_active(&self) -> bool {
            self.active
        }
        fn tick(&mut self) -> f32 {
            if self.active {
                0.1
            } else {
                0.0
            }
        }
    }

    type DummyEngine = Engine<DummySlot, 32, 2>;

    fn collect_events(
        grid: &mut Grid,
        engine: &mut DummyEngine,
        now: u32,
        out: &mut [(u8, u8, u8); 128],
    ) -> usize {
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
        assert_eq!(p.push(0x9F), None);
        assert_eq!(p.push(0x00), None);
        assert_eq!(p.push(0x64), Some(GridEvent::PadDown { x: 0, y: 0 }));

        // Note off for same pad.
        let mut p = GridParser::new();
        assert_eq!(p.push(0x8F), None);
        assert_eq!(p.push(0x00), None);
        assert_eq!(p.push(0x40), Some(GridEvent::PadUp { x: 0, y: 0 }));

        // Zero-velocity note-on is also a release.
        let mut p = GridParser::new();
        assert_eq!(p.push(0x9F), None);
        assert_eq!(p.push(0x10), None); // note 16 = x=0,y=1
        assert_eq!(p.push(0x00), Some(GridEvent::PadUp { x: 0, y: 1 }));
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
        let mut engine = DummyEngine::new_with_kit(&[DummyId(0), DummyId(1)]);

        // Tap S2 (row 0, col 9).
        grid.process_event(GridEvent::PadDown { x: 9, y: 0 }, 0, &mut engine);
        grid.process_event(GridEvent::PadUp { x: 9, y: 0 }, 0, &mut engine);

        assert_eq!(grid.selected_track, 1);
        assert!(matches!(grid.screen, Screen::Parameter));
    }

    #[test]
    fn mixer_fader_sets_level() {
        let mut grid = Grid::new();
        let mut engine = DummyEngine::new_with_kit(&[DummyId(0), DummyId(1)]);

        // Tap top of track 0 fader (row 0, col 0). Value = 7*16 = 112.
        grid.process_event(GridEvent::PadDown { x: 0, y: 0 }, 0, &mut engine);
        grid.process_event(GridEvent::PadUp { x: 0, y: 0 }, 0, &mut engine);

        let expected = 112.0 / 127.0;
        assert!(
            (engine.tracks[0].base_params[SLOT_LEVEL] - expected).abs() < 1e-6,
            "level should jump to 112/127, got {}",
            engine.tracks[0].base_params[SLOT_LEVEL]
        );
    }

    #[test]
    fn parameter_back_returns_to_mixer() {
        let mut grid = Grid::new();
        let mut engine = DummyEngine::new_with_kit(&[DummyId(0), DummyId(1)]);

        grid.screen = Screen::Parameter;
        grid.selected_track = 1;

        grid.process_event(GridEvent::PadDown { x: 0, y: 0 }, 0, &mut engine);
        grid.process_event(GridEvent::PadUp { x: 0, y: 0 }, 0, &mut engine);

        assert!(matches!(grid.screen, Screen::Mixer));
    }

    #[test]
    fn parameter_enum_selector_sets_out() {
        let mut grid = Grid::new();
        let mut engine = DummyEngine::new_with_kit(&[DummyId(0), DummyId(1)]);

        grid.screen = Screen::Parameter;
        // TRACK ordinal 1 is OUT, so page 0 row 1, which is grid row 2.
        grid.group = 0;
        grid.page = 0;
        grid.selected_track = 0;

        // Tap column 5 (second cluster of 4, so Aux1).
        grid.process_event(GridEvent::PadDown { x: 5, y: 2 }, 0, &mut engine);
        grid.process_event(GridEvent::PadUp { x: 5, y: 2 }, 0, &mut engine);

        assert_eq!(engine.tracks[0].strip.out, crate::OutPair::Aux1);
    }

    #[test]
    fn bitmask_toggle_flips_choke_bit() {
        let mut grid = Grid::new();
        let mut engine = DummyEngine::new_with_kit(&[DummyId(0), DummyId(1)]);

        grid.screen = Screen::Parameter;
        // Six TRACK rows fill page 0, so the synthetic choke row is the
        // first row of page 1 — grid row 1.
        grid.group = 0;
        grid.page = 1;
        grid.selected_track = 0;

        // Toggle bit 3.
        grid.process_event(GridEvent::PadDown { x: 3, y: 1 }, 0, &mut engine);
        grid.process_event(GridEvent::PadUp { x: 3, y: 1 }, 0, &mut engine);

        assert_eq!(engine.tracks[0].strip.choke_mask & (1 << 3), 1 << 3);

        // Toggle again.
        grid.process_event(GridEvent::PadDown { x: 3, y: 1 }, 0, &mut engine);
        grid.process_event(GridEvent::PadUp { x: 3, y: 1 }, 0, &mut engine);

        assert_eq!(engine.tracks[0].strip.choke_mask & (1 << 3), 0);
    }

    #[test]
    fn group_selector_switches_groups_and_resets_the_page() {
        let mut grid = Grid::new();
        let mut engine = DummyEngine::new_with_kit(&[DummyId(0), DummyId(1)]);

        grid.screen = Screen::Parameter;
        grid.page = 1;

        let x = group_col(ParamGroup::Filter);
        grid.process_event(GridEvent::PadDown { x, y: 0 }, 0, &mut engine);
        grid.process_event(GridEvent::PadUp { x, y: 0 }, 0, &mut engine);

        assert_eq!(grid.active_group(), ParamGroup::Filter);
        assert_eq!(grid.page, 0, "a group change starts at its first page");

        // FILTER ordinal 0 is the strip cutoff, now on grid row 1.
        grid.process_event(GridEvent::PadDown { x: 5, y: 1 }, 0, &mut engine);
        grid.process_event(GridEvent::PadUp { x: 5, y: 1 }, 0, &mut engine);
        let expected = 40.0 / 127.0;
        assert!(
            (engine.tracks[0].base_params[SLOT_STRIP_CUT] - expected).abs() < 1e-6,
            "cutoff should be 40/127, got {}",
            engine.tracks[0].base_params[SLOT_STRIP_CUT]
        );
    }

    #[test]
    fn a_group_the_machine_does_not_use_is_not_selectable() {
        let mut grid = Grid::new();
        let mut engine = DummyEngine::new_with_kit(&[DummyId(0), DummyId(1)]);

        grid.screen = Screen::Parameter;
        let table = engine.tracks[0].id().param_table();
        assert_eq!(group_len(table, ParamGroup::Shaper), 0, "fixture has none");

        let x = group_col(ParamGroup::Shaper);
        grid.process_event(GridEvent::PadDown { x, y: 0 }, 0, &mut engine);
        grid.process_event(GridEvent::PadUp { x, y: 0 }, 0, &mut engine);

        assert_eq!(
            grid.active_group(),
            ParamGroup::Track,
            "an empty group leaves the selection where it was"
        );
    }

    #[test]
    fn a_page_tab_past_the_end_of_a_group_does_nothing() {
        let mut grid = Grid::new();
        let mut engine = DummyEngine::new_with_kit(&[DummyId(0), DummyId(1)]);

        grid.screen = Screen::Parameter;
        grid.group = ParamGroup::ADDRESSABLE
            .iter()
            .position(|g| *g as u8 == ParamGroup::Voice as u8)
            .unwrap() as u8;

        // VOICE has two rows here: page 1 and beyond are empty.
        grid.process_event(
            GridEvent::PadDown {
                x: PAGE_COL_FIRST + 1,
                y: 0,
            },
            0,
            &mut engine,
        );
        grid.process_event(
            GridEvent::PadUp {
                x: PAGE_COL_FIRST + 1,
                y: 0,
            },
            0,
            &mut engine,
        );
        assert_eq!(grid.page, 0);

        // TRACK's choke row spills onto page 1, which is reachable.
        grid.group = 0;
        grid.process_event(
            GridEvent::PadDown {
                x: PAGE_COL_FIRST + 1,
                y: 0,
            },
            0,
            &mut engine,
        );
        grid.process_event(
            GridEvent::PadUp {
                x: PAGE_COL_FIRST + 1,
                y: 0,
            },
            0,
            &mut engine,
        );
        assert_eq!(grid.page, 1);
    }

    #[test]
    fn widgets_are_derived_from_the_parameter_kind() {
        let table = &DUMMY_PARAMS;

        // TRACK 0 is the machine selector: discrete over the catalogue.
        let mach = page_widget(table, ParamGroup::Track, 0, 0).unwrap();
        assert_eq!(mach.kind, WidgetKind::EnumSelector { count: 2 });
        assert_eq!(mach.label, "MCH");

        // TRACK 2 is pan: bipolar, drawn outward from the centre.
        let pan = page_widget(table, ParamGroup::Track, 0, 2).unwrap();
        assert_eq!(pan.kind, WidgetKind::BipolarPan);

        // A machine row is an ordinary fader.
        let tune = page_widget(table, ParamGroup::Voice, 0, 0).unwrap();
        assert_eq!(tune.kind, WidgetKind::UnipolarFader);
        assert_eq!(tune.target, ParamTarget::Param(0));

        // The LFO destination is discrete over ModDest.
        let dest = page_widget(table, ParamGroup::Lfo, 0, 2).unwrap();
        assert_eq!(
            dest.kind,
            WidgetKind::EnumSelector {
                count: crate::dsp::ModDest::COUNT as u8
            }
        );

        // Past the end of a group there is no row at all.
        assert!(page_widget(table, ParamGroup::Voice, 0, 2).is_none());
        assert!(page_widget(table, ParamGroup::Shaper, 0, 0).is_none());
    }

    #[test]
    fn the_choke_row_follows_the_track_group() {
        let table = &DUMMY_PARAMS;
        let n = group_len(table, ParamGroup::Track);
        assert_eq!(n, 6);
        // Ordinal 6 = page 1, row 0.
        let choke = page_widget(table, ParamGroup::Track, 1, 0).unwrap();
        assert_eq!(choke.target, ParamTarget::ChokeMask);
        assert!(page_widget(table, ParamGroup::Track, 1, 1).is_none());
    }

    #[test]
    fn horizontal_fine_tune_updates_value() {
        let mut grid = Grid::new();
        let mut engine = DummyEngine::new_with_kit(&[DummyId(0), DummyId(1)]);

        grid.screen = Screen::Parameter;
        // FILTER ordinal 0 is the strip cutoff, on grid row 1.
        grid.group = ParamGroup::ADDRESSABLE
            .iter()
            .position(|g| *g as u8 == ParamGroup::Filter as u8)
            .unwrap() as u8;
        grid.page = 0;
        grid.selected_track = 0;

        // Hold column 5 row 1 for long enough, then drag to row 3 in same column.
        grid.process_event(GridEvent::PadDown { x: 5, y: 1 }, 0, &mut engine);
        grid.process_event(GridEvent::PadDown { x: 5, y: 3 }, HOLD_MS + 1, &mut engine);
        grid.process_event(GridEvent::PadUp { x: 5, y: 3 }, HOLD_MS + 2, &mut engine);
        grid.process_event(GridEvent::PadUp { x: 5, y: 1 }, HOLD_MS + 3, &mut engine);

        // Base = 5*8 = 40. Row 3 offset = 7-3 = 4. Total = 44.
        let expected = 44.0 / 127.0;
        assert!(
            (engine.tracks[0].base_params[SLOT_STRIP_CUT] - expected).abs() < 1e-6,
            "cutoff should be 44/127, got {}",
            engine.tracks[0].base_params[SLOT_STRIP_CUT]
        );
    }

    #[test]
    fn vertical_fine_tune_updates_level() {
        let mut grid = Grid::new();
        let mut engine = DummyEngine::new_with_kit(&[DummyId(0), DummyId(1)]);

        // Hold row 4 of track 0 fader, then drag to column 10 in same row.
        grid.process_event(GridEvent::PadDown { x: 0, y: 4 }, 0, &mut engine);
        grid.process_event(GridEvent::PadDown { x: 10, y: 4 }, HOLD_MS + 1, &mut engine);
        grid.process_event(GridEvent::PadUp { x: 10, y: 4 }, HOLD_MS + 2, &mut engine);
        grid.process_event(GridEvent::PadUp { x: 0, y: 4 }, HOLD_MS + 3, &mut engine);

        // Base = (7-4)*16 = 48. Offset = 10. Total = 58.
        let expected = 58.0 / 127.0;
        assert!(
            (engine.tracks[0].base_params[SLOT_LEVEL] - expected).abs() < 1e-6,
            "level should be 58/127, got {}",
            engine.tracks[0].base_params[SLOT_LEVEL]
        );
    }

    #[test]
    fn renderer_emits_channel_16_notes() {
        let mut grid = Grid::new();
        let mut engine = DummyEngine::new_with_kit(&[DummyId(0), DummyId(1)]);

        let mut buf = [(0u8, 0u8, 0u8); 128];
        let n = collect_events(&mut grid, &mut engine, 0, &mut buf);

        // Mixer render should emit updates for the left faders and right buttons.
        assert!(n > 0);
        for event in &buf[..n] {
            assert_eq!(event.0, MIDIGRID_CHANNEL);
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

    #[test]
    fn full_refresh_sends_all_pads() {
        let mut grid = Grid::new();
        let mut engine = DummyEngine::new_with_kit(&[DummyId(0), DummyId(1)]);

        // Initial render at t=0 — only changed pads are emitted.
        let mut first = [(0u8, 0u8, 0u8); 128];
        let n1 = collect_events(&mut grid, &mut engine, 0, &mut first);

        // Full refresh is paced across multiple renders because of the
        // MAX_SENDS_PER_RENDER budget. Keep rendering every RENDER_INTERVAL_MS
        // until every pad has been covered.
        let mut seen = [false; NUM_PADS];
        let mut total = 0usize;
        let mut t = REFRESH_MS + 1;
        for _ in 0..(NUM_PADS / MAX_SENDS_PER_RENDER + 4) {
            let mut buf = [(0u8, 0u8, 0u8); 128];
            let n = collect_events(&mut grid, &mut engine, t, &mut buf);
            total += n;
            for event in &buf[..n] {
                assert_eq!(event.0, MIDIGRID_CHANNEL);
                let note = event.1 as usize;
                assert!(note < NUM_PADS);
                seen[note] = true;
            }
            if seen.iter().all(|&b| b) {
                break;
            }
            t += RENDER_INTERVAL_MS + 1;
        }

        assert!(
            total >= NUM_PADS,
            "full refresh should send all {NUM_PADS} pads, got {total}; first render sent {n1}"
        );
        assert!(
            seen.iter().all(|&b| b),
            "full refresh did not cover every pad"
        );
    }
}
