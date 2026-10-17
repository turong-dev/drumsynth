//! End-to-end test of the `device` mode's MIDI half, over real CoreMIDI:
//!
//! 1. A virtual input port (exactly what `device.rs` creates).
//! 2. A sender that connects to it the way a DAW or controller would.
//! 3. The parse → ring → router chain the device's audio thread drains.
//!
//! No audio hardware is touched, so this runs anywhere with CoreMIDI.

use drum_engine::midi::{handle_midi, MidiEvent, MidiParser};
use drum_engine::{DrumEngine, MachineId};
use mi_drum_engine::{seed_random, DeviceEngine, MiDrumEngine, DEFAULT_RANDOM_SEED};
use midir::os::unix::VirtualInput;
use midir::{Ignore, MidiInput, MidiOutput};
use rtrb::{Consumer, Producer, RingBuffer};
use std::sync::atomic::{AtomicU32, Ordering};
use std::time::{Duration, Instant};

/// The reason `render device mi-drum` runs at all.
///
/// `device::run` hands the engine to cpal's audio-callback thread, so it
/// requires `E: Send`. `MiDrumEngine` only became `Send` when
/// `plaits::EngineRegistry` stopped caching pointers to its own members and
/// started storing offsets (`mi-dsp/vendor/plaits/dsp/engine/engine.h`). This
/// is a compile-time assertion on purpose: if a voice ever goes back to holding
/// a self-pointer, this file stops building rather than the host binary
/// crashing on the audio thread.
///
/// Deliberately not `Sync`. A voice may move between threads; two threads may not
/// share one.
#[test]
fn both_devices_are_send() {
    fn assert_send<T: Send>() {}
    assert_send::<DrumEngine>();
    assert_send::<MiDrumEngine>();
}

/// Heap-allocate the mi-drum engine; six Plaits voices is far more than a test
/// thread's stack has room for.
fn mi_engine_box() -> Box<MiDrumEngine> {
    use std::alloc::{alloc, Layout};
    unsafe {
        let layout = Layout::new::<MiDrumEngine>();
        let ptr = alloc(layout) as *mut MiDrumEngine;
        assert!(!ptr.is_null(), "failed to allocate MiDrumEngine");
        MiDrumEngine::new_in_place(ptr);
        Box::from_raw(ptr)
    }
}

#[test]
fn daw_note_on_triggers_the_mi_drum_engine() {
    let port_name = port_name("mi-note");
    let (_in_conn, mut consumer) = spawn_device_input(&port_name);

    let out = MidiOutput::new("drumkit-engine-test-out").unwrap();
    let port = out
        .ports()
        .into_iter()
        .find(|p| out.port_name(p).map(|n| n == port_name).unwrap_or(false))
        .expect("virtual input port should be visible to outputs");
    let mut conn = out.connect(&port, "drumkit-engine-test-sender").unwrap();

    // Channel 5 → track 5, one of the two Plaits tracks in the default kit, so
    // this exercises the C++ voice rather than a Peaks drum model.
    conn.send(&[0x95, 60, 110]).unwrap();

    let ev = pop_timeout(&mut consumer, Duration::from_secs(2))
        .expect("mi-drum note-on never reached the ring");
    let MidiEvent::NoteOn { channel, .. } = ev else {
        panic!("expected NoteOn, got {ev:?}");
    };
    assert_eq!(channel, 5);

    seed_random(DEFAULT_RANDOM_SEED);
    let mut engine = mi_engine_box();
    handle_midi(&mut *engine, ev);
    assert!(
        engine.tracks()[5].is_active(),
        "track 5 should be sounding after its note-on"
    );

    // The whole point of the route: the triggered track must actually render
    // audio through the Plaits FFI, not merely be flagged active.
    let mut l = [0.0f32; 32];
    let mut r = [0.0f32; 32];
    let mut peak = 0.0f32;
    for _ in 0..64 {
        engine.process(&mut l, &mut r);
        peak = peak.max(l.iter().fold(0.0f32, |m, &s| m.max(s.abs())));
    }
    assert!(peak > 0.001, "track 5 rendered silence: peak={peak}");
}

static PORT_SEQ: AtomicU32 = AtomicU32::new(0);

/// Unique virtual-port name so parallel tests can't steal each other's ports.
fn port_name(tag: &str) -> String {
    format!(
        "drumkit-engine-test-{tag}-{}",
        PORT_SEQ.fetch_add(1, Ordering::Relaxed)
    )
}

/// Create the device's input side: virtual CoreMIDI port, parser + producer
/// living in the callback, consumer handed back to the caller.
fn spawn_device_input(
    port: &str,
) -> (
    midir::MidiInputConnection<(MidiParser, Producer<MidiEvent>)>,
    Consumer<MidiEvent>,
) {
    let (producer, consumer) = RingBuffer::new(64);
    let mut midi_in = MidiInput::new("drumkit-engine-test-in").unwrap();
    midi_in.ignore(Ignore::None);
    let conn = midi_in
        .create_virtual(
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
        )
        .unwrap();
    (conn, consumer)
}

/// CoreMIDI delivers asynchronously, so poll the ring until something lands.
fn pop_timeout(consumer: &mut Consumer<MidiEvent>, timeout: Duration) -> Option<MidiEvent> {
    let deadline = Instant::now() + timeout;
    loop {
        if let Ok(ev) = consumer.pop() {
            return Some(ev);
        }
        if Instant::now() >= deadline {
            return None;
        }
        std::thread::sleep(Duration::from_millis(5));
    }
}

#[test]
fn daw_note_on_triggers_the_engine() {
    let port_name = port_name("note");
    let (_in_conn, mut consumer) = spawn_device_input(&port_name);

    // DAW side: a real output endpoint pointed at our virtual port.
    let out = MidiOutput::new("drumkit-engine-test-out").unwrap();
    let port = out
        .ports()
        .into_iter()
        .find(|p| out.port_name(p).map(|n| n == port_name).unwrap_or(false))
        .expect("virtual input port should be visible to outputs");
    let mut conn = out.connect(&port, "drumkit-engine-test-sender").unwrap();

    // Kick note-on: channel 0 → track 0, middle C (reference pitch).
    conn.send(&[0x90, 60, 100]).unwrap();

    let ev = pop_timeout(&mut consumer, Duration::from_secs(2))
        .expect("kick note-on never reached the ring");
    let MidiEvent::NoteOn {
        note,
        velocity,
        channel,
        ..
    } = ev
    else {
        panic!("expected NoteOn, got {ev:?}");
    };
    assert_eq!(channel, 0);
    assert_eq!(note, 60);
    assert!((velocity - 100.0 / 127.0).abs() < 1e-3);

    // Drain through the router, as the device's audio thread does.
    let mut engine = DrumEngine::new();
    handle_midi(&mut engine, ev);
    assert!(
        engine.tracks[0].is_active(),
        "kick track should be sounding"
    );
    assert_eq!(engine.tracks[0].id(), MachineId::BdClassic);
    assert!(
        !engine.tracks[1].is_active(),
        "channel 0 must not trigger track 1"
    );
}

#[test]
fn daw_nrpn_reaches_the_track_params() {
    use drum_engine::machines::ParamGroup;
    use drum_engine::midi::NRPN_MAX;

    let port_name = port_name("nrpn");
    let (_in_conn, mut consumer) = spawn_device_input(&port_name);

    let out = MidiOutput::new("drumkit-engine-test-out").unwrap();
    let port = out
        .ports()
        .into_iter()
        .find(|p| out.port_name(p).map(|n| n == port_name).unwrap_or(false))
        .unwrap();
    let mut conn = out.connect(&port, "drumkit-engine-test-sender").unwrap();

    // A full NRPN transaction the way a controller sends one: select
    // (VOICE, 4) — BD Classic's DRIVE, flat 7 — then the data pair.
    // (64 << 7) | 0 = 8192, half scale.
    //
    // One CoreMIDI packet per message, not one packet with running status:
    // that is what a controller emits, and midir validates each `send` as a
    // single well-formed message.
    let voice = ParamGroup::Voice as u8;
    for msg in [
        [0xB0, 99, voice],
        [0xB0, 98, 4],
        [0xB0, 6, 64],
        [0xB0, 38, 0],
    ] {
        conn.send(&msg).unwrap();
    }

    // CC 99 and CC 98 are consumed by the parser and emit nothing; the two
    // data messages each produce an event.
    let ev = pop_timeout(&mut consumer, Duration::from_secs(2))
        .expect("the data MSB never reached the ring");
    let MidiEvent::Nrpn {
        channel,
        group,
        index,
        value,
    } = ev
    else {
        panic!("expected Nrpn, got {ev:?}");
    };
    assert_eq!((channel, group, index), (0, voice, 4));
    // MSB alone is the coarse value, 64<<7 | 64.
    assert_eq!(value, (64 << 7) | 64);

    let ev = pop_timeout(&mut consumer, Duration::from_secs(2))
        .expect("the data LSB never reached the ring");
    let MidiEvent::Nrpn { value, .. } = ev else {
        panic!("expected Nrpn, got {ev:?}");
    };
    assert_eq!(value, 64 << 7, "the LSB recomputes the full 14-bit value");

    let mut engine = DrumEngine::new();
    handle_midi(&mut engine, ev);
    // Track params are block-rate smoothed (see the midi module docs): the
    // base param must not jump to the target instantly, so converge the
    // smoother before asserting the final value.
    let target = 8192.0 / NRPN_MAX as f32;
    assert!(
        (engine.tracks[0].base_params[7] - target).abs() > 0.3,
        "an NRPN value should not apply instantly — it is block-rate smoothed"
    );
    for _ in 0..200 {
        engine.tracks[0].control();
    }
    assert_eq!(engine.tracks[0].base_params[7], target);

    // The same address on channel 3 edits track 3 only — channel scoping
    // over the wire, with its own per-channel selection state in the parser.
    for msg in [
        [0xB3, 99, voice],
        [0xB3, 98, 4],
        [0xB3, 6, 32],
        [0xB3, 38, 0],
    ] {
        conn.send(&msg).unwrap();
    }
    // Drop the coarse event, keep the 14-bit one.
    let _ = pop_timeout(&mut consumer, Duration::from_secs(2))
        .expect("the channel-3 data MSB never reached the ring");
    let ev = pop_timeout(&mut consumer, Duration::from_secs(2))
        .expect("the channel-3 data LSB never reached the ring");
    let MidiEvent::Nrpn { channel, .. } = ev else {
        panic!("expected Nrpn, got {ev:?}");
    };
    assert_eq!(channel, 3);
    handle_midi(&mut engine, ev);
    for _ in 0..200 {
        engine.tracks[3].control();
    }
    assert_eq!(engine.tracks[3].base_params[7], 4096.0 / NRPN_MAX as f32);
    assert_eq!(
        engine.tracks[0].base_params[7], target,
        "channel 3 must not touch track 0"
    );
}
