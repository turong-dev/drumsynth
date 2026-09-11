//! Shared main loop for device firmware.
//!
//! [`run`] wires a [`DeviceEngine`](drum_engine::engine::DeviceEngine) to the
//! Teensy 4.1 audio, USB MIDI and DIN MIDI peripherals. Device-specific
//! binaries create their engine (usually in a `.uninit` static) and call
//! `runner::run(engine)`.

use drum_engine::{DeviceEngine, DeviceModel, Slot};
use drum_engine::midi::{schedule_midi, MidiParser};
use drum_engine::BLOCK;

#[cfg(feature = "grid")]
use drum_engine::grid::{Grid, GridParser, MIDIGRID_CHANNEL};

use teensy4_bsp::board;

// The LPUART `read()` is a trait method (embedded-hal 0.2 `serial::Read`),
// not inherent — bring it into scope or the call won't resolve.
use embedded_hal::serial::Read as _;

/// Run the device main loop forever.
///
/// `engine` must be fully initialized and live for the lifetime of the
/// program (typically placed in a `.uninit` static). This function sets up
/// the SAI1 audio path, USB and DIN MIDI transports, then polls forever.
pub fn run<E, const N: usize>(engine: &'static mut E) -> !
where
    E: DeviceEngine<N>,
    <E::Slot as Slot<N>>::Id: DeviceModel<N>,
{
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

    // SAFETY: `engine` is `.uninit` OCRAM (or equivalent), written exactly
    // once before interrupts are enabled — single-threaded init, same
    // requirement the in-place constructors document.
    #[allow(unsafe_code)]
    unsafe {
        crate::audio::setup(&mut ccm, &mut ccm_analog, sai1, &mut pins);
    }

    let led = board::led(&mut gpio2, pins.p13);

    // Unmask the SAI1 interrupt and hand it the LED *immediately* after
    // `setup()` enabled the transmitter/receiver. See the original firmware
    // docs for why this ordering matters (a pre-filled FIFO underruns before
    // the ISR is ever unmasked to refill it).
    #[allow(unsafe_code)]
    unsafe {
        crate::audio::start(&led);
    }

    // DIN MIDI on LPUART6 at 31250 baud.
    let mut midi_uart = board::lpuart(lpuart6, pins.p1, pins.p0, 31_250);

    // Shared USB stack — MIDI class on the one bus the Teensy has.
    #[allow(unsafe_code)]
    unsafe {
        crate::usb::init(usb);
    }

    // FPSCR.FZ — flush denormals to zero across the whole core.
    #[allow(unsafe_code)]
    unsafe {
        let mut fpscr: u32;
        core::arch::asm!("vmrs {}, fpscr", out(reg) fpscr);
        fpscr |= 1 << 24; // FZ
        core::arch::asm!("vmsr fpscr, {}", in(reg) fpscr);
    }

    let mut parser_usb = MidiParser::new();
    let mut parser_uart = MidiParser::new();
    #[cfg(feature = "grid")]
    let mut parser_grid_usb = GridParser::new();
    #[cfg(feature = "grid")]
    let mut parser_grid_uart = GridParser::new();
    #[cfg(feature = "grid")]
    let mut grid = Grid::new();
    let mut usb_midi_buf = [0u8; 64];

    loop {
        #[cfg(feature = "grid")]
        let now_ms = (crate::audio::sample_counter() / 48) as u32;
        #[cfg(not(feature = "grid"))]
        let _now_ms = (crate::audio::sample_counter() / 48) as u32;

        // USB MIDI: the host sends 4-byte USB MIDI event packets. Channel 16
        // is reserved for the Monome Grid; feed it to the grid parser as well.
        let n = crate::usb::poll(&mut usb_midi_buf);
        let mut i = 0;
        while i < n {
            for &b in &usb_midi_buf[i + 1..i + 4] {
                #[cfg(feature = "grid")]
                if let Some(event) = parser_grid_usb.push(b) {
                    grid.process_event(event, now_ms, engine);
                }
                if let Some(event) = parser_usb.push(b) {
                    let offset = arrival_offset(crate::audio::sample_counter());
                    schedule_midi(engine, event, offset);
                }
            }
            i += 4;
        }

        // Drain DIN MIDI.
        while let Ok(byte) = midi_uart.read() {
            #[cfg(feature = "grid")]
            if let Some(event) = parser_grid_uart.push(byte) {
                grid.process_event(event, now_ms, engine);
            }
            if let Some(event) = parser_uart.push(byte) {
                let offset = arrival_offset(crate::audio::sample_counter());
                schedule_midi(engine, event, offset);
            }
        }

        // Render grid LED feedback. Only the USB path carries the midigrid
        // class; DIN MIDI has no LED return.
        #[cfg(feature = "grid")]
        grid.render(engine, now_ms, |_ch, note, vel| {
            let packet = [0x09, 0x90 | MIDIGRID_CHANNEL, note, vel];
            crate::usb::send_midi(&packet)
        });

        // Render the next audio block when the ISR asks for it.
        crate::audio::render_next(engine);
    }
}

/// Where an event drained from a transport right now should fire.
///
/// The main loop renders in whole blocks, so an event that arrives while the
/// engine is `sample_counter` samples in is scheduled to fire at that
/// position in the *next* `process` block. The SAI interrupt counts in whole
/// blocks, so the derived offset is always 0 today.
fn arrival_offset(sample_counter: u64) -> usize {
    (sample_counter % BLOCK as u64) as usize
}
