//! Cycle budget harness.
//!
//! **This is the binary to flash first, before you own a DAC.**
//!
//! It answers the question "am I writing the engine within the hardware's
//! limits" without any audio path at all. No codec, no I2S, no jack, no SD
//! card. It calls `engine.process()` on a throwaway buffer, counts cycles with
//! the DWT counter, and reports over USB serial.
//!
//! # The budget
//!
//! At 600MHz, 48kHz, 32-frame blocks:
//!
//! ```text
//!   block period  = 32 / 48000        = 666.7 µs
//!   cycle budget  = 600e6 × 666.7e-6  = 400,000 cycles per block
//!                                     =  12,500 cycles per output frame
//! ```
//!
//! Anything under about 70% is comfortable. Above that you are gambling that
//! nothing else — MIDI parsing, SD reads, USB interrupts — ever lands badly.
//!
//! # Reading the output
//!
//! ```text
//! voices=3 idle     avg=  1042 cy  peak=  1108 cy   0.3% of budget
//! voices=3 sounding avg= 21883 cy  peak= 23104 cy   5.5% of budget
//! ```
//!
//! Two numbers because the voices early-out when their envelope is at zero.
//! The idle figure tells you the floor; the sounding figure is what you must
//! fit in. Always size against sounding, and against *peak*, not average —
//! the audio callback has to make its deadline every single time, and an
//! average that fits while the peak does not is a click you will hear.

#![no_std]
#![no_main]

use teensy4_panic as _;

use cortex_m::peripheral::DWT;
use drum_engine::{DrumEngine, VoiceId, BLOCK, SAMPLE_RATE};
use teensy4_bsp as bsp;
use teensy4_bsp::board;

/// Nominal core clock. Used only to turn cycles into a percentage — if your
/// board is clocked differently, fix this or the percentages lie.
const CORE_HZ: f32 = 600_000_000.0;

/// Cycles available per block before the audio callback misses its deadline.
const BUDGET: f32 = CORE_HZ * BLOCK as f32 / SAMPLE_RATE;

/// Blocks per measurement run. Enough to average out cache warming.
const RUNS: usize = 512;

#[bsp::rt::entry]
fn main() -> ! {
    let board::Resources {
        pit,
        usb,
        mut gpio2,
        pins,
        ..
    } = board::t41(board::instances());

    // The onboard LED, as a liveness indicator. If this is not blinking,
    // something panicked before the log came up and you are staring at a dead
    // serial port wondering why.
    let led = board::led(&mut gpio2, pins.p13);

    let (_, _, _, mut pit3) = pit;
    let mut poller = imxrt_log::log::usbd(usb, imxrt_log::Interrupts::Disabled)
        .expect("failed to bring up USB logging");

    enable_cycle_counter();

    let mut engine = DrumEngine::new();
    let mut left = [0.0f32; BLOCK];
    let mut right = [0.0f32; BLOCK];

    // Give the host a moment to enumerate and for you to attach a terminal.
    // Without this you miss the header every time.
    delay_blocking(&mut poller, &mut pit3, 3_000);

    log::info!("");
    log::info!("drum-engine cycle bench");
    log::info!("core {:.0} MHz, {} Hz, block {}", CORE_HZ / 1e6, SAMPLE_RATE, BLOCK);
    log::info!("budget {:.0} cycles per block", BUDGET);
    log::info!("");

    loop {
        led.toggle();

        // --- Idle: no voice sounding, everything early-outs ---
        engine.panic();
        let idle = measure(&mut engine, &mut left, &mut right, false);
        report("idle    ", idle);

        // --- Sounding: all three voices retriggered continuously ---
        let sounding = measure(&mut engine, &mut left, &mut right, true);
        report("sounding", sounding);
        log::info!("");

        poller.poll();
        delay_blocking(&mut poller, &mut pit3, 2_000);
    }
}

struct Stats {
    avg: u32,
    peak: u32,
}

/// Run the engine `RUNS` times and collect cycle counts.
///
/// `retrigger` keeps all voices sounding, which is the worst case — no voice
/// hits its early-out path.
fn measure(
    engine: &mut DrumEngine,
    left: &mut [f32; BLOCK],
    right: &mut [f32; BLOCK],
    retrigger: bool,
) -> Stats {
    let mut total: u64 = 0;
    let mut peak: u32 = 0;

    // One untimed pass so the branch predictor and caches are warm. Without
    // it the first sample is an outlier that skews the peak.
    engine.process(left, right);

    for _ in 0..RUNS {
        if retrigger {
            engine.trigger(VoiceId::Kick, 1.0);
            engine.trigger(VoiceId::Snare, 1.0);
            engine.trigger(VoiceId::Hat, 1.0);
        }

        let start = DWT::cycle_count();
        engine.process(left, right);
        let end = DWT::cycle_count();

        // Wrapping subtract: CYCCNT is a free-running 32-bit counter and does
        // roll over, roughly every 7 seconds at 600MHz.
        let elapsed = end.wrapping_sub(start);

        total += elapsed as u64;
        if elapsed > peak {
            peak = elapsed;
        }

        // Defeat dead-code elimination. Without this the optimiser is
        // entitled to notice nothing reads the buffers and delete the whole
        // call, and you get a bench that reports four cycles.
        core::hint::black_box(&left);
        core::hint::black_box(&right);
    }

    Stats {
        avg: (total / RUNS as u64) as u32,
        peak,
    }
}

fn report(label: &str, s: Stats) {
    let pct = s.peak as f32 * 100.0 / BUDGET;
    let per_frame = s.peak / BLOCK as u32;

    log::info!(
        "{} avg={:>7} cy  peak={:>7} cy  {:>5.1}% of budget  ({} cy/frame)",
        label,
        s.avg,
        s.peak,
        pct,
        per_frame,
    );

    if pct > 70.0 {
        log::warn!("  ^ over 70% — little headroom for MIDI, SD or USB work");
    }
}

/// Turn on the DWT cycle counter.
///
/// It is gated behind the debug block, which is disabled out of reset, so
/// both steps are required. Skip the `DCB` enable and `cycle_count()` returns
/// a constant zero, which looks exactly like an implausibly fast engine.
fn enable_cycle_counter() {
    let mut core = cortex_m::Peripherals::take().expect("core peripherals already taken");
    core.DCB.enable_trace();
    // The DWT block carries the lock-access register; writing the key is a
    // no-op on cores whose DWT is not locked.
    DWT::unlock();
    core.DWT.enable_cycle_counter();
}

/// Block for roughly `ms`, pumping the USB poller so logs keep flowing.
fn delay_blocking(
    poller: &mut imxrt_log::Poller,
    pit: &mut bsp::hal::pit::Pit<3>,
    ms: u32,
) {
    // PIT runs from the perclk root; board::PERCLK_FREQUENCY is the divisor
    // to use. Chunked so the poller runs often enough that USB does not stall.
    const CHUNK_MS: u32 = 10;
    let ticks = (board::PERCLK_FREQUENCY / 1_000) * CHUNK_MS;

    for _ in 0..(ms / CHUNK_MS) {
        pit.set_load_timer_value(ticks);
        pit.enable();
        while !pit.is_elapsed() {
            poller.poll();
        }
        pit.clear_elapsed();
        pit.disable();
    }
}
