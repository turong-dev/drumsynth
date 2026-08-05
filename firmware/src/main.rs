//! Audio firmware. **Incomplete — see the SAI section below.**
//!
//! Flash `bench` first. This binary is the shape of the finished thing, but
//! the SAI wiring is deliberately left as a marked gap rather than guessed at,
//! because `imxrt_hal::sai` shipped on 26 July 2026 and its API is not
//! something to write from memory.
//!
//! # What is already decided
//!
//! Everything that matters architecturally:
//!
//! - the engine is constructed once, before interrupts are enabled
//! - the audio callback does nothing but interleave and call `process`
//! - MIDI is parsed in the engine crate, so it is host-testable
//! - parameter updates happen outside the callback
//!
//! # What is missing
//!
//! One function: getting a buffer of samples to the DAC on a clock. Search
//! for `TODO(sai)`.
//!
//! # Recommended hardware
//!
//! A PCM5102A or PCM5100 I2S breakout, not the Teensy Audio Shield. Those
//! parts are hardware-strapped: no I2C, no register writes, no codec driver
//! to find or write. Feed them BCLK, LRCLK and DATA and they produce
//! line-level audio. The Audio Shield's SGTL5000 needs an I2C driver that
//! does not currently exist in Rust, and there is no reason to take that on
//! for an output-only instrument.
//!
//! ```text
//!   Teensy 4.1              PCM5102A breakout
//!   ──────────              ─────────────────
//!   pin 21 (BCLK)   ──────► BCK
//!   pin 20 (LRCLK)  ──────► LCK
//!   pin 7  (OUT1A)  ──────► DIN
//!   3.3V            ──────► VIN
//!   GND             ──────► GND
//! ```
//!
//! Most PCM5102A breakouts want SCK tied low to select internal PLL mode.
//! Check the silkscreen on yours; some have a solder jumper for it already.

#![no_std]
#![no_main]

use teensy4_panic as _;

use drum_engine::{
    midi::{note_to_voice, MidiEvent, MidiParser},
    DrumEngine, Params, BLOCK,
};
use teensy4_bsp as bsp;
use teensy4_bsp::board;

/// Interleaved stereo scratch buffer handed to the DMA.
///
/// Double the block size because it is L/R interleaved. Static rather than
/// stack-allocated because DMA needs a stable address.
static mut TX_BUFFER: [f32; BLOCK * 2] = [0.0; BLOCK * 2];

#[bsp::rt::entry]
fn main() -> ! {
    let board::Resources {
        mut gpio2,
        pins,
        lpuart6,
        ..
    } = board::t41(board::instances());

    let led = board::led(&mut gpio2, pins.p13);

    // MIDI in on a hardware UART at the standard 31250 baud.
    //
    // Given your groove box can act as a USB host, USB MIDI is the better
    // route and this becomes redundant — but DIN MIDI is three parts and no
    // USB stack, so it is the faster thing to get working first.
    let mut midi_uart = board::lpuart(lpuart6, pins.p1, pins.p0, 31_250);

    let mut engine = DrumEngine::new();
    engine.set_params(&Params::default());

    let mut parser = MidiParser::new();

    // TODO(sai): bring up the audio interface.
    //
    // Roughly:
    //   1. Configure the SAI1 clock root in CCM for 48kHz. The MCLK divider
    //      chain is the fiddly part — get this wrong and you get audio at the
    //      wrong pitch, which at least tells you the data path works.
    //   2. Configure SAI1 as transmitter: I2S mode, 32-bit slots, 2 channels,
    //      master (the PCM5102A is a slave and wants BCLK and LRCLK from you).
    //   3. Set up a DMA channel from TX_BUFFER to the SAI TX FIFO, in circular
    //      double-buffered mode.
    //   4. Enable the half-transfer and transfer-complete interrupts and call
    //      `audio_callback` from each, filling whichever half is now free.
    //
    // Until that exists, the loop below runs the engine and throws the output
    // away. Useless for listening, but it exercises the full MIDI-to-audio
    // path and will surface any panic or timing problem before you have
    // hardware attached to blame.

    let mut left = [0.0f32; BLOCK];
    let mut right = [0.0f32; BLOCK];

    loop {
        // Drain whatever MIDI has arrived. In the finished firmware this
        // belongs in a UART interrupt pushing into a queue, so that a burst of
        // notes cannot delay an audio deadline.
        while let Ok(byte) = midi_uart.read() {
            if let Some(event) = parser.push(byte) {
                handle_midi(&mut engine, event);
            }
        }

        // TODO(sai): this call moves into the DMA interrupt handler.
        audio_callback(&mut engine, &mut left, &mut right);

        led.toggle();
    }
}

/// The whole of the audio interrupt.
///
/// Note what is *not* here: no allocation, no locking, no logging, no
/// parameter maths, no branching on MIDI state. Everything expensive happened
/// somewhere else. Keeping it this thin is what makes the cycle numbers from
/// `bench` meaningful — the bench measures `process`, so `process` had better
/// be substantially all of the work.
#[inline]
fn audio_callback(engine: &mut DrumEngine, left: &mut [f32; BLOCK], right: &mut [f32; BLOCK]) {
    engine.process(left, right);

    // Interleave into the DMA buffer. One linear pass, which is why the
    // engine renders planar in the first place.
    //
    // SAFETY: single-threaded access. Once DMA is real this needs to write to
    // whichever half the hardware is not currently reading, and the `static
    // mut` should become a properly split double buffer.
    #[allow(static_mut_refs)]
    let tx = unsafe { &mut TX_BUFFER };
    for i in 0..BLOCK {
        tx[i * 2] = left[i];
        tx[i * 2 + 1] = right[i];
    }
}

/// Apply a MIDI event to the engine.
fn handle_midi(engine: &mut DrumEngine, event: MidiEvent) {
    match event {
        MidiEvent::NoteOn { note, velocity } => {
            if let Some(voice) = note_to_voice(note) {
                engine.trigger(voice, velocity);
            }
        }
        MidiEvent::ControlChange { controller, value } => {
            apply_cc(engine, controller, value);
        }
        MidiEvent::Panic => engine.panic(),
    }
}

/// Map CC numbers onto parameters.
///
/// `set_params` recomputes coefficients, which involves `expf` calls — too
/// expensive for an audio interrupt but entirely fine here in the main loop.
/// If you later move MIDI handling into an interrupt, this needs to move back
/// out again, or become a "params are dirty" flag that the main loop acts on.
fn apply_cc(engine: &mut DrumEngine, controller: u8, value: f32) {
    let mut p = *engine.params();

    match controller {
        // Kick
        20 => p.kick.decay_s = 0.05 + value * 1.2,
        21 => p.kick.pitch_decay_s = 0.005 + value * 0.15,
        22 => p.kick.start_hz = 60.0 + value * 400.0,
        23 => p.kick.drive = 1.0 + value * 6.0,
        // Snare
        24 => p.snare.decay_s = 0.03 + value * 0.6,
        25 => p.snare.noise_mix = value,
        26 => p.snare.body_hz = 100.0 + value * 300.0,
        // Hat
        27 => p.hat.decay_s = 0.01 + value * 0.4,
        28 => p.hat.hp_hz = 2000.0 + value * 10_000.0,
        // Master
        7 => p.master_gain = value,
        _ => return,
    }

    engine.set_params(&p);
}
