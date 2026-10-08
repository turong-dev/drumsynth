//! Read the firmware's headroom report off its USB MIDI port.
//!
//! The firmware pushes three control changes once a second on a reserved
//! channel (see `firmware/src/runner.rs::report_slack`). This decodes them
//! into a live readout.
//!
//! # Why MIDI and not a log line
//!
//! The real firmware's USB stack carries the MIDI class and nothing else.
//! `imxrt-log`'s backend builds and owns an entire stack of its own and
//! cannot share the one bus the Teensy has, which is the reason
//! `firmware/src/usb.rs` exists at all; a CDC class is planned there and has
//! not landed. So the channel that already works is the one that gets used.
//!
//! # What the number means
//!
//! Cycles from a block boundary to that block's render completing, as a
//! fraction of the 400,000-cycle block period. Unlike `mi-bench`, which times
//! `engine.process()` with USB interrupts disabled, nothing on MIDI, no grid
//! and a warm cache, this is measured on the real binary and therefore
//! includes:
//!
//! - the SAI ISR preempting the render, roughly every 333 µs
//! - the main loop's USB and DIN MIDI polling, which happen *before* the
//!   render gets a turn and are polled rather than interrupt-driven
//! - the grid's LED feedback pass
//! - whatever all of that does to the engine's cache residency
//!
//! Those four are exactly what the informal `~70%` ceiling in `BENCHMARKS.md`
//! was a stand-in for, and none of them has ever been measured. This replaces
//! the stand-in.

use std::error::Error;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc;
use std::sync::Arc;
use std::time::Duration;

use midir::{Ignore, MidiInput, MidiOutput};

/// Must match `firmware/src/runner.rs`.
const SLACK_CHANNEL: u8 = 14;
const CC_SLACK_PCT: u8 = 20;
const CC_LATE_BLOCKS: u8 = 21;
const CC_UNDERRUNS: u8 = 22;
const CC_PROCESS_PCT: u8 = 23;
/// Asks the board to drop into HalfKay so the next flash needs no button.
const CC_MIDI_RX: u8 = 24;
const CC_REBOOT: u8 = 119;

/// Must match `firmware/src/runner.rs::CYCLES_PER_STEP`.
const CYCLES_PER_STEP: u32 = 8_192;
/// Must match `firmware/src/audio.rs::BUDGET_CYCLES`.
const BUDGET_CYCLES: u32 = (600_000_000 / 48_000) * 32;

/// Send the reboot command and return.
pub fn reboot(port_filter: Option<&str>) -> Result<(), Box<dyn Error>> {
    let out = MidiOutput::new("drumsynth reboot")?;
    let ports = out.ports();
    let wanted = port_filter.unwrap_or("drumkit");
    let chosen = ports
        .iter()
        .find(|p| {
            out.port_name(p)
                .map(|n| n.to_lowercase().contains(&wanted.to_lowercase()))
                .unwrap_or(false)
        })
        .or_else(|| ports.first())
        .ok_or("no MIDI output port")?
        .clone();
    let name = out.port_name(&chosen)?;
    let mut conn = out.connect(&chosen, "reboot")?;
    // Sent repeatedly, not once.
    //
    // A single CC in isolation was unreliable: note traffic from `--drive-hz`
    // always arrives (the firmware's `midi rx` counter climbs with it), but
    // one packet after an idle bus sometimes does not take, and on one
    // occasion it was delivered only when the device next re-enumerated —
    // which looks exactly like a board that reboots itself on startup for no
    // reason. Ten packets over a second costs nothing and removes the
    // ambiguity; if the board is still up afterwards, it genuinely did not
    // receive them.
    for _ in 0..10 {
        conn.send(&[0xB0 | SLACK_CHANNEL, CC_REBOOT, 127])?;
        std::thread::sleep(Duration::from_millis(100));
    }
    println!("sent reboot-to-HalfKay on '{name}' — the board should be ready to flash");
    Ok(())
}

/// One decoded report.
#[derive(Default, Clone, Copy)]
struct Report {
    pct: Option<u8>,
    proc_pct: Option<u8>,
    late: Option<u8>,
    underruns: Option<u8>,
    midi_rx: Option<u8>,
}

impl Report {
    /// A report is complete once all three control changes have arrived. They
    /// are sent back to back, but USB MIDI packs several into a transfer and
    /// nothing guarantees the host hands them over together.
    fn complete(&self) -> bool {
        self.pct.is_some()
            && self.proc_pct.is_some()
            && self.late.is_some()
            && self.underruns.is_some()
            && self.midi_rx.is_some()
    }
}

/// Hammer every track over MIDI while the report is read.
///
/// Worst case is not something you can wait for — it has to be produced. This
/// retriggers all six tracks at `hz`, which also exercises the firmware's USB
/// MIDI *receive* path: `usb::poll`, the parser and `schedule_midi` all run in
/// the same main loop as the render and come out of the same block period, so
/// driving the board is measuring the board.
///
/// Returns a handle that stops the driver when dropped.
fn drive(port_filter: Option<&str>, hz: f32) -> Result<DriveHandle, Box<dyn Error>> {
    let out = MidiOutput::new("drumsynth slack driver")?;
    let ports = out.ports();
    let wanted = port_filter.unwrap_or("drumkit");
    let chosen = ports
        .iter()
        .find(|p| {
            out.port_name(p)
                .map(|n| n.to_lowercase().contains(&wanted.to_lowercase()))
                .unwrap_or(false)
        })
        .or_else(|| ports.first())
        .ok_or("no MIDI output port to drive")?
        .clone();
    let name = out.port_name(&chosen)?;
    let mut conn = out.connect(&chosen, "drive")?;

    let stop = Arc::new(AtomicBool::new(false));
    let flag = stop.clone();
    // Counted, not discarded: a driver that silently fails to send looks
    // exactly like a firmware that silently fails to receive.
    let sent = Arc::new(core::sync::atomic::AtomicU32::new(0));
    let errs = Arc::new(core::sync::atomic::AtomicU32::new(0));
    let (sent_c, errs_c) = (sent.clone(), errs.clone());
    let period = Duration::from_secs_f32(1.0 / hz.max(1.0));
    let join = std::thread::spawn(move || {
        let (sent, errs) = (sent_c, errs_c);
        // One channel per track, which is the mapping `runner.rs` uses.
        const TRACKS: u8 = 6;
        while !flag.load(Ordering::Relaxed) {
            for ch in 0..TRACKS {
                // Note 60 is each track's default pitch.
                if conn.send(&[0x90 | ch, 60, 100]).is_err() {
                    errs.fetch_add(1, Ordering::Relaxed);
                } else {
                    sent.fetch_add(1, Ordering::Relaxed);
                }
            }
            // Hold for half the period, then release.
            //
            // Without the note-off the gates never close: the first version
            // of this sent note-ons only, every voice latched on, and the
            // engine stayed at 43% of budget indefinitely *after* the driver
            // stopped. That is not a measurement, it is a stuck instrument —
            // and it made idle and driven readings indistinguishable once the
            // board had been driven once.
            std::thread::sleep(period / 2);
            for ch in 0..TRACKS {
                let _ = conn.send(&[0x80 | ch, 60, 0]);
            }
            std::thread::sleep(period / 2);
        }
        // Leave nothing latched behind.
        for ch in 0..TRACKS {
            let _ = conn.send(&[0x80 | ch, 60, 0]);
        }
    });
    Ok(DriveHandle {
        stop,
        join: Some(join),
        name,
        sent,
        errs,
    })
}

struct DriveHandle {
    stop: Arc<AtomicBool>,
    join: Option<std::thread::JoinHandle<()>>,
    name: String,
    sent: Arc<core::sync::atomic::AtomicU32>,
    errs: Arc<core::sync::atomic::AtomicU32>,
}

impl Drop for DriveHandle {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Relaxed);
        if let Some(j) = self.join.take() {
            let _ = j.join();
        }
        println!(
            "driver: {} note-ons sent, {} send errors",
            self.sent.load(Ordering::Relaxed),
            self.errs.load(Ordering::Relaxed)
        );
    }
}

/// Listen until interrupted, printing each report as it arrives.
pub fn run(
    port_filter: Option<&str>,
    seconds: Option<u64>,
    drive_hz: Option<f32>,
) -> Result<(), Box<dyn Error>> {
    let mut midi_in = MidiInput::new("drumsynth slack")?;
    midi_in.ignore(Ignore::None);

    let ports = midi_in.ports();
    if ports.is_empty() {
        return Err("no MIDI input ports — is the Teensy plugged in and running?".into());
    }

    // Default to the first port whose name mentions the board, so the common
    // case needs no argument; `--port` overrides for a machine with several.
    let wanted = port_filter.unwrap_or("Teensy");
    let chosen = ports
        .iter()
        .find(|p| {
            midi_in
                .port_name(p)
                .map(|n| n.to_lowercase().contains(&wanted.to_lowercase()))
                .unwrap_or(false)
        })
        .or_else(|| {
            if port_filter.is_none() {
                ports.first()
            } else {
                None
            }
        })
        .ok_or_else(|| {
            let names: Vec<String> = ports
                .iter()
                .filter_map(|p| midi_in.port_name(p).ok())
                .collect();
            format!("no MIDI input port matching {wanted:?}. Available: {names:?}")
        })?
        .clone();

    let name = midi_in.port_name(&chosen)?;
    let (tx, rx) = mpsc::channel::<(u8, u8)>();

    let _conn = midi_in.connect(
        &chosen,
        "slack",
        move |_stamp, bytes, _| {
            // Control change on the reserved channel, nothing else.
            if bytes.len() == 3 && bytes[0] == 0xB0 | SLACK_CHANNEL {
                let _ = tx.send((bytes[1], bytes[2]));
            }
        },
        (),
    )?;

    println!(
        "listening on '{name}' (channel {}, CC {CC_SLACK_PCT}/{CC_LATE_BLOCKS}/{CC_UNDERRUNS})",
        SLACK_CHANNEL + 1
    );
    // Held for the duration; dropping it stops the driver thread.
    let _driver = match drive_hz {
        Some(hz) => {
            let h = drive(port_filter, hz)?;
            println!("driving all 6 tracks at {hz} Hz via '{}'", h.name);
            Some(h)
        }
        None => {
            println!("not driving — play the board, or pass --drive-hz to retrigger it from here");
            None
        }
    };
    println!("budget is 400,000 cycles per block; the informal ceiling is ~70%\n");
    println!(
        "  {:>11}  {:>11}  {:>9}  {:>9}",
        "boundary→done", "process only", "late blks", "midi rx"
    );
    println!("  {}", "-".repeat(62));

    let deadline = seconds.map(|s| std::time::Instant::now() + Duration::from_secs(s));
    let mut acc = Report::default();
    let mut worst_seen = 0u8;

    loop {
        if let Some(d) = deadline {
            if std::time::Instant::now() >= d {
                break;
            }
        }
        match rx.recv_timeout(Duration::from_millis(250)) {
            Ok((cc, value)) => {
                match cc {
                    CC_SLACK_PCT => acc.pct = Some(value),
                    CC_PROCESS_PCT => acc.proc_pct = Some(value),
                    CC_LATE_BLOCKS => acc.late = Some(value),
                    CC_UNDERRUNS => acc.underruns = Some(value),
                    CC_MIDI_RX => acc.midi_rx = Some(value),
                    _ => {}
                }
                if acc.complete() {
                    let pct = acc.pct.unwrap();
                    worst_seen = worst_seen.max(pct);
                    let cyc = |v: u8| v as u32 * CYCLES_PER_STEP;
                    let of = |v: u8| 100.0 * cyc(v) as f32 / BUDGET_CYCLES as f32;
                    // 127 is the top of the scale, not a reading.
                    let cap = |v: u8| if v >= 127 { " +" } else { "  " };
                    // Already per-window; the firmware resets on read.
                    let late_now = acc.late.unwrap();
                    let flag = if acc.underruns.unwrap() > 0 {
                        "  <-- UNDERRUNS"
                    } else if late_now > 0 {
                        "  <-- MISSED DEADLINE"
                    } else if of(pct) > 70.0 {
                        "  <-- over 70%"
                    } else {
                        ""
                    };
                    println!(
                        "  {:>9.1}%{}  {:>9.1}%{}  {:>9}  {:>9}  {:>7}{flag}",
                        of(pct),
                        cap(pct),
                        of(acc.proc_pct.unwrap()),
                        cap(acc.proc_pct.unwrap()),
                        late_now,
                        acc.underruns.unwrap(),
                        acc.midi_rx.unwrap()
                    );
                    acc = Report::default();
                }
            }
            Err(mpsc::RecvTimeoutError::Timeout) => {}
            Err(mpsc::RecvTimeoutError::Disconnected) => break,
        }
    }

    println!(
        "\nworst over the run: {:.1}% of budget{}",
        100.0 * (worst_seen as u32 * CYCLES_PER_STEP) as f32 / BUDGET_CYCLES as f32,
        if worst_seen >= 127 {
            " (scale capped — the real figure is higher)"
        } else {
            ""
        }
    );
    Ok(())
}
