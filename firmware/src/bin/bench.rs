//! Cycle budget harness — 8-track + send-FX version.
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
//! budget 400000 cycles per block
//! idle    avg=... cy  peak=... cy    X% of budget
//! 3 sounding ...
//! 8 sounding ...                                  (worst case, no FX)
//! fx idle  ...                                    (FX advanced, no sends)
//! 8 + FX   ...                                    (worst case with delay + reverb)
//! ```
//!
//! Three numbers per scenario: idle (loaded kit, nothing sounding), 3-voice
//! (the original baseline, for trend), 8-voice worst case (every track
//! retriggered every block — heavier than any real performance), then the
//! same again with send-FX buses active to expose the FX cost. Always size
//! against the worst case, and against *peak*, not average — the audio
//! callback has to make its deadline every single time, and an average that
//! fits while the peak does not is a click you will hear.
//!
//! Before the sine-table swap in `engine::dsp::fast`, the 3-voice
//! predecessor measured 125,907 cycles (31.5% of budget), ~90% of it inside
//! `libm::sinf`.
//!
//! Phase 5 adds two send-FX scenarios: with FX advanced but no sends
//! ("fx idle" — isolates the FX bookkeeping cost) and with two tracks
//! sending to delay + reverb ("8 + FX" — the realistic worst case).

#![no_std]
#![no_main]

use teensy4_panic as _;

use cortex_m::peripheral::DWT;
use drum_engine::{DrumEngine, BLOCK, SAMPLE_RATE, TRACKS};
use teensy4_bsp as bsp;
use teensy4_bsp::board;

/// Nominal core clock. Used only to turn cycles into a percentage — if your
/// board is clocked differently, fix this or the percentages lie.
const CORE_HZ: f32 = 600_000_000.0;

/// The engine + its send-FX buffers (~256 KB for the delay line + reverb
/// tanks) live in OCRAM via a `.uninit` static, *not* on the stack: the
/// 16 KB stack configured by `t4link.x` cannot hold it, and Phase 5 grew
/// the engine past the point where a stack-allocated local was viable.
/// Initialized below via [`DrumEngine::new_in_place`], not
/// `MaybeUninit::write(DrumEngine::new())` — see the comment at the call
/// site. Doing the write from a single-threaded `main` is sound here — the
/// bench is run-once with interrupts managed by `imxrt_log`.
#[link_section = ".uninit"]
static mut ENGINE_BUF: core::mem::MaybeUninit<DrumEngine> = core::mem::MaybeUninit::uninit();

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

    // Initialize the engine in OCRAM via `new_in_place`, not `p.write(DrumEngine::new())`.
    // The latter still has to build `DrumEngine::new()`'s ~266 KB return
    // value as a value before moving it through `write` — whether that
    // move becomes a direct in-place construction or a stack temporary
    // plus a 266 KB memcpy is an optimizer decision, and the 16 KB DTCM
    // stack survives only the former. `new_in_place` writes every field
    // straight through the pointer instead, so there's nothing for the
    // optimizer to get right or wrong. We construct the reference by raw
    // pointer (rather than going through `&mut ENGINE_BUF`) because Rust
    // 2024 warns on mutable references to mutable statics — and what
    // we're doing here genuinely is sound, but we want to keep the build
    // warning-clean.
    let engine: &'static mut DrumEngine = unsafe {
        let p: *mut DrumEngine = core::ptr::addr_of_mut!(ENGINE_BUF).cast();
        DrumEngine::new_in_place(p)
    };
    let mut left = [0.0f32; BLOCK];
    let mut right = [0.0f32; BLOCK];

    // Give the host a moment to enumerate and for you to attach a terminal.
    // Without this you miss the header every time.
    delay_blocking(&mut poller, &mut pit3, 3_000);

    log::info!("");
    log::info!("drum-engine cycle bench — {} tracks", TRACKS);
    log::info!("core {:.0} MHz, {} Hz, block {}", CORE_HZ / 1e6, SAMPLE_RATE, BLOCK);
    log::info!("budget {:.0} cycles per block", BUDGET);
    log::info!("");

    loop {
        led.toggle();

        // --- Idle: no track sounding, everything early-outs ---
        engine.panic();
        let idle = measure(&mut *engine, &mut left, &mut right, 0);
        report("idle    ", idle);

        // --- 3-voice sounding (the original 3-voice baseline; tracks 0,1,2) ---
        let sounding = measure(&mut *engine, &mut left, &mut right, 3);
        report("3 sounding", sounding);

        // --- 8-track worst case: every track retriggered continuously ---
        let all8 = measure(&mut *engine, &mut left, &mut right, TRACKS);
        report("8 sounding", all8);

        // --- Send-FX scenarios (Phase 5) ---
        // Two tracks send: snare (track 1) to reverb, clap (track 4) to
        // delay. This mirrors the renderer demo and is a realistic worst
        // case. The FX themselves are advanced every block regardless of
        // whether sends are routed, so "fx idle" isolates the per-block
        // bookkeeping cost of having FX in the engine.
        let fx_idle = measure_fxeffect_only(&mut *engine, &mut left, &mut right, TRACKS);
        report("8 FX idle ", fx_idle);

        let with_fx = measure_with_fx(&mut *engine, &mut left, &mut right);
        report("8 + FX    ", with_fx);

        log::info!("");
        poller.poll();
        delay_blocking(&mut poller, &mut pit3, 2_000);
    }
}

struct Stats {
    avg: u32,
    peak: u32,
}

/// Run the engine `RUNS` times and collect cycle counts. `retrigger_count`
/// keeps the first N tracks sounding — `0` for idle, `3` for the legacy
/// baseline, `TRACKS` for the worst case.
fn measure(
    engine: &mut DrumEngine,
    left: &mut [f32; BLOCK],
    right: &mut [f32; BLOCK],
    retrigger_count: usize,
) -> Stats {
    let mut total: u64 = 0;
    let mut peak: u32 = 0;

    // One untimed pass so the branch predictor and caches are warm. Without
    // it the first sample is an outlier that skews the peak.
    engine.process(left, right);

    for _ in 0..RUNS {
        if retrigger_count > 0 {
            let mut t = 0;
            while t < retrigger_count && t < TRACKS {
                engine.trigger(t, 1.0);
                t += 1;
            }
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
        // call, and you get a bench that reports four cycles. Reborrow
        // rather than move into `black_box`, so the next iteration still
        // has its buffers.
        core::hint::black_box(&*left);
        core::hint::black_box(&*right);
    }

    Stats {
        avg: (total / RUNS as u64) as u32,
        peak,
    }
}

/// Same as `measure` but with all 8 tracks retriggered and the send-FX
/// buses punched in: track 1 → reverb, track 4 → delay. This is the
/// realistic Phase 5 worst case: dry path + delay line + plate tank.
fn measure_with_fx(
    engine: &mut DrumEngine,
    left: &mut [f32; BLOCK],
    right: &mut [f32; BLOCK],
) -> Stats {
    // Punch in sends for tracks 1 and 4. `set_strip` refreshes the cache.
    // Snare (1) → reverb, Clap (4) → delay. The other tracks stay dry.
    engine.tracks[1].strip.send_reverb = 0.5;
    engine.tracks[4].strip.send_delay = 0.5;
    let s1 = engine.tracks[1].strip;
    let s4 = engine.tracks[4].strip;
    engine.tracks[1].set_strip(&s1);
    engine.tracks[4].set_strip(&s4);

    let mut total: u64 = 0;
    let mut peak: u32 = 0;

    engine.process(left, right);

    for _ in 0..RUNS {
        let mut t = 0;
        while t < TRACKS {
            engine.trigger(t, 1.0);
            t += 1;
        }

        let start = DWT::cycle_count();
        engine.process(left, right);
        let end = DWT::cycle_count();
        let elapsed = end.wrapping_sub(start);

        total += elapsed as u64;
        if elapsed > peak {
            peak = elapsed;
        }

        core::hint::black_box(&*left);
        core::hint::black_box(&*right);
    }

    Stats {
        avg: (total / RUNS as u64) as u32,
        peak,
    }
}

/// Same as `measure` for the 8-track case but isolates the FX cost when no
/// sends are routed. The FX buses are constructed every block (zeroed send
/// buffers + delay/reverb pre-process on silence) — this is the empty-FX
/// overhead an application that doesn't use sends still pays. It is
/// expected to be small.
fn measure_fxeffect_only(
    engine: &mut DrumEngine,
    left: &mut [f32; BLOCK],
    right: &mut [f32; BLOCK],
    retrigger_count: usize,
) -> Stats {
    let mut total: u64 = 0;
    let mut peak: u32 = 0;

    engine.process(left, right);

    for _ in 0..RUNS {
        if retrigger_count > 0 {
            let mut t = 0;
            while t < retrigger_count && t < TRACKS {
                engine.trigger(t, 1.0);
                t += 1;
            }
        }

        let start = DWT::cycle_count();
        engine.process(left, right);
        let end = DWT::cycle_count();
        let elapsed = end.wrapping_sub(start);

        total += elapsed as u64;
        if elapsed > peak {
            peak = elapsed;
        }

        core::hint::black_box(&*left);
        core::hint::black_box(&*right);
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