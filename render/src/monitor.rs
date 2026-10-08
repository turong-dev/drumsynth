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

use midir::{Ignore, MidiInput};

/// Run the monitor until ctrl-c.
pub fn run(
    port: &Option<String>,
    channel: Option<u8>,
    list: bool,
    hex: bool,
    realtime: bool,
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

    println!("\n{} message(s) seen", count.load(Ordering::Relaxed));
    Ok(())
}

/// Tiny stateful MIDI parser that prints human-readable channel messages.
struct Parser {
    status: u8,
    data: [u8; 2],
    index: usize,
    channel_filter: Option<u8>,
    show_hex: bool,
    show_realtime: bool,
    count: Arc<AtomicUsize>,
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

    fn decode(&self, channel: u8) -> Option<String> {
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
                let name = cc_name(cc);
                if name.is_empty() {
                    Some(format!("Ch {:2} CC {:3} = {}", ch, cc, self.data[1]))
                } else {
                    Some(format!(
                        "Ch {:2} CC {:3} {} = {}",
                        ch, cc, name, self.data[1]
                    ))
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
