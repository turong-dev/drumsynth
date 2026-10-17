//! Launch Control XL 3 DAW-mode wire format: constants, encoders, decoder.
//!
//! This is the I/O half of the integration — the part that knows the
//! controller exists. Everything here is byte formatting against the
//! [programmer's reference guide v1.0]; the UI logic it serves lives in
//! [`super::surface`] and knows nothing about any of it.
//!
//! Every message below is on the **DAW In/Out** USB port. The reference is
//! explicit that DAW mode sends nothing on the DIN outputs and nothing on
//! the main MIDI In/Out interface.
//!
//! # What is confirmed and what is not
//!
//! The control-index map (faders 5–12, encoders 13–36, buttons 37–52, and
//! the transport/page/track buttons) is read off the DAW-mode diagram on
//! page 9 of the reference, which is a rendered image rather than text. The
//! numbers were cross-checked against the same diagram's hexadecimal twin on
//! page 10, and against the relative-encoder CCs the prose does give
//! (row 1 = 77–84, row 2 = 85–92, row 3 = 93–100), which are exactly the
//! absolute encoder indices plus 64. That cross-check is what makes
//! 13–36 safe to rely on.
//!
//! What is *not* confirmed is covered in `docs/launch-control-xl3.md` and
//! is what phase 0 of #13 exists to settle — above all what a fast turn of
//! a relative encoder actually sends. [`relative_delta`] is the one
//! function that decision lands in.
//!
//! [programmer's reference guide v1.0]: https://fael-downloads-prod.focusrite.com/customer/prod/downloads/launch_control_xl_3_programmer_s_reference_guide-pdf_en.pdf

// This module is a transcription of DAW mode's wire format, not only of
// the parts the driver happens to use today. The transport and track
// buttons, the mixer mode and the overlay target are all things phase 0
// has to recognise in a capture and phase 2 may reach for; deleting them
// to silence a warning would mean looking them up again off a rendered
// PDF diagram.
#![allow(dead_code)]

// ---------------------------------------------------------------------------
// Channels
// ---------------------------------------------------------------------------

/// Channel the encoders and faders report on (0-indexed; "channel 16").
pub const CH_ANALOGUE: u8 = 15;
/// Channel the buttons report on (0-indexed; "channel 1").
pub const CH_BUTTONS: u8 = 0;
/// Channel the feature controls live on, in both directions (0-indexed;
/// "channel 7"). Shift and the surface-mode report are feature controls,
/// which is why they are not on [`CH_BUTTONS`].
pub const CH_FEATURE: u8 = 6;
/// Channel touch events report on (0-indexed; "channel 15").
pub const CH_TOUCH: u8 = 14;

// ---------------------------------------------------------------------------
// Control indices
// ---------------------------------------------------------------------------
//
// One numbering serves three purposes: the CC a control reports on, the
// index an LED colour message addresses, and the per-control temporary
// display target. That is why these are named "control index" rather than
// "CC" — see `led_rgb` and `display_config`.

/// First fader. Faders are 5..=12, left to right.
pub const FADER_FIRST: u8 = 5;
/// Number of faders, and of columns on the surface.
pub const COLUMNS: usize = 8;

/// First encoder, top-left. Encoders are 13..=36 in reading order: row 1 is
/// 13..=20, row 2 is 21..=28, row 3 is 29..=36.
pub const ENCODER_FIRST: u8 = 13;
/// Encoder rows.
pub const ROWS: usize = 3;
/// Total encoders.
pub const ENCODERS: usize = ROWS * COLUMNS;

/// First button of the upper button row (the "Solo/Arm" row), 37..=44.
/// #13 uses this row for track select.
pub const TRACK_BUTTON_FIRST: u8 = 37;
/// First button of the lower button row (the "Mute/Select" row), 45..=52.
/// #13 uses this row for the eight addressable parameter groups.
pub const GROUP_BUTTON_FIRST: u8 = 45;

/// Page ▲.
pub const BTN_PAGE_UP: u8 = 106;
/// Page ▼.
pub const BTN_PAGE_DOWN: u8 = 107;
/// Track ◀.
pub const BTN_TRACK_LEFT: u8 = 101;
/// Track ▶.
pub const BTN_TRACK_RIGHT: u8 = 102;
/// Play ▶.
pub const BTN_PLAY: u8 = 116;
/// Record ●.
pub const BTN_RECORD: u8 = 118;

/// Relative-mode encoder CCs are the absolute index plus this.
///
/// Row 1 therefore reports on 77..=84, row 2 on 85..=92, row 3 on 93..=100,
/// which is what the reference's prose states directly. The LED and display
/// messages keep using the *absolute* index either way: "Encoder LEDs can be
/// addressed using the absolute mode control indices when encoders are in
/// either absolute or relative mode."
pub const RELATIVE_CC_OFFSET: u8 = 64;

/// Pivot of the relative encoding: 65 is one detent clockwise, 63 one
/// anticlockwise.
pub const RELATIVE_PIVOT: u8 = 64;

// ---------------------------------------------------------------------------
// Feature controls (channel 7)
// ---------------------------------------------------------------------------

/// Surface mode select, and the report the device sends when the *user*
/// changes mode. Following this is not optional: a user who leaves DAW mode
/// by hand takes the whole surface with them.
pub const FEAT_MODE: u8 = 30;
/// Shift. A feature control rather than a button, hence channel 7.
pub const FEAT_SHIFT: u8 = 63;
/// Relative output for encoder row 1.
pub const FEAT_RELATIVE_ROW1: u8 = 69;
/// Relative output for encoder row 2.
pub const FEAT_RELATIVE_ROW2: u8 = 72;
/// Relative output for encoder row 3.
pub const FEAT_RELATIVE_ROW3: u8 = 73;
/// Fader pickup: a fader out of position does not take effect until it
/// passes through the current value.
pub const FEAT_FADER_PICKUP: u8 = 70;
/// Continuous-control touch events.
pub const FEAT_TOUCH: u8 = 71;
/// LED brightness, 0..=127.
pub const FEAT_LED_BRIGHTNESS: u8 = 111;
/// Screen brightness, 0..=127.
pub const FEAT_SCREEN_BRIGHTNESS: u8 = 112;
/// Temporary-display timeout in tenths of a second, 0..=99 (0 means 1 s).
pub const FEAT_DISPLAY_TIMEOUT: u8 = 113;
/// Encoder acceleration curve: 0 slow, 1 medium, 2 fast.
pub const FEAT_ENCODER_CURVE: u8 = 121;

/// Surface mode: DAW Mixer.
pub const MODE_DAW_MIXER: u8 = 1;
/// Surface mode: DAW Control.
pub const MODE_DAW_CONTROL: u8 = 2;

// ---------------------------------------------------------------------------
// SysEx
// ---------------------------------------------------------------------------

/// Novation header every Launch Control XL 3 SysEx message starts with,
/// in both directions.
pub const SYSEX_HEADER: [u8; 6] = [0xF0, 0x00, 0x20, 0x29, 0x02, 0x15];

/// Command byte: enable/disable DAW mode.
const CMD_DAW_MODE: u8 = 0x02;
/// Command byte: set an LED to an explicit RGB colour (sub-command 0x53).
const CMD_COLOUR: u8 = 0x01;
/// Sub-command under [`CMD_COLOUR`] for an RGB write.
const SUB_COLOUR_RGB: u8 = 0x53;
/// Command byte: configure a display target.
const CMD_DISPLAY_CONFIG: u8 = 0x04;
/// Command byte: set a display target's text field.
const CMD_DISPLAY_TEXT: u8 = 0x06;

/// Display target: the stationary (default) display.
pub const TARGET_STATIONARY: u8 = 0x35;
/// Display target: the global overlay / temporary display.
pub const TARGET_OVERLAY: u8 = 0x36;

/// Display arrangement 3 — "1 line + 2x4: Title and 8 names".
///
/// Nine fields: field 0 is the title, fields 1..=8 are the names under the
/// eight encoders of a row. This is the arrangement #13 specifies for the
/// resting display of whichever screen is up.
pub const ARRANGEMENT_TITLE_AND_8: u8 = 3;

/// Config bit: let the controller raise a temporary display by itself when a
/// control changes. Default set; we clear it, because an automatic display
/// shows the raw CC and value and we are about to send better text.
pub const CFG_AUTO_ON_CHANGE: u8 = 1 << 6;
/// Config bit: same, on touch.
pub const CFG_AUTO_ON_TOUCH: u8 = 1 << 5;

/// Per-control temporary display target for an absolute control index.
///
/// The targets are the control indices themselves — 5..=12 for the faders,
/// 13..=36 for the encoders — which is why one number does CC duty, LED
/// duty and display duty.
pub const fn display_target(control: u8) -> u8 {
    control
}

/// Enable or disable DAW mode.
///
/// The note form (`9F 0C 7F`) and this SysEx form are equivalent; the SysEx
/// is used because it cannot be mistaken for musical data by anything
/// sitting between us and the device.
pub fn daw_mode(on: bool) -> [u8; 9] {
    let mut m = [0u8; 9];
    m[..6].copy_from_slice(&SYSEX_HEADER);
    m[6] = CMD_DAW_MODE;
    m[7] = if on { 0x7F } else { 0x00 };
    m[8] = 0xF7;
    m
}

/// A control change on `channel` (0-indexed).
pub fn cc(channel: u8, controller: u8, value: u8) -> [u8; 3] {
    [0xB0 | (channel & 0x0F), controller & 0x7F, value & 0x7F]
}

/// A feature control write (channel 7).
pub fn feature(control: u8, value: u8) -> [u8; 3] {
    cc(CH_FEATURE, control, value)
}

/// Set one LED to an explicit RGB colour.
///
/// The palette form (`B0 <index> <palette>`) is three bytes against this
/// one's thirteen, and is not used: the palette table in the reference is a
/// rendered image with no machine-readable form, so a palette index would be
/// a number we could not justify in code. RGB says what it means. The cost
/// is paid once per changed LED per repaint, and [`super::driver`] caps that.
///
/// Each component is 7-bit, as every SysEx data byte must be.
pub fn led_rgb(control: u8, r: u8, g: u8, b: u8) -> [u8; 13] {
    let mut m = [0u8; 13];
    m[..6].copy_from_slice(&SYSEX_HEADER);
    m[6] = CMD_COLOUR;
    m[7] = SUB_COLOUR_RGB;
    m[8] = control & 0x7F;
    m[9] = r & 0x7F;
    m[10] = g & 0x7F;
    m[11] = b & 0x7F;
    m[12] = 0xF7;
    m
}

/// Configure a display target's arrangement and automatic behaviour.
///
/// `config` of 0 cancels the display and 127 triggers it with its current
/// contents; anything else is an arrangement in bits 0–4 plus the automatic
/// behaviour bits.
pub fn display_config(target: u8, config: u8) -> [u8; 10] {
    let mut m = [0u8; 10];
    m[..6].copy_from_slice(&SYSEX_HEADER);
    m[6] = CMD_DISPLAY_CONFIG;
    m[7] = target & 0x7F;
    m[8] = config & 0x7F;
    m[9] = 0xF7;
    m
}

/// Longest text the device is asked to render in one field.
///
/// Not a documented limit — the reference states the character set and says
/// nothing about length. It is a self-imposed cap so one field cannot push a
/// repaint over the pacing budget, and 16 is comfortably wider than the
/// four-character `abbrev`s the eight-name arrangement carries.
pub const MAX_TEXT: usize = 16;

/// Bytes a [`display_text`] message can occupy: the six-byte header, the
/// command, target and field, [`MAX_TEXT`] characters, and the terminator.
pub const TEXT_MSG_MAX: usize = 9 + MAX_TEXT + 1;

/// Set one text field of a display target.
///
/// Returns the bytes and their length — a fixed array rather than a `Vec`
/// because this module is written to the same no-alloc discipline as the
/// rest of the engine, so phase 3 can take it to the Teensy unchanged.
///
/// Characters outside the device's ASCII range 0x20..=0x7E are replaced with
/// `?` rather than dropped: a name that silently loses a character is harder
/// to diagnose on a 128-pixel screen than one that shows the damage. The
/// reassigned control codes (0x1B empty box, 0x1C filled box, 0x1D flat,
/// 0x1E heart) are deliberately not reachable from here; nothing in the
/// parameter table wants them.
pub fn display_text(target: u8, field: u8, text: &str) -> ([u8; TEXT_MSG_MAX], usize) {
    let mut m = [0u8; TEXT_MSG_MAX];
    m[..6].copy_from_slice(&SYSEX_HEADER);
    m[6] = CMD_DISPLAY_TEXT;
    m[7] = target & 0x7F;
    m[8] = field & 0x7F;
    let mut n = 9;
    for c in text.chars() {
        if n >= 9 + MAX_TEXT {
            break;
        }
        let b = c as u32;
        m[n] = if (0x20..=0x7E).contains(&b) {
            b as u8
        } else {
            b'?'
        };
        n += 1;
    }
    m[n] = 0xF7;
    (m, n + 1)
}

// ---------------------------------------------------------------------------
// Decoding
// ---------------------------------------------------------------------------

/// Which physical control a message came from, in surface coordinates.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Control {
    /// Encoder at `row` (0..3, top to bottom) and `column` (0..8).
    Encoder { row: u8, column: u8 },
    /// Fader under `column`.
    Fader { column: u8 },
}

impl Control {
    /// The absolute control index this control is addressed by — its LED
    /// index and its per-control display target, both.
    pub const fn index(self) -> u8 {
        match self {
            Control::Encoder { row, column } => ENCODER_FIRST + row * COLUMNS as u8 + column,
            Control::Fader { column } => FADER_FIRST + column,
        }
    }

    /// The control an absolute index names, or `None` if the index is not a
    /// fader or an encoder.
    pub const fn from_index(index: u8) -> Option<Control> {
        if index >= FADER_FIRST && index < FADER_FIRST + COLUMNS as u8 {
            return Some(Control::Fader {
                column: index - FADER_FIRST,
            });
        }
        if index >= ENCODER_FIRST && index < ENCODER_FIRST + ENCODERS as u8 {
            let n = index - ENCODER_FIRST;
            return Some(Control::Encoder {
                row: n / COLUMNS as u8,
                column: n % COLUMNS as u8,
            });
        }
        None
    }
}

/// A button the driver has a use for.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Button {
    /// Upper button row, column 0..8 — track select.
    Track(u8),
    /// Lower button row, column 0..8 — parameter group.
    Group(u8),
    /// Page ▲.
    PageUp,
    /// Page ▼.
    PageDown,
}

/// One decoded message from the controller.
///
/// Anything the driver has no use for decodes to `None` rather than a
/// catch-all variant: the surface should not have to pattern-match past
/// controls it does not drive.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Incoming {
    /// A relative encoder detent. `delta` is signed steps.
    Encoder { row: u8, column: u8, delta: i8 },
    /// A fader moved, 7-bit absolute.
    Fader { column: u8, value: u8 },
    /// A button went down or came up.
    Button { button: Button, down: bool },
    /// Shift went down or came up.
    Shift(bool),
    /// A continuous control was touched or released.
    Touch { control: Control, touched: bool },
    /// The surface changed mode — either because we asked, or because the
    /// user did.
    Mode(u8),
}

/// Signed detents from a relative-mode data byte.
///
/// The reference gives the pivot and the two unit steps — 65 is +1, 63 is
/// −1 — and says nothing about what a *fast* turn sends. Two readings are
/// possible and they are not equivalent: either the magnitude grows with
/// speed (binary offset, so 70 is +6) or the messages simply arrive more
/// often and the magnitude stays 1. This function assumes the first, which
/// is the one Novation's "Using Relative Encoders" article describes and the
/// one that lets the delta pass straight through to CC 96/97 as a step size.
///
/// **If phase 0's capture shows otherwise, this function is the only thing
/// that changes.** Under the second reading it would clamp to the sign and
/// the driver would apply its own acceleration; the surface above it is
/// written in detents either way and would not notice.
pub const fn relative_delta(value: u8) -> i8 {
    (value as i8).wrapping_sub(RELATIVE_PIVOT as i8)
}

/// Decode one channel-voice message into something the surface understands.
///
/// Takes a parsed `(status, data1, data2)` rather than a byte stream: the
/// driver already has midir handing it whole messages, and a second
/// byte-level parser next to [`drum_engine::midi::MidiParser`] would be one
/// parser too many.
pub fn decode(status: u8, d1: u8, d2: u8) -> Option<Incoming> {
    if status & 0xF0 != 0xB0 {
        return None;
    }
    let channel = status & 0x0F;
    match channel {
        CH_ANALOGUE => decode_analogue(d1, d2),
        CH_BUTTONS => decode_button(d1, d2),
        CH_FEATURE => match d1 {
            FEAT_SHIFT => Some(Incoming::Shift(d2 >= 64)),
            FEAT_MODE => Some(Incoming::Mode(d2)),
            _ => None,
        },
        CH_TOUCH => Control::from_index(d1).map(|control| Incoming::Touch {
            control,
            touched: d2 >= 64,
        }),
        _ => None,
    }
}

fn decode_analogue(cc: u8, value: u8) -> Option<Incoming> {
    if (FADER_FIRST..FADER_FIRST + COLUMNS as u8).contains(&cc) {
        return Some(Incoming::Fader {
            column: cc - FADER_FIRST,
            value,
        });
    }
    // Relative encoders. Absolute-mode encoder CCs (13..=36) are deliberately
    // not decoded: the driver puts every row into relative mode at startup,
    // and an absolute message arriving afterwards means the device did not
    // take the mode change — which should be visible as a dead encoder
    // rather than silently handled as a jump to an absolute position.
    let rel_first = ENCODER_FIRST + RELATIVE_CC_OFFSET;
    if (rel_first..rel_first + ENCODERS as u8).contains(&cc) {
        let n = cc - rel_first;
        let delta = relative_delta(value);
        if delta == 0 {
            return None;
        }
        return Some(Incoming::Encoder {
            row: n / COLUMNS as u8,
            column: n % COLUMNS as u8,
            delta,
        });
    }
    None
}

fn decode_button(cc: u8, value: u8) -> Option<Incoming> {
    let down = value >= 64;
    let button = if (TRACK_BUTTON_FIRST..TRACK_BUTTON_FIRST + COLUMNS as u8).contains(&cc) {
        Button::Track(cc - TRACK_BUTTON_FIRST)
    } else if (GROUP_BUTTON_FIRST..GROUP_BUTTON_FIRST + COLUMNS as u8).contains(&cc) {
        Button::Group(cc - GROUP_BUTTON_FIRST)
    } else if cc == BTN_PAGE_UP {
        Button::PageUp
    } else if cc == BTN_PAGE_DOWN {
        Button::PageDown
    } else {
        return None;
    };
    Some(Incoming::Button { button, down })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn daw_mode_matches_the_reference() {
        assert_eq!(
            daw_mode(true),
            [0xF0, 0x00, 0x20, 0x29, 0x02, 0x15, 0x02, 0x7F, 0xF7]
        );
        assert_eq!(
            daw_mode(false),
            [0xF0, 0x00, 0x20, 0x29, 0x02, 0x15, 0x02, 0x00, 0xF7]
        );
    }

    /// The relative CCs the reference states in prose must be exactly the
    /// absolute control indices plus 64. This is the cross-check that makes
    /// the control-index map — read off a rendered diagram — trustworthy.
    #[test]
    fn relative_rows_are_the_absolute_indices_plus_64() {
        let row = |r: u8| Control::Encoder { row: r, column: 0 }.index() + RELATIVE_CC_OFFSET;
        assert_eq!(row(0), 77);
        assert_eq!(row(1), 85);
        assert_eq!(row(2), 93);
        let last = Control::Encoder { row: 2, column: 7 }.index() + RELATIVE_CC_OFFSET;
        assert_eq!(last, 100);
    }

    #[test]
    fn control_index_round_trips() {
        for row in 0..ROWS as u8 {
            for column in 0..COLUMNS as u8 {
                let c = Control::Encoder { row, column };
                assert_eq!(Control::from_index(c.index()), Some(c));
            }
        }
        for column in 0..COLUMNS as u8 {
            let c = Control::Fader { column };
            assert_eq!(Control::from_index(c.index()), Some(c));
        }
        // Nothing between, below or above the two blocks is a control.
        assert_eq!(Control::from_index(4), None);
        assert_eq!(Control::from_index(37), None);
    }

    #[test]
    fn faders_are_five_to_twelve_and_encoders_thirteen_to_thirtysix() {
        assert_eq!(Control::Fader { column: 0 }.index(), 5);
        assert_eq!(Control::Fader { column: 7 }.index(), 12);
        assert_eq!(Control::Encoder { row: 0, column: 0 }.index(), 13);
        assert_eq!(Control::Encoder { row: 2, column: 7 }.index(), 36);
    }

    #[test]
    fn relative_delta_is_signed_around_the_pivot() {
        assert_eq!(relative_delta(65), 1);
        assert_eq!(relative_delta(63), -1);
        assert_eq!(relative_delta(64), 0);
        assert_eq!(relative_delta(70), 6);
        assert_eq!(relative_delta(58), -6);
    }

    #[test]
    fn decodes_a_relative_encoder_turn() {
        // Row 2, column 3 is relative CC 85 + 3 = 88.
        let got = decode(0xB0 | CH_ANALOGUE, 88, 65);
        assert_eq!(
            got,
            Some(Incoming::Encoder {
                row: 1,
                column: 3,
                delta: 1
            })
        );
    }

    #[test]
    fn a_zero_delta_is_not_an_event() {
        assert_eq!(decode(0xB0 | CH_ANALOGUE, 77, RELATIVE_PIVOT), None);
    }

    /// An absolute-mode encoder message means the relative-mode enable did
    /// not take. Decoding it as a turn would hide that.
    #[test]
    fn absolute_encoder_messages_are_not_decoded() {
        assert_eq!(decode(0xB0 | CH_ANALOGUE, 13, 100), None);
    }

    #[test]
    fn decodes_faders() {
        assert_eq!(
            decode(0xB0 | CH_ANALOGUE, 5, 64),
            Some(Incoming::Fader {
                column: 0,
                value: 64
            })
        );
        assert_eq!(
            decode(0xB0 | CH_ANALOGUE, 12, 127),
            Some(Incoming::Fader {
                column: 7,
                value: 127
            })
        );
    }

    #[test]
    fn decodes_both_button_rows() {
        assert_eq!(
            decode(0xB0 | CH_BUTTONS, 37, 127),
            Some(Incoming::Button {
                button: Button::Track(0),
                down: true
            })
        );
        assert_eq!(
            decode(0xB0 | CH_BUTTONS, 52, 0),
            Some(Incoming::Button {
                button: Button::Group(7),
                down: false
            })
        );
        assert_eq!(
            decode(0xB0 | CH_BUTTONS, BTN_PAGE_UP, 127),
            Some(Incoming::Button {
                button: Button::PageUp,
                down: true
            })
        );
    }

    /// Shift is on the feature channel, not the button channel. Decoding it
    /// on channel 1 would make it unreachable.
    #[test]
    fn shift_is_on_the_feature_channel() {
        assert_eq!(
            decode(0xB0 | CH_FEATURE, FEAT_SHIFT, 127),
            Some(Incoming::Shift(true))
        );
        assert_eq!(decode(0xB0 | CH_BUTTONS, FEAT_SHIFT, 127), None);
    }

    #[test]
    fn decodes_the_mode_report() {
        assert_eq!(
            decode(0xB0 | CH_FEATURE, FEAT_MODE, MODE_DAW_CONTROL),
            Some(Incoming::Mode(MODE_DAW_CONTROL))
        );
    }

    #[test]
    fn decodes_touch_against_the_absolute_index() {
        assert_eq!(
            decode(0xB0 | CH_TOUCH, 5, 127),
            Some(Incoming::Touch {
                control: Control::Fader { column: 0 },
                touched: true
            })
        );
        assert_eq!(
            decode(0xB0 | CH_TOUCH, 36, 0),
            Some(Incoming::Touch {
                control: Control::Encoder { row: 2, column: 7 },
                touched: false
            })
        );
    }

    #[test]
    fn non_cc_status_decodes_to_nothing() {
        assert_eq!(decode(0x90, 60, 100), None);
    }

    #[test]
    fn led_rgb_matches_the_reference() {
        assert_eq!(
            led_rgb(13, 0x7F, 0x00, 0x40),
            [0xF0, 0x00, 0x20, 0x29, 0x02, 0x15, 0x01, 0x53, 13, 0x7F, 0x00, 0x40, 0xF7]
        );
    }

    #[test]
    fn display_text_is_framed_and_ascii_clamped() {
        let (m, n) = display_text(TARGET_STATIONARY, 1, "CUT");
        assert_eq!(
            &m[..n],
            &[0xF0, 0x00, 0x20, 0x29, 0x02, 0x15, 0x06, 0x35, 1, b'C', b'U', b'T', 0xF7]
        );
    }

    #[test]
    fn display_text_replaces_out_of_range_characters() {
        let (m, n) = display_text(TARGET_OVERLAY, 0, "f\u{00f8}o");
        assert_eq!(&m[9..n - 1], b"f?o");
    }

    /// A long name must not run off the end of the buffer, and must still be
    /// terminated — an unterminated SysEx would swallow everything after it.
    #[test]
    fn display_text_truncates_and_still_terminates() {
        let long = "ABCDEFGHIJKLMNOPQRSTUVWXYZ";
        let (m, n) = display_text(TARGET_STATIONARY, 8, long);
        assert_eq!(n, 9 + MAX_TEXT + 1);
        assert_eq!(m[n - 1], 0xF7);
        assert_eq!(&m[9..n - 1], &long.as_bytes()[..MAX_TEXT]);
    }

    #[test]
    fn display_config_frames_its_target_and_config() {
        assert_eq!(
            display_config(TARGET_STATIONARY, ARRANGEMENT_TITLE_AND_8),
            [0xF0, 0x00, 0x20, 0x29, 0x02, 0x15, 0x04, 0x35, 3, 0xF7]
        );
    }

    #[test]
    fn feature_writes_land_on_channel_seven() {
        assert_eq!(feature(FEAT_ENCODER_CURVE, 2), [0xB6, 121, 2]);
    }
}
