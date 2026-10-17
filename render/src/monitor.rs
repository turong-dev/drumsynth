//! `render monitor` — a compact MIDI input monitor.
//!
//! Opens a MIDI input port and prints every channel message that arrives,
//! with an optional channel filter. Useful for verifying what a controller,
//! DAW, or groove box is actually sending before the bytes reach the engine.
//!
//! Because it shares no code with the audio path, it is safe to leave running
//! while debugging the firmware or host `device` harness.

use std::error::Error;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::Arc;

use drum_engine::machines::ParamGroup;
use drum_engine::midi::{
    CC_DATA_DECREMENT, CC_DATA_ENTRY_LSB, CC_DATA_ENTRY_MSB, CC_DATA_INCREMENT, CC_NRPN_PARAM_LSB,
    CC_NRPN_PARAM_MSB, CC_RPN_PARAM_LSB, CC_RPN_PARAM_MSB, NRPN_MAX,
};
use midir::{Ignore, MidiInput};

use crate::lcxl3::{driver, wire};

/// Run the monitor until ctrl-c.
///
/// With `daw` set, a Launch Control XL 3 is put into DAW mode first and
/// taken back out at the end. That is phase 0 of #13: the programmer's
/// reference is v1.0, the relative-encoder encoding is described loosely
/// across Novation's sources, and the control-index map is a rendered
/// diagram rather than text — so what the surface actually sends has to be
/// captured before a driver is written against it. See
/// `docs/launch-control-xl3.md` for what to capture and what each capture
/// decides.
pub fn run(
    port: &Option<String>,
    channel: Option<u8>,
    list: bool,
    hex: bool,
    realtime: bool,
    daw: bool,
) -> Result<(), Box<dyn Error>> {
    let mut midi_in = MidiInput::new("Drumkit MIDI Monitor")?;
    midi_in.ignore(Ignore::None);

    let ports = midi_in.ports();
    let port_names: Vec<String> = ports
        .iter()
        .map(|p| midi_in.port_name(p).unwrap_or_else(|_| "?".into()))
        .collect();

    if list {
        println!("MIDI input ports:");
        for (i, name) in port_names.iter().enumerate() {
            println!("  {i:2}: {name}");
        }
        return Ok(());
    }

    let in_port = match port {
        Some(needle) => {
            let needle_lower = needle.to_lowercase();
            let matches: Vec<usize> = port_names
                .iter()
                .enumerate()
                .filter(|(_, n)| n.to_lowercase().contains(&needle_lower))
                .map(|(i, _)| i)
                .collect();
            match matches.len() {
                0 => {
                    return Err(format!(
                        "no MIDI input port matching '{needle}'. available: {}",
                        port_names.join(", ")
                    )
                    .into());
                }
                1 => &ports[matches[0]],
                _ => {
                    println!("multiple ports match '{needle}':");
                    for i in matches {
                        println!("  {i:2}: {}", port_names[i]);
                    }
                    return Err("specify a more precise --port name".into());
                }
            }
        }
        None => match ports.len() {
            0 => return Err("no MIDI input ports available".into()),
            1 => {
                println!("auto-selected: {}", port_names[0]);
                &ports[0]
            }
            _ => {
                println!("available MIDI input ports (use --port <name-or-index>):");
                for (i, name) in port_names.iter().enumerate() {
                    println!("  {i:2}: {name}");
                }
                return Err("multiple ports available; pick one with --port".into());
            }
        },
    };

    let port_name = midi_in.port_name(in_port)?;
    let filter = channel.map(|c| c.saturating_sub(1).min(15));

    if let Some(ch) = channel {
        println!("Monitoring '{port_name}' — channel {ch} only");
    } else {
        println!("Monitoring '{port_name}' — all channels");
    }
    println!("press ctrl-c to stop\n");

    // Hold the output connection for the whole session: dropping it would
    // close the port and the device would keep whatever mode it was left
    // in. `_daw_out` is also what takes it back out of DAW mode at the end.
    let mut daw_out = if daw {
        Some(enter_daw_mode(port.as_deref())?)
    } else {
        None
    };

    let running = Arc::new(AtomicBool::new(true));
    let r = running.clone();

    ctrlc::set_handler(move || {
        r.store(false, Ordering::Relaxed);
    })
    .ok();

    let count = Arc::new(AtomicUsize::new(0));
    let mut parser = Parser::new(filter, hex, realtime, count.clone());

    let _conn = midi_in.connect(
        in_port,
        "drumsynth-monitor",
        move |stamp_us, bytes, _| {
            parser.feed(stamp_us, bytes);
        },
        (),
    )?;

    while running.load(Ordering::Relaxed) {
        std::thread::sleep(std::time::Duration::from_millis(50));
    }

    if let Some(out) = daw_out.as_mut() {
        let _ = out.send(&wire::daw_mode(false));
        println!("left DAW mode");
    }

    println!("\n{} message(s) seen", count.load(Ordering::Relaxed));
    Ok(())
}

/// Put a Launch Control XL 3 into DAW mode with everything #13 needs
/// reporting, and return the connection so the caller can undo it.
///
/// The order matters: DAW mode first, because the relative-row and touch
/// feature controls are only fully listening once it is on.
fn enter_daw_mode(port: Option<&str>) -> Result<midir::MidiOutputConnection, Box<dyn Error>> {
    let mut out = driver::open_output_port(port)?;
    out.send(&wire::daw_mode(true))?;
    out.send(&wire::feature(wire::FEAT_MODE, wire::MODE_DAW_CONTROL))?;
    for row in [
        wire::FEAT_RELATIVE_ROW1,
        wire::FEAT_RELATIVE_ROW2,
        wire::FEAT_RELATIVE_ROW3,
    ] {
        out.send(&wire::feature(row, 0x7F))?;
    }
    out.send(&wire::feature(wire::FEAT_TOUCH, 0x7F))?;
    out.send(&wire::feature(wire::FEAT_FADER_PICKUP, 0x7F))?;
    println!("DAW mode on: relative encoder rows 1-3, touch events, fader pickup");
    println!("turn an encoder slowly, then fast — the data byte is what phase 0 needs");
    Ok(out)
}

/// Tiny stateful MIDI parser that prints human-readable channel messages.
///
/// It tracks the NRPN selection per channel for the same reason the engine's
/// parser does: an NRPN transaction spans four messages and the wire carries
/// no end marker, so `CC 6 = 100` means nothing without the CC 99/98 pair
/// that came before it. Printing the raw CCs and leaving the reader to
/// reassemble them is exactly the job the monitor exists to do for you.
struct Parser {
    status: u8,
    data: [u8; 2],
    index: usize,
    channel_filter: Option<u8>,
    show_hex: bool,
    show_realtime: bool,
    count: Arc<AtomicUsize>,
    /// Per-channel NRPN selection, `(group, index)`, or `None` when no
    /// parameter is armed on that channel.
    nrpn_sel: [Option<(u8, u8)>; 16],
    /// Per-channel last data-entry MSB, so a CC 38 can be shown as the
    /// 14-bit value the receiver will actually apply.
    nrpn_msb: [u8; 16],
}

impl Parser {
    fn new(
        channel_filter: Option<u8>,
        show_hex: bool,
        show_realtime: bool,
        count: Arc<AtomicUsize>,
    ) -> Self {
        Self {
            status: 0,
            data: [0; 2],
            index: 0,
            channel_filter,
            show_hex,
            show_realtime,
            count,
            nrpn_sel: [None; 16],
            nrpn_msb: [0; 16],
        }
    }

    fn feed(&mut self, stamp_us: u64, bytes: &[u8]) {
        for &b in bytes {
            self.push_byte(stamp_us, b);
        }
    }

    fn push_byte(&mut self, stamp_us: u64, byte: u8) {
        // System real-time: single-byte, does not affect running status.
        if byte >= 0xF8 {
            if self.show_realtime {
                self.print_realtime(stamp_us, byte);
            }
            return;
        }

        // System common / exclusive: cancels running status.
        if byte >= 0xF0 {
            self.status = 0;
            self.index = 0;
            if byte == 0xF0 && self.show_hex {
                println!("{:10} SysEx start", fmt_time(stamp_us));
            } else if byte == 0xF7 && self.show_hex {
                println!("{:10} SysEx end", fmt_time(stamp_us));
            }
            return;
        }

        // Status byte.
        if byte >= 0x80 {
            self.status = byte;
            self.index = 0;
            return;
        }

        // Data byte with no active status — ignore.
        if self.status == 0 {
            return;
        }

        self.data[self.index] = byte;
        self.index += 1;

        let expected = match self.status & 0xF0 {
            0xC0 | 0xD0 => 1,
            _ => 2,
        };

        if self.index < expected {
            return;
        }
        self.index = 0;

        let channel = self.status & 0x0F;
        if let Some(want) = self.channel_filter {
            if channel != want {
                return;
            }
        }

        if let Some(line) = self.decode(channel) {
            self.count.fetch_add(1, Ordering::Relaxed);
            if self.show_hex {
                let hex = if expected == 1 {
                    format!("{:02X} {:02X}", self.status, self.data[0])
                } else {
                    format!(
                        "{:02X} {:02X} {:02X}",
                        self.status, self.data[0], self.data[1]
                    )
                };
                println!("{:10} {}  {}", fmt_time(stamp_us), hex, line);
            } else {
                println!("{:10} {}", fmt_time(stamp_us), line);
            }
        }
    }

    fn decode(&mut self, channel: u8) -> Option<String> {
        let ch = channel + 1;
        match self.status & 0xF0 {
            0x80 => Some(format!(
                "Ch {:2} NoteOff {} ({}) vel {}",
                ch,
                note_name(self.data[0]),
                self.data[0],
                self.data[1]
            )),
            0x90 => {
                if self.data[1] == 0 {
                    Some(format!(
                        "Ch {:2} NoteOff {} ({}) (zero-vel)",
                        ch,
                        note_name(self.data[0]),
                        self.data[0]
                    ))
                } else {
                    Some(format!(
                        "Ch {:2} NoteOn  {} ({}) vel {}",
                        ch,
                        note_name(self.data[0]),
                        self.data[0],
                        self.data[1]
                    ))
                }
            }
            0xA0 => Some(format!(
                "Ch {:2} PolyPressure {} ({}) val {}",
                ch,
                note_name(self.data[0]),
                self.data[0],
                self.data[1]
            )),
            0xB0 => {
                let cc = self.data[0];
                let value = self.data[1];
                if let Some(line) = self.decode_nrpn(channel, cc, value) {
                    return Some(line);
                }
                let name = cc_name(cc);
                if name.is_empty() {
                    Some(format!("Ch {ch:2} CC {cc:3} = {value}"))
                } else {
                    Some(format!("Ch {ch:2} CC {cc:3} {name} = {value}"))
                }
            }
            0xC0 => Some(format!("Ch {:2} ProgramChange {}", ch, self.data[0])),
            0xD0 => Some(format!("Ch {:2} ChannelPressure {}", ch, self.data[0])),
            0xE0 => {
                let value = ((self.data[1] as u16) << 7 | self.data[0] as u16) - 8192;
                Some(format!("Ch {:2} PitchBend {}", ch, value))
            }
            _ => None,
        }
    }

    /// Decode one of the eight NRPN/RPN/data-entry controls, or `None` if
    /// `cc` is an ordinary controller.
    ///
    /// The selection messages print as themselves — you want to see a
    /// controller arming the wrong address — and the data messages print
    /// the address they land on, so a line reads the way the device reads
    /// it: `Ch  1 NRPN (VOICE, 0) = 10273  0.627`.
    fn decode_nrpn(&mut self, channel: u8, cc: u8, value: u8) -> Option<String> {
        let ch = channel + 1;
        let i = channel as usize;
        match cc {
            CC_NRPN_PARAM_MSB => {
                let (_, index) = self.nrpn_sel[i].unwrap_or((value, 0));
                self.nrpn_sel[i] = Some((value, index));
                Some(format!(
                    "Ch {ch:2} NRPN select group {value} ({})",
                    group_name(value)
                ))
            }
            CC_NRPN_PARAM_LSB => {
                // 98-before-99 is legal, so an index can arrive with no
                // group yet. Hold it; the group completes the address.
                let (group, _) = self.nrpn_sel[i].unwrap_or((0, value));
                self.nrpn_sel[i] = Some((group, value));
                Some(format!("Ch {ch:2} NRPN select index {value}"))
            }
            CC_RPN_PARAM_MSB | CC_RPN_PARAM_LSB => {
                // An RPN select takes the data-entry pair back; the device
                // drops its NRPN selection, so the monitor must too or the
                // next data message is attributed to a stale address.
                self.nrpn_sel[i] = None;
                Some(format!("Ch {ch:2} RPN select {value} (clears NRPN)"))
            }
            CC_DATA_ENTRY_MSB => {
                self.nrpn_msb[i] = value;
                let raw = ((value as u16) << 7) | value as u16;
                Some(self.nrpn_line(channel, "=", &format!("{raw:5}  {:.4}", normalised(raw))))
            }
            CC_DATA_ENTRY_LSB => {
                let raw = ((self.nrpn_msb[i] as u16) << 7) | value as u16;
                Some(self.nrpn_line(channel, "=", &format!("{raw:5}  {:.4}", normalised(raw))))
            }
            CC_DATA_INCREMENT | CC_DATA_DECREMENT => {
                let sign = if cc == CC_DATA_INCREMENT { '+' } else { '-' };
                let magnitude = value.max(1);
                Some(self.nrpn_line(channel, "", &format!("{sign}{magnitude} LSB")))
            }
            _ => None,
        }
    }

    /// One data-message line, with the channel's armed address in it.
    fn nrpn_line(&self, channel: u8, op: &str, body: &str) -> String {
        let ch = channel + 1;
        match self.nrpn_sel[channel as usize] {
            Some((group, index)) => {
                let sep = if op.is_empty() { "" } else { " " };
                format!(
                    "Ch {ch:2} NRPN ({}, {index}){sep}{op} {body}",
                    group_name(group)
                )
            }
            // No selection armed: the device ignores this message, and
            // saying so is more useful than printing a value nothing takes.
            None => format!("Ch {ch:2} NRPN data {body} — no parameter selected, ignored"),
        }
    }

    fn print_realtime(&mut self, stamp_us: u64, byte: u8) {
        let label = match byte {
            0xF8 => "Clock",
            0xFA => "Start",
            0xFB => "Continue",
            0xFC => "Stop",
            0xFE => "ActiveSense",
            0xFF => "Reset",
            _ => "RealTime",
        };
        println!("{:10} {}", fmt_time(stamp_us), label);
    }
}

/// A group's display name, or the raw number when this build has no
/// identifier for it — a controller addressing group 9 is sending to a
/// parameter that cannot exist, and the monitor should show that rather
/// than guess.
fn group_name(group: u8) -> String {
    match ParamGroup::from_u8(group) {
        Some(g) => g.name().to_string(),
        None => format!("group {group}?"),
    }
}

/// A raw 14-bit data value as the engine normalises it.
fn normalised(raw: u16) -> f32 {
    raw.min(NRPN_MAX) as f32 / NRPN_MAX as f32
}

fn fmt_time(stamp_us: u64) -> String {
    let secs = stamp_us / 1_000_000;
    let ms = (stamp_us % 1_000_000) / 1000;
    format!("{secs}.{ms:03}")
}

fn note_name(note: u8) -> String {
    let names = [
        "C", "C#", "D", "D#", "E", "F", "F#", "G", "G#", "A", "A#", "B",
    ];
    let n = note as usize % 12;
    let oct = (note as i16 / 12) - 1; // MIDI note 60 = C4 (middle C)
    format!("{}{}", names[n], oct)
}

fn cc_name(cc: u8) -> &'static str {
    match cc {
        0 => "BankSelect",
        1 => "ModWheel",
        2 => "Breath",
        4 => "Foot",
        5 => "PortamentoTime",
        6 => "DataEntryMSB",
        7 => "Volume",
        8 => "Balance",
        10 => "Pan",
        11 => "Expression",
        12 => "Effect1",
        13 => "Effect2",
        16 => "GenPurpose1",
        17 => "GenPurpose2",
        18 => "GenPurpose3",
        19 => "GenPurpose4",
        64 => "Sustain",
        65 => "Portamento",
        66 => "Sostenuto",
        67 => "SoftPedal",
        68 => "Legato",
        69 => "Hold2",
        120 => "AllSoundOff",
        123 => "AllNotesOff",
        _ if (32..=63).contains(&cc) => "LSB",
        _ if (64..=95).contains(&cc) => "Switch/Mode",
        _ if (96..=101).contains(&cc) => "Data/Control",
        _ if (102..=119).contains(&cc) => "Undefined",
        _ => "",
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::AtomicUsize;

    fn feed(parser: &mut Parser, bytes: &[u8]) {
        parser.feed(0, bytes);
    }

    #[test]
    fn parses_note_on() {
        let c = Arc::new(AtomicUsize::new(0));
        let mut p = Parser::new(None, false, false, c);
        feed(&mut p, &[0x90, 60, 100]);
        assert_eq!(p.count.load(Ordering::Relaxed), 1);
    }

    #[test]
    fn zero_velocity_note_on_is_note_off() {
        let c = Arc::new(AtomicUsize::new(0));
        let mut p = Parser::new(None, false, false, c);
        feed(&mut p, &[0x90, 60, 0]);
        assert_eq!(p.count.load(Ordering::Relaxed), 1);
    }

    #[test]
    fn explicit_note_off_is_counted() {
        let c = Arc::new(AtomicUsize::new(0));
        let mut p = Parser::new(None, false, false, c);
        feed(&mut p, &[0x80, 36, 64]);
        assert_eq!(p.count.load(Ordering::Relaxed), 1);
    }

    #[test]
    fn channel_filter_rejects_other_channels() {
        let c = Arc::new(AtomicUsize::new(0));
        // Filter to channel 2 (wire value 1).
        let mut p = Parser::new(Some(1), false, false, c.clone());
        feed(&mut p, &[0x90, 60, 100]); // channel 1
        assert_eq!(p.count.load(Ordering::Relaxed), 0);
        feed(&mut p, &[0x91, 60, 100]); // channel 2
        assert_eq!(p.count.load(Ordering::Relaxed), 1);
    }

    #[test]
    fn running_status_is_parsed() {
        let c = Arc::new(AtomicUsize::new(0));
        let mut p = Parser::new(None, false, false, c.clone());
        feed(&mut p, &[0x90, 60, 100, 62, 80]);
        assert_eq!(p.count.load(Ordering::Relaxed), 2);
    }

    /// Collect the decoded lines rather than the count, so the assertions
    /// can be about what the monitor actually prints.
    fn lines(bytes: &[u8]) -> Vec<String> {
        let c = Arc::new(AtomicUsize::new(0));
        let mut p = Parser::new(None, false, false, c);
        let mut out = Vec::new();
        for &b in bytes {
            // Mirror push_byte's framing without its printing.
            if b >= 0x80 {
                p.status = b;
                p.index = 0;
                continue;
            }
            p.data[p.index] = b;
            p.index += 1;
            if p.index < 2 {
                continue;
            }
            p.index = 0;
            let channel = p.status & 0x0F;
            if let Some(line) = p.decode(channel) {
                out.push(line);
            }
        }
        out
    }

    #[test]
    fn nrpn_data_entry_is_decoded_against_its_address() {
        let out = lines(&[
            0xB0,
            CC_NRPN_PARAM_MSB,
            1, // group VOICE
            CC_NRPN_PARAM_LSB,
            4, // index 4
            CC_DATA_ENTRY_MSB,
            80, //
            CC_DATA_ENTRY_LSB,
            33,
        ]);
        assert_eq!(out.len(), 4);
        assert!(out[0].contains("select group 1 (VOICE)"), "{}", out[0]);
        assert!(out[1].contains("select index 4"), "{}", out[1]);
        // MSB alone: 80<<7 | 80 = 10320, which normalises to 80/127.
        assert!(out[2].contains("NRPN (VOICE, 4) = 10320"), "{}", out[2]);
        // The LSB recomputes the pair: 80<<7 | 33 = 10273.
        assert!(out[3].contains("NRPN (VOICE, 4) = 10273"), "{}", out[3]);
        assert!(out[3].contains("0.627"), "{}", out[3]);
    }

    #[test]
    fn nrpn_select_order_does_not_matter() {
        // 98 before 99 is legal and must name the same address.
        let out = lines(&[
            0xB0,
            CC_NRPN_PARAM_LSB,
            2,
            CC_NRPN_PARAM_MSB,
            3,
            CC_DATA_ENTRY_MSB,
            0,
        ]);
        assert!(out[2].contains("NRPN (SHAPER, 2)"), "{}", out[2]);
    }

    #[test]
    fn nrpn_steps_print_as_signed_lsbs() {
        let out = lines(&[
            0xB0,
            CC_NRPN_PARAM_MSB,
            2,
            CC_NRPN_PARAM_LSB,
            0,
            CC_DATA_INCREMENT,
            12,
            CC_DATA_DECREMENT,
            0,
        ]);
        assert!(out[2].contains("NRPN (FILTER, 0) +12 LSB"), "{}", out[2]);
        // A zero data byte still means one step, which is what the engine
        // applies — printing "+0" would misreport it.
        assert!(out[3].contains("NRPN (FILTER, 0) -1 LSB"), "{}", out[3]);
    }

    #[test]
    fn data_entry_with_no_selection_says_so() {
        let out = lines(&[0xB0, CC_DATA_ENTRY_MSB, 64]);
        assert!(out[0].contains("no parameter selected"), "{}", out[0]);
    }

    #[test]
    fn an_rpn_select_clears_the_nrpn_address() {
        let out = lines(&[
            0xB0,
            CC_NRPN_PARAM_MSB,
            0,
            CC_NRPN_PARAM_LSB,
            2,
            CC_RPN_PARAM_MSB,
            0,
            CC_DATA_ENTRY_MSB,
            64,
        ]);
        assert!(out[2].contains("clears NRPN"), "{}", out[2]);
        assert!(
            out[3].contains("no parameter selected"),
            "an RPN select must take the data pair back: {}",
            out[3]
        );
    }

    #[test]
    fn an_unknown_group_is_shown_as_unknown() {
        let out = lines(&[
            0xB0,
            CC_NRPN_PARAM_MSB,
            9,
            CC_NRPN_PARAM_LSB,
            0,
            CC_DATA_ENTRY_MSB,
            1,
        ]);
        assert!(out[0].contains("group 9?"), "{}", out[0]);
        assert!(out[2].contains("(group 9?, 0)"), "{}", out[2]);
    }

    #[test]
    fn nrpn_state_is_per_channel() {
        let out = lines(&[
            0xB0,
            CC_NRPN_PARAM_MSB,
            1,
            CC_NRPN_PARAM_LSB,
            0, // ch 1: VOICE 0
            0xB1,
            CC_NRPN_PARAM_MSB,
            5,
            CC_NRPN_PARAM_LSB,
            1, // ch 2: LFO 1
            0xB0,
            CC_DATA_ENTRY_MSB,
            10, //
            0xB1,
            CC_DATA_ENTRY_MSB,
            20,
        ]);
        assert!(out[4].contains("Ch  1 NRPN (VOICE, 0)"), "{}", out[4]);
        assert!(out[5].contains("Ch  2 NRPN (LFO, 1)"), "{}", out[5]);
    }

    #[test]
    fn cc_name_lookup_works() {
        assert_eq!(cc_name(1), "ModWheel");
        assert_eq!(cc_name(7), "Volume");
        assert_eq!(cc_name(10), "Pan");
    }

    #[test]
    fn note_names_are_correct() {
        assert_eq!(note_name(60), "C4");
        assert_eq!(note_name(36), "C2");
        assert_eq!(note_name(69), "A4");
    }
}
