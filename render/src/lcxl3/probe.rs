//! `render screen-probe` — find out what actually drives the XL 3's OLED.
//!
//! Phase 0 of #13 in the narrowest possible form. The screen is the part
//! of the reference with the least to go on: the target numbering is
//! stated twice with different numbers (`35h`/`36h` for configure-and-text,
//! `20h`/`21h` for the bitmap command), the arrangement field is overloaded
//! so that 0 cancels, 1–30 configure and 31 triggers, and nothing says
//! whether the *stationary* display — "a default display which is shown
//! unless any event requires a different display" — needs triggering at
//! all.
//!
//! Rather than reason about that, send each candidate in turn with its
//! step number on the screen and let the hardware answer. Every step prints
//! the exact bytes it sent, so a run of this is a capture that can be
//! pasted into the issue.
//!
//! This is deliberately not clever and not reused by the driver: it is an
//! instrument, and its value is that each step varies exactly one thing.

use std::error::Error;
use std::io::{BufRead, Write};

use midir::MidiOutputConnection;

use super::driver;
use super::wire;

/// One thing to try.
struct Step {
    /// What this step varies, in one line.
    what: &'static str,
    /// Display target byte.
    target: u8,
    /// Config byte sent before the text.
    config: u8,
    /// Text fields, in field order from 0.
    fields: &'static [&'static str],
    /// Trigger byte sent after the text, or `None` to send no trigger.
    trigger: Option<u8>,
}

fn steps() -> Vec<Step> {
    // Arrangement 3 with both automatic-display bits clear: what the
    // driver configures.
    const ARR3: u8 = wire::ARRANGEMENT_TITLE_AND_8;
    // The same, with the automatic bits left at their documented default.
    const ARR3_AUTO: u8 = ARR3 | wire::CFG_AUTO_ON_CHANGE | wire::CFG_AUTO_ON_TOUCH;

    vec![
        Step {
            what: "stationary 35h, arrangement 3, NO trigger (the original bug)",
            target: wire::TARGET_STATIONARY,
            config: ARR3,
            fields: &[
                "STEP 1", "ONE", "TWO", "TRI", "FOR", "FIV", "SIX", "SVN", "EGT",
            ],
            trigger: None,
        },
        Step {
            what: "stationary 35h, arrangement 3, trigger 31 (the fix as shipped)",
            target: wire::TARGET_STATIONARY,
            config: ARR3,
            fields: &[
                "STEP 2", "ONE", "TWO", "TRI", "FOR", "FIV", "SIX", "SVN", "EGT",
            ],
            trigger: Some(wire::ARRANGEMENT_TRIGGER),
        },
        Step {
            what: "stationary 35h, arrangement 3, trigger 7Fh (documented shorthand)",
            target: wire::TARGET_STATIONARY,
            config: ARR3,
            fields: &[
                "STEP 3", "ONE", "TWO", "TRI", "FOR", "FIV", "SIX", "SVN", "EGT",
            ],
            trigger: Some(0x7F),
        },
        Step {
            what: "stationary 35h, arrangement 3 with the auto bits SET, trigger 7Fh",
            target: wire::TARGET_STATIONARY,
            config: ARR3_AUTO,
            fields: &[
                "STEP 4", "ONE", "TWO", "TRI", "FOR", "FIV", "SIX", "SVN", "EGT",
            ],
            trigger: Some(0x7F),
        },
        Step {
            what: "overlay 36h, arrangement 3, trigger 7Fh",
            target: wire::TARGET_OVERLAY,
            config: ARR3,
            fields: &[
                "STEP 5", "ONE", "TWO", "TRI", "FOR", "FIV", "SIX", "SVN", "EGT",
            ],
            trigger: Some(0x7F),
        },
        Step {
            what: "target 20h — the number the bitmap section calls stationary",
            target: 0x20,
            config: ARR3,
            fields: &[
                "STEP 6", "ONE", "TWO", "TRI", "FOR", "FIV", "SIX", "SVN", "EGT",
            ],
            trigger: Some(0x7F),
        },
        Step {
            what: "target 21h — the bitmap section's global temporary",
            target: 0x21,
            config: ARR3,
            fields: &[
                "STEP 7", "ONE", "TWO", "TRI", "FOR", "FIV", "SIX", "SVN", "EGT",
            ],
            trigger: Some(0x7F),
        },
        Step {
            what: "stationary 35h, arrangement 2 (Title/Name/Value), trigger 31",
            target: wire::TARGET_STATIONARY,
            config: 2,
            fields: &["STEP 8", "NAME", "VALUE"],
            trigger: Some(wire::ARRANGEMENT_TRIGGER),
        },
        Step {
            what: "per-control: encoder 1 (0Dh), arrangement 4 (Name/Value), trigger 31",
            target: 0x0D,
            config: 4,
            fields: &["STEP 9", "42%"],
            trigger: Some(wire::ARRANGEMENT_TRIGGER),
        },
    ]
}

/// Run the probe.
pub fn run(port: &Option<String>) -> Result<(), Box<dyn Error>> {
    let mut out = driver::open_output_port(port.as_deref())?;

    println!("entering DAW mode");
    send(&mut out, &wire::daw_mode(true), "enable DAW mode");
    send(
        &mut out,
        &wire::feature(wire::FEAT_MODE, wire::MODE_DAW_CONTROL),
        "mode = DAW Control",
    );
    send(
        &mut out,
        &wire::feature(wire::FEAT_SCREEN_BRIGHTNESS, 127),
        "screen brightness = max",
    );
    send(
        &mut out,
        &wire::feature(wire::FEAT_LED_BRIGHTNESS, 127),
        "LED brightness = max",
    );

    println!();
    println!("--- step 0: LEDs, to confirm SysEx is reaching the device at all ---");
    println!("encoder 1 should go red (RGB SysEx), encoder 2 green (palette CC).");
    println!("if neither does, the problem is the output port or DAW mode, not the screen.");
    send(
        &mut out,
        &wire::led_rgb(wire::ENCODER_FIRST, 127, 0, 0),
        "encoder 1 red, RGB SysEx",
    );
    send(
        &mut out,
        &wire::cc(wire::CH_BUTTONS, wire::ENCODER_FIRST + 1, 21),
        "encoder 2, palette CC (21 is green on most Novation palettes)",
    );
    pause();

    for (i, step) in steps().iter().enumerate() {
        println!();
        println!("--- step {}: {} ---", i + 1, step.what);
        send(
            &mut out,
            &wire::display_config(step.target, step.config),
            &format!(
                "configure target {:02X}h with config {:02X}h",
                step.target, step.config
            ),
        );
        for (field, text) in step.fields.iter().enumerate() {
            let (bytes, len) = wire::display_text(step.target, field as u8, text);
            send(
                &mut out,
                &bytes[..len],
                &format!("field {field} = {text:?}"),
            );
        }
        match step.trigger {
            Some(t) => send(
                &mut out,
                &wire::display_config(step.target, t),
                &format!("trigger with {t:02X}h"),
            ),
            None => println!("    (no trigger sent)"),
        }
        println!("look at the screen. does it show STEP {}?", i + 1);
        pause();
    }

    println!();
    println!("leaving DAW mode");
    send(&mut out, &wire::daw_mode(false), "disable DAW mode");
    println!();
    println!("the lowest-numbered step that showed its text is the answer.");
    println!("record it in docs/launch-control-xl3.md and on #13.");
    Ok(())
}

/// Send a message and print exactly what went out.
fn send(out: &mut MidiOutputConnection, bytes: &[u8], what: &str) {
    let hex: Vec<String> = bytes.iter().map(|b| format!("{b:02X}")).collect();
    match out.send(bytes) {
        Ok(()) => println!("    {:<52} {}", hex.join(" "), what),
        Err(e) => println!("    {:<52} {} — SEND FAILED: {e}", hex.join(" "), what),
    }
}

/// Wait for the user to look at the device.
fn pause() {
    print!("press enter for the next step (ctrl-c to stop)... ");
    let _ = std::io::stdout().flush();
    let mut line = String::new();
    let _ = std::io::stdin().lock().read_line(&mut line);
}
