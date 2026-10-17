//! The Launch Control XL 3 driver: ports, startup, the event loop, repaint.
//!
//! Everything that touches midir or formats a byte lives here or in
//! [`super::wire`]; the UI logic is in [`super::surface`] and is reached
//! only through [`Surface::handle`] and its render accessors.
//!
//! # Startup and shutdown
//!
//! DAW mode is a mode the device is *put into* and must be taken out of
//! again: left in DAW mode after the host exits, the controller's own
//! custom modes are unreachable. [`Driver::shutdown`] runs on ctrl-c as
//! well as on a clean return, which is why the surface is driven from the
//! main thread rather than a midir callback.
//!
//! # Following the device
//!
//! The user can leave DAW mode by hand, and the device reports it on
//! channel 7 CC 30. The driver follows the report rather than assuming its
//! own state is the truth: a mode that is not DAW Control means the surface
//! is showing something else, so the driver re-enters and repaints from
//! scratch (#13, open question 4).
//!
//! # Pacing
//!
//! A transition repaints a title, eight names and forty LEDs. Text is one
//! SysEx per field and an RGB colour is thirteen bytes, so a naive repaint
//! is a burst. Two defences, both lifted from the grid's pacing and neither
//! from its `REFRESH_MS` full-resync, which cost a complete recompute every
//! second whether or not anything was attached:
//!
//! - **Diffing.** The driver keeps the last bytes it sent for every LED and
//!   every text field and sends only what changed.
//! - **A budget.** At most [`MAX_SENDS_PER_RENDER`] messages leave in one
//!   pass, and no pass starts within [`RENDER_INTERVAL_MS`] of the last.
//!   Anything left over goes out on the next pass.
//!
//! Both numbers are first guesses. Phase 2 of #13 asks for a *measured*
//! rate limit, which needs the hardware; `docs/launch-control-xl3.md`
//! records how to measure it.

use std::error::Error;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};

use midir::{Ignore, MidiInput, MidiInputConnection, MidiOutput, MidiOutputConnection};
use rtrb::Producer;

use drum_engine::midi::MidiEvent;

use super::surface::{Addr, Button, Input, Reading, Rgb, Surface, Title, COLUMNS, ENCODERS, ROWS};
use super::wire::{self, Incoming};

/// Substring used to find the controller's DAW ports when none is given.
///
/// The device presents several interfaces; the DAW one is the only one
/// that speaks DAW mode, and on macOS it is named with this suffix.
pub const DEFAULT_PORT_FILTER: &str = "LCXL3";
/// Narrower filter applied on top of [`DEFAULT_PORT_FILTER`], to pick the
/// DAW interface out of the device's several ports.
pub const DAW_PORT_HINT: &str = "DAW";

/// Minimum gap between repaint passes.
const RENDER_INTERVAL_MS: u64 = 10;
/// Messages one repaint pass may send.
const MAX_SENDS_PER_RENDER: usize = 12;

/// How long the per-control temporary display stays up, in tenths of a
/// second. Long enough to read a value after a turn stops, short enough
/// that the stationary display is what you normally see.
const DISPLAY_TIMEOUT_TENTHS: u8 = 12;
/// LED brightness, 0..=127.
const LED_BRIGHTNESS: u8 = 100;
/// Screen brightness, 0..=127.
const SCREEN_BRIGHTNESS: u8 = 100;
/// Encoder acceleration curve: 0 slow, 1 medium, 2 fast.
///
/// Medium. Acceleration is the controller's job by design (`DESIGN.md`,
/// "The control surface is NRPN"), so this is the knob that decides what a
/// fast turn is worth — and what it actually does to the wire is open
/// question 1, which phase 0 settles.
const ENCODER_CURVE: u8 = 1;

/// Fields of display arrangement 3: a title and eight names.
const TITLE_FIELD: u8 = 0;
const NAME_FIELDS: usize = COLUMNS;

/// Options for starting the driver.
pub struct Config {
    /// Substring matched against MIDI port names. `None` uses
    /// [`DEFAULT_PORT_FILTER`].
    pub port: Option<String>,
}

/// The driver's own copy of what the controller is currently showing, so a
/// repaint sends only differences.
struct Painted {
    encoder_leds: [Rgb; ENCODERS],
    track_leds: [Rgb; COLUMNS],
    group_leds: [Rgb; COLUMNS],
    title: String,
    names: [String; NAME_FIELDS],
    /// Which encoder row the screen's eight names are showing.
    row: u8,
}

impl Painted {
    /// A state that matches nothing, so the first repaint sends everything.
    ///
    /// The controller's LEDs and screen are in an unknown state when we
    /// arrive — it may have been showing a custom mode a moment ago — so
    /// "unknown" has to differ from every value the surface can produce.
    /// An impossible colour does that: components are 7-bit on the wire.
    fn unknown() -> Painted {
        const IMPOSSIBLE: Rgb = Rgb {
            r: 0xFF,
            g: 0xFF,
            b: 0xFF,
        };
        Painted {
            encoder_leds: [IMPOSSIBLE; ENCODERS],
            track_leds: [IMPOSSIBLE; COLUMNS],
            group_leds: [IMPOSSIBLE; COLUMNS],
            title: "\u{0}".into(),
            names: std::array::from_fn(|_| "\u{0}".into()),
            row: u8::MAX,
        }
    }
}

/// A connected Launch Control XL 3 in DAW mode.
pub struct Driver<const N: usize> {
    out: MidiOutputConnection,
    surface: Surface<N>,
    painted: Painted,
    /// Encoder row the screen's names are drawn from — the last row
    /// touched, so the screen follows the hand.
    row: u8,
    last_render: Instant,
    pending: bool,
    started: Instant,
}

impl<const N: usize> Driver<N> {
    /// Open the controller, put it in DAW mode, and configure the surface.
    pub fn start(surface: Surface<N>, config: &Config) -> Result<Self, Box<dyn Error>> {
        let out = open_output_port(config.port.as_deref())?;
        let mut driver = Driver {
            out,
            surface,
            painted: Painted::unknown(),
            row: 0,
            last_render: Instant::now() - Duration::from_secs(1),
            pending: true,
            started: Instant::now(),
        };
        driver.enter_daw_mode();
        Ok(driver)
    }

    /// Milliseconds since the driver started — the surface's clock.
    fn now_ms(&self) -> u32 {
        self.started.elapsed().as_millis() as u32
    }

    /// Put the device into DAW mode and set up every feature the surface
    /// depends on.
    ///
    /// Also used to *re*-enter after the device reports it left, so it must
    /// be idempotent and must leave the painted state unknown — the device
    /// has been showing something else and nothing we remember is true.
    fn enter_daw_mode(&mut self) {
        let _ = self.out.send(&wire::daw_mode(true));
        let _ = self
            .out
            .send(&wire::feature(wire::FEAT_MODE, wire::MODE_DAW_CONTROL));

        // Relative output per encoder row. This is what makes the wire's
        // 7 bits stop bounding precision: a detent becomes a delta, and the
        // driver decides what it is worth in 14-bit LSBs.
        for row in [
            wire::FEAT_RELATIVE_ROW1,
            wire::FEAT_RELATIVE_ROW2,
            wire::FEAT_RELATIVE_ROW3,
        ] {
            let _ = self.out.send(&wire::feature(row, 0x7F));
        }
        // Faders are absolute, so a fader physically out of position would
        // jump the level on first touch. Pickup is the fix.
        let _ = self.out.send(&wire::feature(wire::FEAT_FADER_PICKUP, 0x7F));
        // Touch events: used only to keep a turn in progress on its target
        // when a modal opens underneath it.
        let _ = self.out.send(&wire::feature(wire::FEAT_TOUCH, 0x7F));
        let _ = self
            .out
            .send(&wire::feature(wire::FEAT_ENCODER_CURVE, ENCODER_CURVE));
        let _ = self
            .out
            .send(&wire::feature(wire::FEAT_LED_BRIGHTNESS, LED_BRIGHTNESS));
        let _ = self.out.send(&wire::feature(
            wire::FEAT_SCREEN_BRIGHTNESS,
            SCREEN_BRIGHTNESS,
        ));
        let _ = self.out.send(&wire::feature(
            wire::FEAT_DISPLAY_TIMEOUT,
            DISPLAY_TIMEOUT_TENTHS,
        ));

        // The stationary display carries the screen: arrangement 3 is a
        // title and eight names. The automatic-temporary bits are cleared
        // because the controller's own temporary display shows the raw CC
        // and a number out of 127, and the driver is about to send better.
        let _ = self.out.send(&wire::display_config(
            wire::TARGET_STATIONARY,
            wire::ARRANGEMENT_TITLE_AND_8,
        ));

        self.painted = Painted::unknown();
        self.pending = true;
    }

    /// Take the device back out of DAW mode.
    pub fn shutdown(&mut self) {
        let _ = self.out.send(&wire::daw_mode(false));
    }

    /// Feed one decoded controller message to the surface, pushing any
    /// engine events into `events`.
    ///
    /// Returns `false` if the queue was full and an event was dropped,
    /// which the caller reports rather than hides.
    pub fn on_incoming(&mut self, msg: Incoming, events: &mut Producer<MidiEvent>) -> bool {
        let now = self.now_ms();
        let mut ok = true;

        // The device telling us the user changed mode. Anything but DAW
        // Control means the surface is no longer ours.
        if let Incoming::Mode(mode) = msg {
            if mode != wire::MODE_DAW_CONTROL {
                self.enter_daw_mode();
            }
            return ok;
        }

        // Keep the screen's eight names on the row the hand is on: the
        // stationary display's arrangement holds one row of eight, and
        // there are three rows of encoders.
        let row = match msg {
            Incoming::Encoder { row, .. }
            | Incoming::Touch {
                control: wire::Control::Encoder { row, .. },
                ..
            } => Some(row),
            _ => None,
        };
        if let Some(row) = row {
            if self.row != row {
                self.row = row;
                self.pending = true;
            }
        }

        let Some(input) = to_input(msg) else {
            return ok;
        };
        let outcome = self.surface.handle(input, now);
        for ev in outcome.events() {
            if events.push(ev).is_err() {
                ok = false;
            }
        }
        if outcome.repaint {
            self.pending = true;
        }
        if let Some(addr) = outcome.touched {
            self.show_readout(addr);
        }
        ok
    }

    /// Raise the per-control temporary display for the parameter a control
    /// just moved, so it reads `STRIP.CUT` and a value in the parameter's
    /// own terms rather than `CC 77` and a number out of 127.
    fn show_readout(&mut self, addr: Addr) {
        let Some(readout) = self.surface.readout(addr) else {
            return;
        };
        let Some(control) = self.control_for(addr) else {
            return;
        };
        let target = wire::display_target(control.index());
        let value = match readout.reading {
            Reading::Continuous(v) => format!("{:.0}%", v * 100.0),
            Reading::Bipolar(v) => format!("{:+.0}", v * 100.0),
            Reading::Discrete { option, count } => match readout.machine_label {
                Some(label) => label.to_string(),
                None => format!("{}/{}", option + 1, count),
            },
        };
        // Arrangement 4 — "Parameter Name and Numeric Parameter Value" — is
        // the per-control default and the right shape for one control.
        let _ = self.out.send(&wire::display_config(target, 4));
        send_text(&mut self.out, target, 0, readout.name);
        send_text(&mut self.out, target, 1, &value);
    }

    /// The physical control an address is currently under, if any — needed
    /// to aim a per-control display at the right encoder.
    fn control_for(&self, addr: Addr) -> Option<wire::Control> {
        for row in 0..ROWS as u8 {
            for column in 0..COLUMNS as u8 {
                if self.surface.encoder_addr(row, column) == Some(addr) {
                    return Some(wire::Control::Encoder { row, column });
                }
            }
        }
        for column in 0..COLUMNS as u8 {
            if self.surface.fader_addr(column) == Some(addr) {
                return Some(wire::Control::Fader { column });
            }
        }
        None
    }

    /// Send whatever part of the surface has changed, within the pass
    /// budget. Call it often; it rate-limits itself.
    pub fn render(&mut self) {
        if !self.pending {
            return;
        }
        if self.last_render.elapsed() < Duration::from_millis(RENDER_INTERVAL_MS) {
            return;
        }
        self.last_render = Instant::now();

        let mut budget = MAX_SENDS_PER_RENDER;
        let encoder_leds = self.surface.encoder_leds();
        let track_leds = self.surface.track_leds();
        let group_leds = self.surface.group_leds();
        let names = self.surface.encoder_names();
        let title = format_title(self.surface.title(), self.row);

        // Title first, then names, then LEDs: if the budget runs out, the
        // half-painted surface should still say what state it is in.
        if title != self.painted.title && budget > 0 {
            send_text(&mut self.out, wire::TARGET_STATIONARY, TITLE_FIELD, &title);
            self.painted.title = title;
            budget -= 1;
        }

        let base = self.row as usize * COLUMNS;
        for i in 0..NAME_FIELDS {
            if budget == 0 {
                self.pending = true;
                return;
            }
            let text = names[base + i].unwrap_or("");
            if self.painted.row == self.row && self.painted.names[i] == text {
                continue;
            }
            send_text(
                &mut self.out,
                wire::TARGET_STATIONARY,
                TITLE_FIELD + 1 + i as u8,
                text,
            );
            self.painted.names[i] = text.to_string();
            budget -= 1;
        }
        self.painted.row = self.row;

        let mut leds: [(u8, Rgb, usize); ENCODERS + 2 * COLUMNS] =
            [(0, Rgb::DARK, 0); ENCODERS + 2 * COLUMNS];
        let mut n = 0;
        for (i, &rgb) in encoder_leds.iter().enumerate() {
            let control = wire::Control::Encoder {
                row: (i / COLUMNS) as u8,
                column: (i % COLUMNS) as u8,
            };
            leds[n] = (control.index(), rgb, i);
            n += 1;
        }
        for (c, &rgb) in track_leds.iter().enumerate() {
            leds[n] = (wire::TRACK_BUTTON_FIRST + c as u8, rgb, ENCODERS + c);
            n += 1;
        }
        for (c, &rgb) in group_leds.iter().enumerate() {
            leds[n] = (
                wire::GROUP_BUTTON_FIRST + c as u8,
                rgb,
                ENCODERS + COLUMNS + c,
            );
            n += 1;
        }

        for &(control, rgb, slot) in leds.iter().take(n) {
            if budget == 0 {
                self.pending = true;
                return;
            }
            let previous = match slot {
                s if s < ENCODERS => self.painted.encoder_leds[s],
                s if s < ENCODERS + COLUMNS => self.painted.track_leds[s - ENCODERS],
                s => self.painted.group_leds[s - ENCODERS - COLUMNS],
            };
            if previous == rgb {
                continue;
            }
            let _ = self.out.send(&wire::led_rgb(control, rgb.r, rgb.g, rgb.b));
            match slot {
                s if s < ENCODERS => self.painted.encoder_leds[s] = rgb,
                s if s < ENCODERS + COLUMNS => self.painted.track_leds[s - ENCODERS] = rgb,
                s => self.painted.group_leds[s - ENCODERS - COLUMNS] = rgb,
            }
            budget -= 1;
        }

        self.pending = false;
    }
}

fn send_text(out: &mut MidiOutputConnection, target: u8, field: u8, text: &str) {
    let (bytes, len) = wire::display_text(target, field, text);
    let _ = out.send(&bytes[..len]);
}

/// Turn a decoded controller message into a surface input.
///
/// Fader and encoder *touch* on a fader is dropped: the surface only uses
/// touch to anchor an encoder turn, and a touched fader has nothing to
/// anchor.
fn to_input(msg: Incoming) -> Option<Input> {
    Some(match msg {
        Incoming::Encoder { row, column, delta } => Input::Encoder { row, column, delta },
        Incoming::Fader { column, value } => Input::Fader { column, value },
        Incoming::Shift(down) => Input::Shift(down),
        Incoming::Button { button, down } => Input::Button {
            button: match button {
                wire::Button::Track(c) => Button::Track(c),
                wire::Button::Group(c) => Button::Group(c),
                wire::Button::PageUp => Button::PageUp,
                wire::Button::PageDown => Button::PageDown,
            },
            down,
        },
        Incoming::Touch {
            control: wire::Control::Encoder { row, column },
            touched,
        } => Input::EncoderTouch {
            row,
            column,
            touched,
        },
        Incoming::Touch { .. } | Incoming::Mode(_) => return None,
    })
}

/// The screen's title line.
///
/// The state's own name plus the page, and — because the eight names below
/// it are one row of three — which row they are. A modal UI that does not
/// show its mode is a worse MIDI mapping, and on a group screen the row is
/// part of the mode.
fn format_title(title: Title, row: u8) -> String {
    match title {
        Title::Macros { page, pages } => {
            if pages > 1 {
                format!("MACRO {}/{} R{}", page + 1, pages, row + 1)
            } else {
                format!("MACRO R{}", row + 1)
            }
        }
        Title::Group { group, page, pages } => {
            if pages > 1 {
                format!("{} {}/{} R{}", group.name(), page + 1, pages, row + 1)
            } else {
                format!("{} R{}", group.name(), row + 1)
            }
        }
    }
}

// ---------------------------------------------------------------------------
// Ports
// ---------------------------------------------------------------------------

/// Open the controller's DAW output port.
pub fn open_output_port(filter: Option<&str>) -> Result<MidiOutputConnection, Box<dyn Error>> {
    let out = MidiOutput::new("drumsynth lcxl3")?;
    let ports = out.ports();
    let names: Vec<String> = ports
        .iter()
        .map(|p| out.port_name(p).unwrap_or_else(|_| "?".into()))
        .collect();
    let i = pick_port(&names, filter).ok_or_else(|| {
        format!(
            "no MIDI output port matching '{}'. available: {}",
            filter.unwrap_or(DEFAULT_PORT_FILTER),
            names.join(", ")
        )
    })?;
    println!("Launch Control XL 3 out: {}", names[i]);
    Ok(out.connect(&ports[i], "lcxl3-out")?)
}

/// Open the controller's DAW input port, delivering decoded messages to
/// `on_message`.
pub fn open_input<F>(
    filter: Option<&str>,
    mut on_message: F,
) -> Result<MidiInputConnection<()>, Box<dyn Error>>
where
    F: FnMut(Incoming) + Send + 'static,
{
    let mut input = MidiInput::new("drumsynth lcxl3")?;
    // SysEx is not needed from the device, but ignoring nothing keeps the
    // timestamps honest and costs nothing at this rate.
    input.ignore(Ignore::None);
    let ports = input.ports();
    let names: Vec<String> = ports
        .iter()
        .map(|p| input.port_name(p).unwrap_or_else(|_| "?".into()))
        .collect();
    let i = pick_port(&names, filter).ok_or_else(|| {
        format!(
            "no MIDI input port matching '{}'. available: {}",
            filter.unwrap_or(DEFAULT_PORT_FILTER),
            names.join(", ")
        )
    })?;
    println!("Launch Control XL 3 in:  {}", names[i]);
    Ok(input.connect(
        &ports[i],
        "lcxl3-in",
        move |_, bytes, _| {
            if bytes.len() >= 3 {
                if let Some(msg) = wire::decode(bytes[0], bytes[1], bytes[2]) {
                    on_message(msg);
                }
            }
        },
        (),
    )?)
}

/// Pick a port by substring, preferring the DAW interface.
///
/// The controller presents several ports and only the DAW one speaks DAW
/// mode, so a match on the device name alone is ambiguous. A port whose
/// name also contains [`DAW_PORT_HINT`] wins; failing that the first
/// match is taken, which is what a user passing an explicit `--port`
/// expects.
pub fn pick_port(names: &[String], filter: Option<&str>) -> Option<usize> {
    let needle = filter.unwrap_or(DEFAULT_PORT_FILTER).to_lowercase();
    let matches: Vec<usize> = names
        .iter()
        .enumerate()
        .filter(|(_, n)| n.to_lowercase().contains(&needle))
        .map(|(i, _)| i)
        .collect();
    matches
        .iter()
        .copied()
        .find(|&i| {
            names[i]
                .to_lowercase()
                .contains(&DAW_PORT_HINT.to_lowercase())
        })
        .or_else(|| matches.first().copied())
}

/// Install a ctrl-c handler and return the flag it clears.
pub fn interrupt_flag() -> Arc<AtomicBool> {
    let running = Arc::new(AtomicBool::new(true));
    let r = running.clone();
    ctrlc::set_handler(move || r.store(false, Ordering::Relaxed)).ok();
    running
}

#[cfg(test)]
mod tests {
    use super::*;

    fn names(v: &[&str]) -> Vec<String> {
        v.iter().map(|s| s.to_string()).collect()
    }

    /// The device presents several ports; only the DAW one speaks DAW mode.
    #[test]
    fn the_daw_port_wins_over_the_other_interfaces() {
        let ports = names(&["LCXL3 1 MIDI In", "LCXL3 1 DAW In", "LCXL3 1 DIN"]);
        assert_eq!(pick_port(&ports, None), Some(1));
    }

    #[test]
    fn an_explicit_filter_without_a_daw_port_takes_the_first_match() {
        let ports = names(&["Other", "Teensy MIDI", "Teensy DIN"]);
        assert_eq!(pick_port(&ports, Some("teensy")), Some(1));
    }

    #[test]
    fn no_match_is_none() {
        assert_eq!(pick_port(&names(&["Nothing"]), None), None);
    }

    #[test]
    fn the_title_names_the_state_the_page_and_the_row() {
        use drum_engine::machines::ParamGroup;
        assert_eq!(
            format_title(
                Title::Group {
                    group: ParamGroup::Shaper,
                    page: 1,
                    pages: 3
                },
                2
            ),
            "SHAPER 2/3 R3"
        );
        // A single-page group does not waste screen width on "1/1".
        assert_eq!(
            format_title(
                Title::Group {
                    group: ParamGroup::Amp,
                    page: 0,
                    pages: 1
                },
                0
            ),
            "AMP R1"
        );
        assert_eq!(
            format_title(Title::Macros { page: 0, pages: 2 }, 0),
            "MACRO 1/2 R1"
        );
    }

    /// Every title must fit the field the wire layer will truncate at,
    /// or the screen silently loses the page number.
    #[test]
    fn titles_fit_the_text_field() {
        use drum_engine::machines::ParamGroup;
        for &group in &ParamGroup::ADDRESSABLE {
            let t = format_title(
                Title::Group {
                    group,
                    page: 8,
                    pages: 9,
                },
                2,
            );
            assert!(t.len() <= wire::MAX_TEXT, "{t:?} is {} chars", t.len());
        }
    }
}
