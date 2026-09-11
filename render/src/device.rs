//! `render device` — run a device engine as a MIDI-in / audio-out host harness.
//!
//! The harness is the firmware's main loop relocated to macOS: MIDI bytes in
//! from a virtual CoreMIDI port, audio out through a 48 kHz device (BlackHole,
//! your speakers, or an aggregate you created in Audio MIDI Setup). Because
//! both targets call the shared [`drum_engine::midi::handle_midi`] router, a
//! DAW or controller drives the Mac rig and the Teensy identically — which is
//! the point: tune here, flash the same numbers to the device.
//!
//! Threading mirrors the hardware too: the CoreMIDI callback parses bytes and
//! pushes events into a lock-free ring; the audio callback drains the ring and
//! applies events *between* engine blocks, never inside
//! [`DeviceEngine::process`](drum_engine::engine::DeviceEngine::process).
//!
//! # `--multi-out`
//!
//! The engine has four stereo output pairs: a master mix (pair 0) plus three
//! stereo auxes (pairs 1..3). Each track routes to exactly one pair via
//! [`drum_engine::OutPair`] on its strip; the default is `Master` (pair 0).
//!
//! Stereo mode (the default) sums everything to channels 0/1 — the master
//! mix only, with the wet FX return mixed in by the engine's `process`
//! wrapper.
//!
//! Multi-out mode writes the engine's
//! [`process_dry_wet`](drum_engine::engine::DeviceEngine::process_dry_wet)
//! primitive straight to an 8-channel output: pair 0 = master (with wet
//! mixed in), pairs 1..3 = auxes. A DAW sees 8 channels: 2 master + 6
//! individual. Matches the firmware's future 8-output TDM/multi-DAC
//! routing one-for-one — the same primitive, the same 8-channel frame.

use std::error::Error;

use cpal::traits::{DeviceTrait, HostTrait};
use drum_engine::midi::{handle_midi, MidiEvent, MidiParser};
use drum_engine::{DeviceEngine, BLOCK, SAMPLE_RATE};
use midir::os::unix::VirtualInput;
use midir::{Ignore, MidiInput};
use rtrb::{Producer, RingBuffer};

/// Ring capacity, in parsed MIDI events. Dense sequenced drum fills arrive at
/// far less than this per audio callback; the queue exists to decouple the
/// CoreMIDI thread from the audio thread, not to absorb real load.
const EVENT_QUEUE_CAP: usize = 1024;

/// Multi-out channel count: master stereo pair + three stereo aux pairs.
/// Matches the engine's four-pair routing exactly and the firmware's
/// 8-output limit.
const MULTI_OUT_CHANNELS: usize = 8;

/// Run the device until ctrl-c.
///
/// The engine is passed as a `Box<E>` so large devices (e.g. `MiDrumEngine`,
/// whose Plaits voices are hundreds of kilobytes) can be allocated in-place on
/// the heap and moved into the audio callback without ever putting the whole
/// struct on the caller's stack.
pub fn run<E, const N: usize>(
    engine: Box<E>,
    out: &Option<String>,
    port: &str,
    list: bool,
    multi_out: bool,
) -> Result<(), Box<dyn Error>>
where
    E: DeviceEngine<N> + Send + 'static,
{
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
    let config = pick_48k_config(&device, multi_out)?;
    let channels = config.channels() as usize;

    if multi_out && channels < MULTI_OUT_CHANNELS {
        return Err(format!(
            "device '{device_name}' offers {channels} channels but --multi-out needs \
             {MULTI_OUT_CHANNELS} (1 master stereo pair + 3 stereo aux pairs). Create an \
             aggregate device in Audio MIDI Setup that offers at least 8 channels at 48 kHz \
             and retry."
        )
        .into());
    }

    // Engine and its MIDI queue are owned by the audio callback thread.
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
        "audio out on '{device_name}' at {} Hz ({} ch{})",
        config.sample_rate().0,
        channels,
        if multi_out {
            ", multi-out: ch 0/1 master, 2/3 aux1, 4/5 aux2, 6/7 aux3"
        } else {
            ", stereo sum on ch 0/1"
        }
    );
    println!("one channel per track: MIDI channel N (0..7) drives track N");
    println!("notes are chromatic — any note plays that track, middle C (60) = default pitch, 1 semitone per note step");
    println!("CC 20..27 on channel N sets track N's 8 macros; CC 7 = master gain (any channel)");
    println!("per-track routing: strip.out = Master | Aux1 | Aux2 | Aux3");
    println!("ctrl-c to stop");

    let (err_tx, err_rx) = std::sync::mpsc::channel();

    // Per-block render state, kept across callbacks. The multi-out buses
    // are only read when `multi_out` is true, but allocating them
    // unconditionally (2 KB total) avoids a conditional type the closure
    // can't express cleanly.
    let mut l = [0.0f32; BLOCK];
    let mut r = [0.0f32; BLOCK];
    let mut master_l = [0.0f32; BLOCK];
    let mut master_r = [0.0f32; BLOCK];
    let mut aux = [[0.0f32; BLOCK]; 6];
    let mut wet_l = [0.0f32; BLOCK];
    let mut wet_r = [0.0f32; BLOCK];
    let mut cursor = BLOCK; // force a render on the first frame
    let mut consumer = consumer;
    let mut engine = engine;
    let master_gain = engine.master_gain();
    let fx_drive = engine.fx_drive();

    let stream = device.build_output_stream(
        &config.into(),
        move |data: &mut [f32], _| {
            for frame in data.chunks_mut(channels) {
                if cursor >= BLOCK {
                    // Apply everything that arrived since the last block,
                    // then render one engine block. Same discipline as the
                    // firmware: sequence between blocks, never inside.
                    while let Ok(ev) = consumer.pop() {
                        handle_midi(&mut *engine, ev);
                    }
                    if multi_out {
                        engine.process_dry_wet(
                            &mut master_l,
                            &mut master_r,
                            &mut aux,
                            &mut wet_l,
                            &mut wet_r,
                        );
                        // Apply the canonical master chain (FX-bus drive +
                        // inner clip on wet, sum with dry master, master
                        // gain + final clip) to channels 0/1 — same chain
                        // the stereo `process` wrapper applies.
                        for i in 0..BLOCK {
                            let wet_l_clipped =
                                drum_engine::dsp::fast::soft_clip(wet_l[i] * fx_drive);
                            let wet_r_clipped =
                                drum_engine::dsp::fast::soft_clip(wet_r[i] * fx_drive);
                            master_l[i] = drum_engine::dsp::fast::soft_clip(
                                (master_l[i] + wet_l_clipped) * master_gain,
                            );
                            master_r[i] = drum_engine::dsp::fast::soft_clip(
                                (master_r[i] + wet_r_clipped) * master_gain,
                            );
                        }
                    } else {
                        engine.process(&mut l, &mut r);
                    }
                    cursor = 0;
                }

                if multi_out {
                    // Pair 0: master (post-wet, post-gain, post-clip).
                    if !frame.is_empty() {
                        frame[0] = master_l[cursor];
                    }
                    if frame.len() >= 2 {
                        frame[1] = master_r[cursor];
                    }
                    // Pairs 1..3: aux dry (pre-everything — DAW handles
                    // drive/gain/clip per its own channel strip).
                    for p in 0..3usize {
                        let li = 2 + 2 * p;
                        let ri = 3 + 2 * p;
                        if li < frame.len() {
                            frame[li] = aux[2 * p][cursor];
                        }
                        if ri < frame.len() {
                            frame[ri] = aux[2 * p + 1][cursor];
                        }
                    }
                    // Silence anything past 8 channels (e.g. a 16ch
                    // aggregate) so nothing leaks onto extra outs.
                    let tail_start = MULTI_OUT_CHANNELS.min(frame.len());
                    for out in &mut frame[tail_start..] {
                        *out = 0.0;
                    }
                } else {
                    let (sl, sr) = (l[cursor], r[cursor]);
                    // Stereo source into a multi-channel device: L on ch 0,
                    // R on ch 1, silence everywhere else. Repeating the
                    // pair across every channel would leak it into all of
                    // BlackHole's ports.
                    for (i, out) in frame.iter_mut().enumerate() {
                        *out = match i {
                            0 => sl,
                            1 => sr,
                            _ => 0.0,
                        };
                    }
                }

                cursor += 1;
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
///
/// `multi_out` asks for at least [`MULTI_OUT_CHANNELS`] (8: 1 master stereo
/// pair + 3 stereo aux pairs). When unset, any channel count is accepted —
/// the stereo sum is written to channels 0/1 and the rest are zeroed.
fn pick_48k_config(
    device: &cpal::Device,
    multi_out: bool,
) -> Result<cpal::SupportedStreamConfig, Box<dyn Error>> {
    use cpal::SampleFormat;

    let target = cpal::SampleRate(SAMPLE_RATE as u32);
    let min_channels = if multi_out { MULTI_OUT_CHANNELS } else { 2 };

    let mut fallback: Option<cpal::SupportedStreamConfig> = None;

    for range in device.supported_output_configs()? {
        if range.min_sample_rate() > target || target > range.max_sample_rate() {
            continue;
        }
        if range.sample_format() != SampleFormat::F32 {
            continue;
        }
        let cfg = range.with_sample_rate(target);
        let ch = cfg.channels() as usize;
        if ch >= min_channels {
            return Ok(cfg);
        }
        // Track the best fallback for the error message — prefer the
        // largest channel count at 48 kHz so the user sees what's
        // available.
        if let Some(prev) = &fallback {
            if ch > prev.channels() as usize {
                fallback = Some(cfg);
            }
        } else {
            fallback = Some(cfg);
        }
    }

    if let Some(cfg) = fallback {
        if cfg.channels() as usize >= min_channels {
            return Ok(cfg);
        }
    }

    let suffix = if multi_out {
        format!(" and at least {min_channels} channels (for --multi-out)")
    } else {
        String::new()
    };
    Err(format!(
        "the engine is built for {} Hz f32{suffix} — set the output device (or your aggregate \
         device in Audio MIDI Setup) to {} Hz and try again",
        SAMPLE_RATE, SAMPLE_RATE
    )
    .into())
}
