//! `render device` — run the engine as a MIDI-in / audio-out device.
//!
//! The engine is a drum module, so the harness is just the firmware's main
//! loop relocated to macOS: MIDI bytes in from a virtual CoreMIDI port, audio
//! out through a 48 kHz device (BlackHole, your speakers, or an aggregate you
//! created in Audio MIDI Setup). Because both targets call the shared
//! [`drum_engine::midi::handle_midi`] router, a DAW or controller drives the
//! Mac rig and the Teensy identically — which is the point: tune here, flash
//! the same numbers to the device.
//!
//! Threading mirrors the hardware too: the CoreMIDI callback parses bytes and
//! pushes events into a lock-free ring; the audio callback drains the ring and
//! applies events *between* engine blocks, never inside [`DrumEngine::process`].

use std::error::Error;

use cpal::traits::{DeviceTrait, HostTrait};
use drum_engine::midi::{handle_midi, MidiEvent, MidiParser};
use drum_engine::{DrumEngine, BLOCK, SAMPLE_RATE};
use midir::os::unix::VirtualInput;
use midir::{Ignore, MidiInput};
use rtrb::{Producer, RingBuffer};

/// Ring capacity, in parsed MIDI events. Dense sequenced drum fills arrive at
/// far less than this per audio callback; the queue exists to decouple the
/// CoreMIDI thread from the audio thread, not to absorb real load.
const EVENT_QUEUE_CAP: usize = 1024;

/// Run the device until ctrl-c.
pub fn run(out: &Option<String>, port: &str, list: bool) -> Result<(), Box<dyn Error>> {
    use cpal::traits::{DeviceTrait, HostTrait, StreamTrait};

    let host = cpal::default_host();

    if list {
        for (i, dev) in host.output_devices()?.enumerate() {
            println!("{i:2}: {}", dev.name()?);
        }
        return Ok(());
    }

    let device = resolve_output(&host, out)?;
    let device_name = device.name()?;
    let config = pick_48k_config(&device)?;
    let channels = config.channels() as usize;

    // Engine and its MIDI queue are owned by the audio callback thread.
    let engine = DrumEngine::new();
    let (producer, consumer) = RingBuffer::<MidiEvent>::new(EVENT_QUEUE_CAP);

    // Virtual MIDI input: appears system-wide as an input port any app can
    // route to. The callback runs on a CoreMIDI thread; parse and queue.
    let mut midi_in = MidiInput::new("Drumkit Engine")?;
    midi_in.ignore(Ignore::None);
    let _midi_conn = midi_in.create_virtual(
        port,
        |_, bytes, data: &mut (MidiParser, Producer<MidiEvent>)| {
            let (parser, producer) = data;
            for &b in bytes {
                if let Some(ev) = parser.push(b) {
                    let _ = producer.push(ev);
                }
            }
        },
        (MidiParser::new(), producer),
    )?;

    println!("Drumkit Engine — listening for MIDI on '{port}'");
    println!(
        "audio out on '{device_name}' at {} Hz ({} ch)",
        config.sample_rate().0,
        channels
    );
    println!("one channel per track: MIDI channel N (0..7) drives track N");
    println!("notes are chromatic — any note plays that track, middle C (60) = default pitch, 1 semitone per note step");
    println!("CC 20..27 on channel N sets track N's 8 macros; CC 7 = master gain (any channel)");
    println!("ctrl-c to stop");

    let (err_tx, err_rx) = std::sync::mpsc::channel();

    // Per-block render state, kept across callbacks.
    let mut l = [0.0f32; BLOCK];
    let mut r = [0.0f32; BLOCK];
    let mut cursor = BLOCK; // force a render on the first frame
    let mut consumer = consumer;
    let mut engine = engine;

    let stream = device.build_output_stream(
        &config.into(),
        move |data: &mut [f32], _| {
            for frame in data.chunks_mut(channels) {
                if cursor >= BLOCK {
                    // Apply everything that arrived since the last block,
                    // then render one engine block. Same discipline as the
                    // firmware: sequence between blocks, never inside.
                    while let Ok(ev) = consumer.pop() {
                        handle_midi(&mut engine, ev);
                    }
                    engine.process(&mut l, &mut r);
                    cursor = 0;
                }

                let (sl, sr) = (l[cursor], r[cursor]);
                cursor += 1;

                // Stereo source into a multi-channel device: L on ch 0, R on
                // ch 1, silence everywhere else. Repeating the pair across
                // every channel would leak it into all of BlackHole's ports.
                for (i, out) in frame.iter_mut().enumerate() {
                    *out = match i {
                        0 => sl,
                        1 => sr,
                        _ => 0.0,
                    };
                }
            }
        },
        move |e| {
            let _ = err_tx.send(e);
        },
        None,
    )?;

    stream.play()?;

    loop {
        if let Ok(e) = err_rx.try_recv() {
            eprintln!("stream error: {e}");
            return Err("audio stream failed".into());
        }
        std::thread::sleep(std::time::Duration::from_millis(100));
    }
}

/// Pick the output device: substring match on `--out`, else BlackHole if
/// installed, else the system default.
fn resolve_output(host: &cpal::Host, out: &Option<String>) -> Result<cpal::Device, Box<dyn Error>> {
    let devices: Vec<cpal::Device> = host.output_devices()?.collect();
    let names: Vec<String> = devices
        .iter()
        .map(|d| d.name().unwrap_or_else(|_| "?".into()))
        .collect();

    if let Some(needle) = out {
        let needle = needle.to_lowercase();
        if let Some(dev) = devices.iter().find(|d| {
            d.name()
                .unwrap_or_default()
                .to_lowercase()
                .contains(&needle)
        }) {
            return Ok(dev.clone());
        }
        return Err(format!(
            "no output device matching '{needle}'. available: {}",
            names.join(", ")
        )
        .into());
    }

    if let Some(dev) = devices.iter().find(|d| {
        d.name()
            .unwrap_or_default()
            .to_lowercase()
            .contains("blackhole")
    }) {
        return Ok(dev.clone());
    }

    host.default_output_device()
        .ok_or_else(|| "no output device available".into())
}

/// The engine bakes `SAMPLE_RATE` into every decay/filter coefficient, so the
/// device must run at 48 kHz F32. Refuse anything else with a clear message —
/// a wrong-rate session would sound detuned and waste a tuning session.
fn pick_48k_config(device: &cpal::Device) -> Result<cpal::SupportedStreamConfig, Box<dyn Error>> {
    use cpal::SampleFormat;

    let target = cpal::SampleRate(SAMPLE_RATE as u32);

    for range in device.supported_output_configs()? {
        if range.min_sample_rate() <= target
            && target <= range.max_sample_rate()
            && range.sample_format() == SampleFormat::F32
        {
            return Ok(range.with_sample_rate(target));
        }
    }

    let default = device.default_output_config()?;
    if default.sample_rate().0 == SAMPLE_RATE as u32 && default.sample_format() == SampleFormat::F32
    {
        return Ok(default);
    }

    Err(format!(
        "the engine is built for {} Hz f32 — set the output device (or your aggregate \
         device in Audio MIDI Setup) to {} Hz and try again",
        SAMPLE_RATE, SAMPLE_RATE
    )
    .into())
}
