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
    // Caches first, before anything touches bulk data or the audio interrupt
    // is unmasked. This is what makes an engine in OCRAM affordable.
    //
    // The cycle counter comes up in the same breath because
    // `cortex_m::Peripherals::take()` succeeds exactly once, and
    // `audio::render_next` needs DWT running to measure its slack. Without
    // `enable_trace` the counter is gated off behind the debug block and
    // reads a constant zero, which would look like a render that costs
    // nothing rather than an instrument that is switched off.
    {
        let mut core = cortex_m::Peripherals::take().expect("core peripherals already taken");
        #[cfg(feature = "cache")]
        crate::enable_caches(&mut core.SCB, &mut core.CPUID);
        core.DCB.enable_trace();
        cortex_m::peripheral::DWT::unlock();
        core.DWT.enable_cycle_counter();
    }

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

    // Last slack report, in ms since boot.
    let mut last_report_ms: u32 = 0;
    // The report in flight: three control changes, sent one per loop pass
    // until each one lands. `usb::send_midi` returns false when the bulk
    // endpoint is still busy, and the grid's LED feedback keeps it busy
    // constantly, so a fire-and-forget send is simply dropped — which is how
    // the first version of this measured nothing at all while the board was
    // visibly transmitting. `REPORT_IDLE` means nothing is pending.
    const REPORT_IDLE: usize = 3;
    let mut report: [(u8, u8); 3] = [(0, 0); 3];
    let mut report_idx: usize = REPORT_IDLE;

    loop {
        let now_ms = (crate::audio::sample_counter() / 48) as u32;

        // Drain the pending report before the grid gets a turn at the
        // endpoint, so a saturated LED stream cannot starve it indefinitely.
        if report_idx < REPORT_IDLE {
            let (cc, value) = report[report_idx];
            if crate::usb::send_midi(&[0x0B, 0xB0 | SLACK_CHANNEL, cc, value]) {
                report_idx += 1;
            }
        }

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

        // Queue a headroom report once a second, but only once the previous
        // one has fully drained — a half-sent report would pair this second's
        // worst case with last second's counters.
        if report_idx >= REPORT_IDLE && now_ms.wrapping_sub(last_report_ms) >= SLACK_REPORT_MS {
            last_report_ms = now_ms;
            report = slack_report();
            report_idx = 0;
        }
    }
}

/// How often the headroom report goes out, in ms.
const SLACK_REPORT_MS: u32 = 1_000;

/// MIDI channel the headroom report is sent on.
///
/// 14, immediately below the grid's 15. Both are reserved: a kit would have
/// to be using all sixteen channels before this collided with anything, and
/// the grid already set that precedent.
const SLACK_CHANNEL: u8 = 14;

/// CC carrying worst-case cycles used, as 0..127 of the block budget.
const CC_SLACK_PCT: u8 = 20;
/// CC carrying late blocks since boot, saturating.
const CC_LATE_BLOCKS: u8 = 21;
/// CC carrying SAI TX FIFO underruns since boot, saturating.
const CC_UNDERRUNS: u8 = 22;

/// Build the headroom measurement as three control changes.
///
/// MIDI rather than a log line because this firmware's USB stack carries the
/// MIDI class and nothing else — `imxrt-log`'s backend owns an entire stack
/// of its own and cannot share the one bus the Teensy has, which is the whole
/// reason `usb.rs` exists. A CDC class is planned there; until it lands, three
/// control changes on a reserved channel is the channel that already works,
/// and any MIDI monitor can read it.
///
/// 7 bits is 0.8% of budget per step, which is ample: the question these
/// answer is whether the worst case sits nearer 70% or 90%, not what it is to
/// a tenth of a percent.
fn slack_report() -> [(u8, u8); 3] {
    let used = crate::audio::take_worst_used();
    // `* 127 / BUDGET` in u64 so the multiply cannot overflow before the
    // divide — `used` can legitimately exceed the budget, which is the
    // interesting case and exactly when a u32 product would wrap.
    let pct = ((used as u64 * 127) / crate::audio::BUDGET_CYCLES as u64).min(127) as u8;
    let sat = |v: u32| -> u8 { v.min(127) as u8 };
    // Control change: 0x0B is the USB-MIDI CIN for one.
    [
        (CC_SLACK_PCT, pct),
        (CC_LATE_BLOCKS, sat(crate::audio::late_blocks())),
        (CC_UNDERRUNS, sat(crate::audio::underruns() as u32)),
    ]
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
