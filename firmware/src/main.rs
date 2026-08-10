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
//! - parameter updates happen outside the callback, at CC rate
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
//! line-level audio. The Audio Shield's SGTL5000 needs an I2C bootstrap
//! (power-up, clock setup, DAC enable, volume) — a WIP `sgtl5000` crate now
//! exists (0.0.1, effectively unmaintained) but it adds a fragile dependency
//! on top of the same SAI + DMA work either board needs, and there is no
//! reason to take that on for an output-only instrument. The shield only
//! pays for itself if you want its headphone amp or line-in ADC (the
//! resample/loopback path).
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
//!
//! # MIDI CC map
//!
//! MIDI is routed by the shared `drum_engine::midi` router: **one channel
//! per track**, so a CC on channel `N` edits track `N`. The CC map is the
//! same flat macro index used everywhere — `CC (20 + idx)` sets macro `idx`
//! (see [`drum_engine::midi::CC_TRACK_BASE`]):
//!
//! - CC 7  = master gain                              (0..1, global)
//! - CC 20..27  = PITCH  macros 0..7   on the track's channel
//! - CC 28..35  = FILTER macros 8..15  on the track's channel
//! - CC 36..43  = AMP    macros 16..23 on the track's channel
//! - CC 44..51  = MOD    macros 24..31 on the track's channel
//! - CC 120, 123 = panic (all sound off), any channel
//!
//! PITCH CC 25 is the machine selector: its value quantises over
//! [`drum_engine::MachineId::ALL`] and loads that machine on the track.
//!
//! MIDI channels are conventionally labelled 1..=16; on the wire the nibble
//! is 0-based, so channel 1 = wire 0 = track 0, up to channel 8 = wire 7 =
//! track 7. Channels 9..=16 have no track — notes are silent, CCs ignored.
//!
//! # MIDI in
//!
//! Two transports, both routed through the shared [`drum_engine::midi`]
//! router and the sample-accuracy machinery: USB MIDI on the main USB port
//! (the [`usb`] module owns the one bus the Teensy has) and DIN MIDI on
//! LPUART6 at 31250 baud for anyone still wired that way. The USB path
//! drains 4-byte USB MIDI event packets and feeds their data bytes through
//! a [`MidiParser`], because USB MIDI is just a transport wrapper around
//! ordinary MIDI bytes.

#![no_std]
#![no_main]

use teensy4_panic as _;

mod usb;

use drum_engine::{
    midi::{schedule_midi, MidiParser},
    DrumEngine, BLOCK,
};
// The LPUART `read()` is a trait method (embedded-hal 0.2 `serial::Read`),
// not inherent — bring it into scope or the call won't resolve.
use embedded_hal::serial::Read as _;
use teensy4_bsp as bsp;
use teensy4_bsp::board;

/// Interleaved stereo scratch buffer handed to the DMA.
///
/// Double the block size because it is L/R interleaved. Static rather than
/// stack-allocated because DMA needs a stable address.
static mut TX_BUFFER: [f32; BLOCK * 2] = [0.0; BLOCK * 2];

/// The engine itself (~266 KB, almost all of it the send-FX delay/reverb
/// buffers) — in OCRAM via a `.uninit` static, *not* the stack. `t4link.x`
/// gives this target a 16 KB DTCM stack, and `DrumEngine::new()`'s return
/// value alone is over 16x that; see [`DrumEngine::new_in_place`], which is
/// what actually initializes this below.
#[link_section = ".uninit"]
static mut ENGINE_BUF: core::mem::MaybeUninit<DrumEngine> = core::mem::MaybeUninit::uninit();

#[bsp::rt::entry]
fn main() -> ! {
    let board::Resources {
        mut gpio2,
        pins,
        lpuart6,
        usb,
        ..
    } = board::t41(board::instances());

    let led = board::led(&mut gpio2, pins.p13);

    // MIDI in on a hardware UART at the standard 31250 baud.
    //
    // Given your groove box can act as a USB host, USB MIDI is the better
    // route and this becomes redundant — but DIN MIDI is three parts and no
    // USB stack, so it is the faster thing to get working first.
    let mut midi_uart = board::lpuart(lpuart6, pins.p1, pins.p0, 31_250);

    // The shared USB stack — MIDI class on the one bus the Teensy has (see
    // the `usb` module for why the class lives here rather than in a crate).
    //
    // SAFETY: called exactly once, here, before the loop polls it; interrupts
    // do not exist yet.
    #[allow(unsafe_code)]
    unsafe {
        usb::init(usb);
    }

    // FPSCR.FZ — flush denormals to zero across the whole core.
    //
    // The engine clamps its own tail state via `dsp::DENORMAL_FLOOR`, but
    // that only covers what it can see. The send-FX buses, the `libm`
    // helpers, and any future code all flush at the FPU boundary instead,
    // which is the only guarantee that a long decay cannot turn into a
    // denormal stall. One bit at init, paid forever after.
    #[allow(unsafe_code)]
    unsafe {
        let mut fpscr: u32;
        core::arch::asm!("vmrs {}, fpscr", out(reg) fpscr);
        fpscr |= 1 << 24; // FZ
        core::arch::asm!("vmsr fpscr, {}", in(reg) fpscr);
    }

    // SAFETY: `ENGINE_BUF` is `.uninit` OCRAM, written exactly once, here,
    // before interrupts are enabled — single-threaded init, same
    // requirement `new_in_place` documents. Raw-pointer construction
    // (rather than `&mut ENGINE_BUF`) sidesteps the Rust 2024 warning on
    // mutable-static references.
    #[allow(unsafe_code)]
    let engine: &'static mut DrumEngine = unsafe {
        let p: *mut DrumEngine = core::ptr::addr_of_mut!(ENGINE_BUF).cast();
        DrumEngine::new_in_place(p)
    };

    let mut parser_usb = MidiParser::new();
    let mut parser_uart = MidiParser::new();

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

    // Samples rendered so far. The next `process` block starts at this count.
    // Before SAI this is a plain counter incremented once per block, so the
    // offset below is always 0; once the audio interrupt owns it, the same
    // arithmetic turns into the arrival-sample timing that `schedule_midi`
    // was built for. Nothing else here needs to change for that.
    let mut sample_counter: u64 = 0;
    let mut usb_midi_buf = [0u8; 64];

    loop {
        // USB MIDI: the host sends 4-byte USB MIDI event packets, each a
        // status byte plus up to two data bytes. Feed those through the
        // same parser as the DIN socket — USB MIDI is just a transport.
        let n = usb::poll(&mut usb_midi_buf);
        let mut i = 0;
        while i < n {
            for &b in &usb_midi_buf[i + 1..i + 4] {
                if let Some(event) = parser_usb.push(b) {
                    // Shared with the host `device` harness — the Teensy and
                    // the Mac tuning rig interpret the same bytes identically.
                    schedule_midi(engine, event, arrival_offset(sample_counter));
                }
            }
            i += 4;
        }

        // Drain whatever MIDI has arrived on DIN. In the finished firmware
        // this belongs in a UART interrupt pushing into a queue, so that a
        // burst of notes cannot delay an audio deadline.
        while let Ok(byte) = midi_uart.read() {
            if let Some(event) = parser_uart.push(byte) {
                schedule_midi(engine, event, arrival_offset(sample_counter));
            }
        }

        // TODO(sai): this call moves into the DMA interrupt handler.
        audio_callback(engine, &mut left, &mut right);
        sample_counter += BLOCK as u64;

        led.toggle();
    }
}

/// Where an event drained from a transport right now should fire.
///
/// The main loop renders in whole blocks, so an event that arrives while the
/// engine is `sample_counter` samples in is scheduled to fire at that
/// position in the *next* `process` block — the engine's `TimedQueue`
/// contract, not a guess. Before SAI this is always 0 (the loop only reaches
/// the drain points between blocks), which is exactly right: a block boundary
/// is the earliest a note can play.
fn arrival_offset(sample_counter: u64) -> usize {
    (sample_counter % BLOCK as u64) as usize
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
