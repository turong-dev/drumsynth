//! Cycle budget harness — mi-drum (Plaits) device.
//!
//! The companion to `bench.rs`, measuring the *other* device. Same DWT
//! method, same output format, same `=== BENCH END ===` sentinel, so
//! `tools/benchloop.py` parses it without changes:
//!
//! ```text
//! tools/benchloop.py --bin mi-bench --label mi-baseline
//! ```
//!
//! # Why a separate binary
//!
//! Both engines in one image does not fit. `bench` is 301 KB of `.bss` and
//! the mi-drum image is 503 KB; together that is past what the Teensy's RAM
//! will take once the FX buffers and stack are accounted for. Keeping them
//! apart also means this file cannot perturb the numbers in
//! `bench-results/`, every one of which came from `bench.rs`.
//!
//! # Reading the output
//!
//! Six tracks, not eight — the mi-drum kit is six Plaits voices, limited by
//! RAM rather than by taste. So `6 sounding` is this device's worst case and
//! the number to compare against the ~70% ceiling; it is *not* directly
//! comparable to `bench`'s `8 sounding`, and dividing by tracks to compare
//! per-voice costs is the only fair reading.
//!
//! ```text
//! idle        nothing sounding, every voice early-outs
//! 3 sounding  tracks 0-2, for trend against the drum device
//! 6 sounding  worst case, every track retriggered every block
//! 6 FX idle   FX advanced with no sends routed — the bookkeeping floor
//! 6 + FX      worst case with delay + reverb, the realistic ceiling
//! ```
//!
//! # What this establishes
//!
//! Phase 13.5's gate ("bench under ~70%") was never runnable, because no
//! mi-drum scenario existed. This is that gate. It is also the reference the
//! Phase 14 stage-substitution work measures its per-sub-phase deltas
//! against, so treat a change in these numbers as a result, not noise — the
//! harness's run-to-run variance is about 4 cycles in 350,000.
//!
//! # A caveat specific to this device
//!
//! The Plaits voices are block-rate: `MiSlot` renders 24 samples at a time
//! and drips them out over `tick()`. `BLOCK` is 32, so a voice renders on
//! some blocks and not others, and the per-block cost is genuinely uneven in
//! a way the drum device's is not. `peak` is therefore the honest number
//! here even more than usual — `avg` understates what the audio callback has
//! to survive.

#![no_std]
#![no_main]

use teensy4_panic as _;

use cortex_m::peripheral::DWT;
use mi_drum_engine::{
    DeviceEngine, MiDrumEngine, SLOT_AD_FILTER_DEPTH, SLOT_AD_WARPS_DEPTH, SLOT_FILT_0,
    SLOT_LFO_FILTER_DEPTH, SLOT_LFO_WARPS_DEPTH, BLOCK, SAMPLE_RATE, TRACKS,
};
use teensy4_bsp as bsp;
use teensy4_bsp::board;

/// Nominal core clock. Used only to turn cycles into a percentage — if your
/// board is clocked differently, fix this or the percentages lie.
const CORE_HZ: f32 = 600_000_000.0;

/// Cycles available per block before the audio callback misses its deadline.
const BUDGET: f32 = CORE_HZ * BLOCK as f32 / SAMPLE_RATE;

/// Blocks per measurement run. Enough to average out cache warming.
///
/// Matches `bench.rs` so the two devices' numbers are gathered the same way.
/// It matters a little more here: at 24-sample voice blocks against 32-sample
/// engine blocks, the render phase only repeats every three engine blocks, so
/// a short run could sample the pattern unevenly.
const RUNS: usize = 512;

/// The engine lives in `.uninit` OCRAM, not DTCM.
///
/// `MiDrumEngine` is ~343 KB against 320 KB of DTCM, so unlike `bench.rs`'s
/// `DrumEngine` it cannot go there at all — the same constraint `bin/mi-drum.rs`
/// documents. With the L1 caches on that costs roughly 1.2%, which the
/// optimisation pass measured directly; with them off it would be
/// catastrophic. The `cache` feature is on by default and
/// `enable_cycle_counter` turns the caches on before any measurement, so the
/// numbers below are cached-OCRAM numbers. Flip the feature off and expect
/// them to roughly double.
#[link_section = ".uninit"]
static mut ENGINE_BUF: core::mem::MaybeUninit<MiDrumEngine> = core::mem::MaybeUninit::uninit();

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
    // On solid through init, toggling once the sweep is running. Dark means
    // the fault was before this line; solid-on forever means it was during
    // init; S.O.S. is `teensy4-panic`.
    led.set();

    let mut pit = pit;
    let mut poller = imxrt_log::log::usbd(usb, imxrt_log::Interrupts::Disabled)
        .expect("failed to bring up USB logging");

    // Enumerate before anything expensive runs.
    //
    // `Interrupts::Disabled` means the USB device only makes progress inside
    // `poller.poll()`, so until the first poll the board is absent from the
    // bus -- not silent on it, absent. Anything that faults between here and
    // that first poll therefore looks, from the host, like a board that
    // flashed, said `Booting`, and then never appeared: no port to open, no
    // output to read, and nothing to say which of the 343 KB of engine
    // construction below went wrong. `benchloop.py` reports it as "no
    // /dev/cu.usbmodem* appeared", which is true and useless.
    //
    // Polling first costs the 3 s the host needed anyway to attach a terminal
    // (it used to be spent after init, at the bottom of this block) and turns
    // that failure into a port that opens and then goes quiet, which is a
    // symptom with a name.
    delay_blocking(&mut poller, &mut pit, 3_000);
    log::info!("usb up; constructing the engine");
    delay_blocking(&mut poller, &mut pit, 50);

    enable_cycle_counter();

    // FPSCR.FZ — flush denormals to zero, matching `main.rs` and `bench.rs`.
    // The send-FX tanks decay freely below `DENORMAL_FLOOR`, and on the M7 VFP
    // a subnormal result takes the slow trap path (~20-30 cycles), which would
    // make "6 FX idle" and "6 + FX" diverge for reasons that have nothing to
    // do with the engine.
    #[allow(unsafe_code)]
    unsafe {
        let mut fpscr: u32;
        core::arch::asm!("vmrs {}, fpscr", out(reg) fpscr);
        fpscr |= 1 << 24; // FZ
        core::arch::asm!("vmsr fpscr, {}", in(reg) fpscr);
    }

    // SAFETY: `ENGINE_BUF` is `.uninit` OCRAM, written exactly once, here,
    // before interrupts are enabled — single-threaded init, the same
    // requirement `new_in_place` documents. Constructed through a raw pointer
    // rather than `&mut ENGINE_BUF` because Rust 2024 warns on mutable
    // references to mutable statics, and because a by-value
    // `MaybeUninit::write(MiDrumEngine::new())` would ask the optimiser to
    // elide a 343 KB move that the 16 KB DTCM stack cannot hold if it
    // declines.
    #[allow(unsafe_code)]
    let engine: &'static mut MiDrumEngine = unsafe {
        let p: *mut MiDrumEngine = core::ptr::addr_of_mut!(ENGINE_BUF).cast();
        MiDrumEngine::new_in_place(p)
    };

    let mut left = [0.0f32; BLOCK];
    let mut right = [0.0f32; BLOCK];

    // One shared resonator, standing in for a send-bus effect. Struck a third
    // of the way along, all 24 modes — the same configuration the per-track
    // spike used, so the two numbers are comparable.
    let mut res_send_fx = mi_drum_engine::stages::Resonator::new(0.3, 24);

    log::info!("engine ready");

    // The enumeration wait has already happened, above. This is only the
    // flush the report path needs between lines.
    delay_blocking(&mut poller, &mut pit, 50);

    log::info!("");
    log::info!("mi-drum cycle bench — {} tracks", TRACKS);
    log::info!(
        "core {:.0} MHz, {} Hz, block {}",
        CORE_HZ / 1e6,
        SAMPLE_RATE,
        BLOCK
    );
    log::info!("budget {:.0} cycles per block", BUDGET);
    log::info!("");

    loop {
        led.toggle();

        engine.panic();
        let idle = measure(engine, &mut left, &mut right, 0);
        report("idle    ", idle, &mut poller, &mut pit);

        let sounding = measure(engine, &mut left, &mut right, 3);
        report("3 sounding", sounding, &mut poller, &mut pit);

        let all = measure(engine, &mut left, &mut right, TRACKS);
        report("6 sounding", all, &mut poller, &mut pit);

        // Sends explicitly zeroed inside, so this measures FX-on-silence
        // regardless of what ran before it — `eff_send_*` survives
        // `panic()`/`reset()` and would otherwise leak between scenarios.
        let fx_idle = measure_fx_only(engine, &mut left, &mut right, TRACKS);
        report("6 FX idle ", fx_idle, &mut poller, &mut pit);

        let with_fx = measure_with_fx(engine, &mut left, &mut right);
        report("6 + FX    ", with_fx, &mut poller, &mut pit);

        // --- Phase 14 spike: one MI stage on all six tracks ---
        // The question these answer is whether the stage catalog is
        // affordable at all, given `6 sounding` already sits at 65.6% of a
        // ~70% ceiling. Each is the 6-track worst case with one stage
        // selected on every track, so the delta against `6 sounding` is the
        // whole cost of that stage times six.
        let lpg = measure_with_stage(engine, &mut left, &mut right, WARPS_ALGO_LPG);
        report("6 + LPG   ", lpg, &mut poller, &mut pit);

        let od = measure_with_stage(engine, &mut left, &mut right, WARPS_ALGO_OVERDRIVE);
        report("6 + DRIVE ", od, &mut poller, &mut pit);

        let res = measure_with_stage(engine, &mut left, &mut right, WARPS_ALGO_RESONATOR);
        report("6 + RESON ", res, &mut poller, &mut pit);

        // Heaviest Warps algorithm setting. If this does not fit, the fixed
        // strip is too expensive.
        let chain = measure_with_stage(engine, &mut left, &mut right, WARPS_ALGO_LPG_DRIVE);
        report("6 + WARP+DR", chain, &mut poller, &mut pit);

        // The resonator as a *shared send*, which is what it was always meant
        // to be: one instance fed by the tracks, not one per track. The
        // per-track scenario above measures six of them and is over budget by
        // 44 points; this measures the configuration that was actually
        // planned.
        let res_send = measure_resonator_send(engine, &mut left, &mut right, &mut res_send_fx);
        report("6 + RES SND", res_send, &mut poller, &mut pit);

        // --- Phase 14.3: Stages modulation on all six tracks ---
        // The strip already runs Warps + Ripples; this adds the four Stages
        // segment generators (2 LFOs + 2 AD envelopes) per track with all four
        // static routes at full depth, which is the 14.3 worst case. The delta
        // against `6 + WARP+DR` is the cost of the modulation bus.
        let mod_off = measure_modulation(engine, &mut left, &mut right, 0.0);
        report("6 + MOD off", mod_off, &mut poller, &mut pit);

        let mod_on = measure_modulation(engine, &mut left, &mut right, 1.0);
        report("6 + MOD on ", mod_on, &mut poller, &mut pit);

        log::info!("");

        // With `autoboot`, one sweep per flash: emit the sentinel the host
        // harness keys on, drain the log, then drop into HalfKay so the next
        // `teensy_loader_cli -w` catches the board with no button press.
        #[cfg(feature = "autoboot")]
        {
            log::info!("=== BENCH END ===");
            delay_blocking(&mut poller, &mut pit, 500);
            reboot_to_bootloader();
        }

        #[cfg(not(feature = "autoboot"))]
        {
            poller.poll();
            delay_blocking(&mut poller, &mut pit, 2_000);
        }
    }
}

/// Reboot into the Teensy 4 HalfKay bootloader via the MKL02's `bkpt #251`
/// watch. See `bench.rs` for why neither `teensy_loader_cli -s` nor `-r`
/// substitutes for this.
#[cfg(feature = "autoboot")]
fn reboot_to_bootloader() -> ! {
    #[allow(unsafe_code)]
    unsafe {
        core::arch::asm!("bkpt #251");
    }
    // Only reached if the bootloader chip did not take over. Spin rather than
    // returning: falling back into the report loop would look like a reboot
    // that worked and then came back.
    loop {
        core::hint::spin_loop();
    }
}

struct Stats {
    avg: u32,
    peak: u32,
}

/// Set a track's delay/reverb send levels, refreshing the engine's
/// `eff_send_*` caches via `set_strip`.
///
/// Writing `strip.send_*` directly is not enough: `process_dry_wet` reads the
/// cached values, which only `set_strip` (or the macro path) re-derives.
fn set_send(engine: &mut MiDrumEngine, track: usize, delay: f32, reverb: f32) {
    let tracks = engine.tracks_mut();
    tracks[track].strip.send_delay = delay;
    tracks[track].strip.send_reverb = reverb;
    let s = tracks[track].strip;
    tracks[track].set_strip(&s);
}

/// Retrigger the first `count` tracks.
fn retrigger(engine: &mut MiDrumEngine, count: usize) {
    let mut t = 0;
    while t < count && t < TRACKS {
        engine.trigger(t, 1.0);
        t += 1;
    }
}

/// Time `RUNS` blocks, retriggering the first `retrigger_count` tracks before
/// each. `0` for idle, `3` for the trend baseline, `TRACKS` for worst case.
fn measure(
    engine: &mut MiDrumEngine,
    left: &mut [f32; BLOCK],
    right: &mut [f32; BLOCK],
    retrigger_count: usize,
) -> Stats {
    // One untimed pass so the branch predictor and caches are warm. Matters
    // more here than on the drum device: the first pass also forces each
    // Plaits voice's first block render.
    engine.process(left, right);

    let mut total: u64 = 0;
    let mut peak: u32 = 0;

    for _ in 0..RUNS {
        retrigger(engine, retrigger_count);

        let start = DWT::cycle_count();
        engine.process(left, right);
        let end = DWT::cycle_count();

        // Wrapping subtract: CYCCNT is free-running 32-bit and rolls over
        // roughly every 7 seconds at 600 MHz.
        let elapsed = end.wrapping_sub(start);

        total += elapsed as u64;
        if elapsed > peak {
            peak = elapsed;
        }

        // Defeat dead-code elimination. Without this the optimiser may notice
        // nothing reads the buffers and delete the call, giving a bench that
        // reports four cycles.
        core::hint::black_box(&*left);
        core::hint::black_box(&*right);
    }

    Stats {
        avg: (total / RUNS as u64) as u32,
        peak,
    }
}

/// Worst case with the send-FX buses punched in: track 1 → reverb,
/// track 4 → delay, mirroring `bench.rs` so the two devices' FX scenarios
/// load the buses the same way.
///
/// Restores dry sends on the way out.
fn measure_with_fx(
    engine: &mut MiDrumEngine,
    left: &mut [f32; BLOCK],
    right: &mut [f32; BLOCK],
) -> Stats {
    set_send(engine, 1, 0.0, 0.5);
    set_send(engine, 4, 0.5, 0.0);

    let stats = measure(engine, left, right, TRACKS);

    set_send(engine, 1, 0.0, 0.0);
    set_send(engine, 4, 0.0, 0.0);

    stats
}

/// Warps algorithm values (SLOT_FILT_0). Phase 14.2 replaced the spike stage
/// selector with the Warps algorithm macro, so these now exercise different
/// cross-modulation algorithms rather than LPG/overdrive/resonator stages.
const WARPS_ALGO_NONE: f32 = 0.0;
const WARPS_ALGO_LPG: f32 = 0.25;
const WARPS_ALGO_OVERDRIVE: f32 = 0.45;
const WARPS_ALGO_RESONATOR: f32 = 0.65;
const WARPS_ALGO_LPG_DRIVE: f32 = 0.85;

/// Worst case with one Warps algorithm selected on every track.
///
/// Restores the bypass algorithm on the way out, so a later scenario in the
/// same loop iteration does not silently measure an algorithm it did not ask
/// for — the same discipline `measure_with_fx` applies to the sends.
fn measure_with_stage(
    engine: &mut MiDrumEngine,
    left: &mut [f32; BLOCK],
    right: &mut [f32; BLOCK],
    stage: f32,
) -> Stats {
    for t in 0..TRACKS {
        engine.tracks_mut()[t].set_macro(SLOT_FILT_0, stage);
    }

    let stats = measure(engine, left, right, TRACKS);

    for t in 0..TRACKS {
        engine.tracks_mut()[t].set_macro(SLOT_FILT_0, WARPS_ALGO_NONE);
    }

    stats
}

/// Worst case with the Stages modulation bus driven at `depth` on every track.
///
/// Sets all four static routes — LFO 1 to Ripples cutoff, LFO 2 to Warps
/// timbre, AD 1 to Ripples cutoff, AD 2 to Warps timbre — so this is the full
/// 14.3 modulation cost times six tracks. Restores zero depth on the way out
/// for the same reason `measure_with_stage` restores its algorithm.
fn measure_modulation(
    engine: &mut MiDrumEngine,
    left: &mut [f32; BLOCK],
    right: &mut [f32; BLOCK],
    depth: f32,
) -> Stats {
    const ROUTES: [usize; 4] = [
        SLOT_LFO_FILTER_DEPTH,
        SLOT_LFO_WARPS_DEPTH,
        SLOT_AD_FILTER_DEPTH,
        SLOT_AD_WARPS_DEPTH,
    ];
    for t in 0..TRACKS {
        for &slot in &ROUTES {
            engine.tracks_mut()[t].set_macro(slot, depth);
        }
    }

    let stats = measure(engine, left, right, TRACKS);

    for t in 0..TRACKS {
        for &slot in &ROUTES {
            engine.tracks_mut()[t].set_macro(slot, 0.0);
        }
    }

    stats
}

/// Worst case plus one shared resonator on a send bus.
///
/// The resonator runs once per engine block over `BLOCK` samples, fed the
/// master sum — which is what a send effect costs, as against the per-track
/// scenario's six instances each running over their own voice block. Both the
/// engine and the resonator are inside the timed region, so the delta against
/// `6 sounding` is the whole cost of adding it.
///
/// A real send would also pay the bus summing and the wet/dry mix, but
/// `SendFx` already does that for delay and reverb and the FX scenarios show
/// it costing essentially nothing. This isolates the resonator itself.
fn measure_resonator_send(
    engine: &mut MiDrumEngine,
    left: &mut [f32; BLOCK],
    right: &mut [f32; BLOCK],
    resonator: &mut mi_drum_engine::stages::Resonator,
) -> Stats {
    let mut wet = [0.0f32; BLOCK];

    engine.process(left, right);

    let mut total: u64 = 0;
    let mut peak: u32 = 0;

    for _ in 0..RUNS {
        retrigger(engine, TRACKS);

        let start = DWT::cycle_count();
        engine.process(left, right);
        // Mono send feed, as a send bus would be.
        resonator.process(0.01, 0.3, 0.5, 0.3, left, &mut wet);
        let end = DWT::cycle_count();
        let elapsed = end.wrapping_sub(start);

        total += elapsed as u64;
        if elapsed > peak {
            peak = elapsed;
        }

        core::hint::black_box(&*left);
        core::hint::black_box(&*right);
        core::hint::black_box(&wet);
    }

    Stats {
        avg: (total / RUNS as u64) as u32,
        peak,
    }
}

/// The FX bookkeeping floor: buses constructed and tanks advanced every block
/// with no sends routed. Expected to be small, and the baseline that makes
/// `6 + FX` legible.
fn measure_fx_only(
    engine: &mut MiDrumEngine,
    left: &mut [f32; BLOCK],
    right: &mut [f32; BLOCK],
    retrigger_count: usize,
) -> Stats {
    // Force dry regardless of what ran before.
    set_send(engine, 1, 0.0, 0.0);
    set_send(engine, 4, 0.0, 0.0);

    measure(engine, left, right, retrigger_count)
}

fn report(label: &str, s: Stats, poller: &mut imxrt_log::Poller, pit: &mut bsp::hal::pit::Pit) {
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

    // Push the line out before the next scenario. `imxrt-log`'s bbqueue is
    // 1024 bytes and is only drained by `poll()`; the scenarios run back to
    // back, so without this the buffer fills partway through and every later
    // line is dropped — including the sentinel the host harness keys on. The
    // measurement is unaffected: DWT brackets `process()` only.
    delay_blocking(poller, pit, 50);
}

/// Turn on the DWT cycle counter (and the L1 caches, under the `cache`
/// feature). The counter is gated behind the debug block, which is disabled
/// out of reset — skip `DCB::enable_trace` and `cycle_count()` returns a
/// constant zero, which looks exactly like an implausibly fast engine.
fn enable_cycle_counter() {
    let mut core = cortex_m::Peripherals::take().expect("core peripherals already taken");
    #[cfg(feature = "cache")]
    firmware::enable_caches(&mut core.SCB, &mut core.CPUID);
    core.DCB.enable_trace();
    DWT::unlock();
    core.DWT.enable_cycle_counter();
}

/// Block for roughly `ms`, pumping the USB poller so logs keep flowing.
fn delay_blocking(poller: &mut imxrt_log::Poller, pit: &mut bsp::hal::pit::Pit, ms: u32) {
    use bsp::hal::pit::Channel;
    const CHUNK_MS: u32 = 10;
    let ticks = (board::PERCLK_FREQUENCY / 1_000) * CHUNK_MS;

    for _ in 0..(ms / CHUNK_MS) {
        pit.set_load_timer_value(Channel::Chan0, ticks);
        pit.enable(Channel::Chan0);
        while !pit.is_elapsed(Channel::Chan0) {
            poller.poll();
        }
        pit.clear_elapsed(Channel::Chan0);
        pit.disable(Channel::Chan0);
    }
}
