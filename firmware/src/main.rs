//! Audio firmware.
//!
//! # What is already decided
//!
//! Everything that matters architecturally:
//!
//! - the engine is constructed once, before interrupts are enabled
//! - the audio interrupt plays pre-rendered double-buffered blocks; the main
//!   loop renders them via `audio::render_next` (a whole `process()` cannot
//!   fit inside the ISR without underrunning the TX FIFO)
//! - MIDI is parsed in the engine crate, so it is host-testable
//! - parameter updates happen outside the callback, at CC rate
//!
//! # What is where
//!
//! The SAI1-to-PCM5102 audio path lives in the [`audio`] module: the 48 kHz
//! clock chain, pad routing, SAI1 configuration, and the FIFO-request
//! interrupt that pumps samples. This file wires it together and owns the
//! MIDI transports.
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
//!   3.3V            ──────► A3V3   (if exposed — see below)
//!   3.3V            ──────► XSMT   (if exposed — see below)
//!   GND             ──────► GND
//!   GND             ──────► SCK    (ties it low — internal PLL mode)
//!   GND             ──────► AGND   (if exposed — see below)
//! ```
//!
//! Most PCM5102A breakouts want SCK tied low to select internal PLL mode.
//! Check the silkscreen on yours; some have a solder jumper for it already.
//! On boards that break out more of the chip's own pins (rather than tying
//! them internally), three more connections are not optional — miss any one
//! and you get total, otherwise-unexplained silence despite BCLK/LRCLK/DATA
//! all being correct: **AGND** (analog ground, separate from digital `GND`)
//! and **A3V3** (analog supply, separate from digital `VIN`) both need
//! their own wire — an unpowered or ungrounded analog stage stays silent
//! regardless of how correct the digital side is — and **XSMT** (soft-mute)
//! needs to be tied to 3.3V, since low or floating means muted. **FMT**
//! (audio format select) also needs to be tied to GND if exposed: low
//! selects I2S (what this firmware sends), high selects left-justified,
//! and a floating/wrong FMT shifts every sample by one bit position. See
//! [`audio`]'s module docs for the full breakdown.
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
//! MOD CC 46 is the output-pair selector: its value quantises over the
//! 4 [`drum_engine::OutPair`] variants (Master/Aux1/Aux2/Aux3) and routes
//! the track's dry signal to that pair in `process_dry_wet`. Both jump
//! instantly under CC rather than smoothing — discrete choices.
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

mod audio;
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
        mut pins,
        lpuart6,
        usb,
        mut ccm,
        mut ccm_analog,
        sai1,
        ..
    } = board::t41(board::instances());

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

    // SAI1 as an I2S master at exactly 48 kHz: the 48 kHz clock chain, pad
    // routing, and SAI1 configuration live in the `audio` module. The SAI
    // interrupt never touches the engine — it only plays the double-buffered
    // blocks the main loop renders via `audio::render_next` below (that split
    // is what fixed the FIFO underruns the old in-ISR `process()` caused).
    // Interrupts stay masked until `audio::start()`.
    //
    // SAFETY: called exactly once, before interrupts are enabled; the engine
    // is fully constructed. Runs before `board::led` / `board::lpuart` move
    // `pins.p13` / `pins.p0` / `pins.p1` out of the struct.
    #[allow(unsafe_code)]
    unsafe {
        audio::setup(&mut ccm, &mut ccm_analog, sai1, &mut pins);
    }

    let led = board::led(&mut gpio2, pins.p13);

    // Unmask the SAI1 interrupt and hand it the LED *immediately* after
    // `setup()` enabled the transmitter/receiver — nothing else runs in
    // between. `setup()` pre-fills only 15 frames (30 words) before
    // enabling, which drains in ~312us at this bit rate; every line of init
    // that used to sit between `setup()` and this call (LPUART, USB stack
    // bring-up, FPSCR, parsers) easily eats more than that, so the FIFO was
    // underrunning — and, on this SAI block, apparently halting its
    // serializer outright rather than just flagging it — before the
    // interrupt was ever unmasked to refill it. Writing more data in after
    // the fact tops up the FIFO's contents but never restarts a halted
    // clock, which is exactly the "runs briefly, then dead forever" symptom
    // this reorder fixes. Everything else now happens after the ISR is
    // already keeping the FIFO fed.
    //
    // SAFETY: everything the ISR touches — SAI1, buffers, counter, LED — was
    // initialized; call this exactly once.
    #[allow(unsafe_code)]
    unsafe {
        audio::start(&led);
    }

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

    let mut parser_usb = MidiParser::new();
    let mut parser_uart = MidiParser::new();
    let mut usb_midi_buf = [0u8; 64];

    loop {
        // USB MIDI: the host sends 4-byte USB MIDI event packets, each a
        // status byte plus up to two data bytes. Feed those through the
        // same parser as the DIN socket — USB MIDI is just a transport.
        //
        // Polled on *every* pass: `usb::poll` also drives enumeration and
        // the control transfers, so the device disappears from the host if
        // the loop ever stops reaching it.
        let n = usb::poll(&mut usb_midi_buf);
        let mut i = 0;
        while i < n {
            for &b in &usb_midi_buf[i + 1..i + 4] {
                if let Some(event) = parser_usb.push(b) {
                    // Shared with the host `device` harness — the Teensy and
                    // the Mac tuning rig interpret the same bytes identically.
                    let offset = arrival_offset(audio::sample_counter());
                    schedule_midi(engine, event, offset);
                }
            }
            i += 4;
        }

        // Drain whatever MIDI has arrived on DIN. In the finished firmware
        // this belongs in a UART interrupt pushing into a queue, so that a
        // burst of notes cannot delay an audio deadline.
        while let Ok(byte) = midi_uart.read() {
            if let Some(event) = parser_uart.push(byte) {
                let offset = arrival_offset(audio::sample_counter());
                schedule_midi(engine, event, offset);
            }
        }

        // Render the audio: keep one block rendered ahead of the SAI
        // interrupt. `render_next` does nothing unless the ISR raised
        // `RENDER_PENDING` at a block boundary, so this costs a couple of
        // loads on every other pass and a 150–450 µs `process()` once per
        // block — inside the main loop, where the ISR can preempt it to keep
        // the TX FIFO fed (see the `audio` module docs for why rendering in
        // the ISR was the underrun bug).
        audio::render_next(engine);

        // The LED blinks from inside the audio ISR (see `audio::TEST_TONE`);
        // nothing here to do for it.
    }
}

/// Where an event drained from a transport right now should fire.
///
/// The main loop renders in whole blocks, so an event that arrives while the
/// engine is `sample_counter` samples in is scheduled to fire at that
/// position in the *next* `process` block — the engine's `TimedQueue`
/// contract, not a guess. The SAI interrupt counts in whole blocks, so the
/// derived offset is always 0: a block boundary is the earliest a note can
/// play, and `schedule_midi` with offset 0 already covers that case.
fn arrival_offset(sample_counter: u64) -> usize {
    (sample_counter % BLOCK as u64) as usize
}
