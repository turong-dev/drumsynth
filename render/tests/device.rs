//! End-to-end test of the `device` mode's MIDI half, over real CoreMIDI:
//!
//! 1. A virtual input port (exactly what `device.rs` creates).
//! 2. A sender that connects to it the way a DAW or controller would.
//! 3. The parse → ring → router chain the device's audio thread drains.
//!
//! No audio hardware is touched, so this runs anywhere with CoreMIDI.

use drum_engine::midi::{handle_midi, MidiEvent, MidiParser};
use drum_engine::{DrumEngine, MachineId};
use midir::os::unix::VirtualInput;
use midir::{Ignore, MidiInput, MidiOutput};
use rtrb::{Consumer, Producer, RingBuffer};
use std::sync::atomic::{AtomicU32, Ordering};
use std::time::{Duration, Instant};

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
fn daw_cc_reaches_the_track_macro_grid() {
    let port_name = port_name("cc");
    let (_in_conn, mut consumer) = spawn_device_input(&port_name);

    let out = MidiOutput::new("drumkit-engine-test-out").unwrap();
    let port = out
        .ports()
        .into_iter()
        .find(|p| out.port_name(p).map(|n| n == port_name).unwrap_or(false))
        .unwrap();
    let mut conn = out.connect(&port, "drumkit-engine-test-sender").unwrap();

    // CC 20 + 3 → track 0, macro 3 (SWEEP). Value 64 → 0.5.
    conn.send(&[0xB0, 23, 64]).unwrap();

    let ev = pop_timeout(&mut consumer, Duration::from_secs(2)).expect("CC never reached the ring");
    let MidiEvent::ControlChange {
        controller, value, ..
    } = ev
    else {
        panic!("expected ControlChange, got {ev:?}");
    };
    assert_eq!(controller, 23);
    assert!((value - 64.0 / 127.0).abs() < 1e-3);

    let mut engine = DrumEngine::new();
    handle_midi(&mut engine, ev);
    // Track macros are block-rate smoothed (see the midi module docs): the
    // base macro must not jump to the target instantly, so converge the
    // smoother before asserting the final value.
    assert!(
        (engine.tracks[0].base_macros[3] - value).abs() > 0.3,
        "CC should not apply instantly — it is block-rate smoothed"
    );
    for _ in 0..200 {
        engine.tracks[0].control();
    }
    assert_eq!(engine.tracks[0].base_macros[3], value);

    // Same CC on channel 3 edits track 3 only — channel scoping over the wire.
    conn.send(&[0xB3, 23, 32]).unwrap();
    let ev = pop_timeout(&mut consumer, Duration::from_secs(2))
        .expect("channel-3 CC never reached the ring");
    let MidiEvent::ControlChange { channel, value, .. } = ev else {
        panic!("expected ControlChange, got {ev:?}");
    };
    assert_eq!(channel, 3);
    handle_midi(&mut engine, ev);
    for _ in 0..200 {
        engine.tracks[3].control();
    }
    assert_eq!(engine.tracks[3].base_macros[3], value);
    assert_eq!(
        engine.tracks[0].base_macros[3],
        64.0 / 127.0,
        "channel 3 must not touch track 0"
    );
}
