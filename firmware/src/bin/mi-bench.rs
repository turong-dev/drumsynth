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
//! # Decomposing the strip
//!
//! `6 sounding` is 290% of budget, and the Phase 14 per-unit table cannot say
//! which term is responsible: of its four strip terms, only the Stages
//! segment was ever measured. These subtract one component at a time, each
//! using a knob that already ships, so a delta is that component's whole
//! cost at six tracks:
//!
//! ```text
//! 1 sounding  }  scaling curve. Per-track cost is flat if the engine is
//! 2 sounding  }  compute-bound and rises if it is cache-bound.
//! 4 sounding     tracks 0-3 = the four Peaks drums of DEFAULT_KIT, so
//!                `6 - 4` is the two Plaits voices and `(4 - idle) / 4`
//!                is one Peaks voice with the strip on it
//! 6 no SHAPE     SHAPE.DRV = 0 is a bypass detent, so
//!                `6 sounding - 6 no SHAPE` is the shaping stage's cost
//! ```
//!
//! **What is still not reachable from here.** The four Stages generators
//! (2 LFO + 2 AD) run unconditionally in `MiSlot`, every block, every track,
//! whatever the route depths are — see `lib.rs`'s
//! `process_free_running`/`process` calls. No param gates them, so no
//! scenario here can subtract them, and their cost stays folded into
//! `6 sounding`.
//!
//! That is worth more than a measurement gap. Every route depth defaults to
//! 0, so a stock kit already runs four segment generators per track and
//! multiplies all four outputs by zero. An early-out when every depth is
//! zero would be both the missing instrumentation and a real saving, but it
//! is an engine change rather than a bench one.
//!
//! One number this harness has already pinned without a new scenario: the
//! `libm::powf` in the Ripples cutoff loop. It is per sample, per track,
//! ungated, and `6 + MOD on` − `6 + MOD off` isolates it at **407,257
//! cycles per block — 102% of the whole budget**, 2,121 cycles across each
//! of 192 calls. At depth 0 the exponent is 0 and libm takes its fast path,
//! which is why `6 + MOD off` and `6 sounding` agree and why the difference
//! is that clean.
//!
//! # What this establishes
//!
//! Phase 13.5's gate ("bench under ~70%") was never runnable, because no
//! mi-drum scenario existed. This is that gate. It is also the reference the
//! Phase 14 stage-substitution work measures its per-sub-phase deltas
//! against.
//!
//! # How much of a delta is a result
//!
//! This used to say the run-to-run variance was "about 4 cycles in 350,000",
//! and treat anything above that as a result. That was true when it was
//! written and is not true now. Two back-to-back runs of the same binary
//! (`bench-results/mi-phase14.json` and `mi-phase14-recheck.json`) differ by
//! up to **3.7%**:
//!
//! ```text
//!   idle            31,119 ->    31,119   0.00%
//!   6 sounding   1,170,735 -> 1,160,063   0.91%
//!   6 + MOD off  1,133,801 -> 1,165,719   2.82%
//!   6 + LPG        728,638 ->   755,451   3.68%
//! ```
//!
//! `idle` is still bit-exact, so the harness itself has not become sloppy.
//! What changed is the engine: at 476 KB it sits in OCRAM at 94% occupancy
//! against a 32 KB L1 D-cache, so the hit rate now depends on where in the
//! engine each scenario's working set happens to land. Below about 4%, a
//! difference in a sounding scenario is cache luck and not a result. Measure
//! twice before believing one.
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
    seed_random, DeviceEngine, MiDrumEngine, DEFAULT_RANDOM_SEED, SLOT_AD_FILTER_DEPTH,
    SLOT_AD_SHAPER_DEPTH, SLOT_FILT_0, SLOT_LFO_FILTER_DEPTH, SLOT_LFO_SHAPER_DEPTH,
    SLOT_SHAPER_DRIVE, BLOCK, SAMPLE_RATE, TRACKS,
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

    // Caches and the cycle counter before the first `poller.poll()`, which is
    // the order `bench.rs` runs in and the only order this device has ever
    // been known to work in.
    //
    // `enable_dcache` invalidates the whole D-cache and then starts holding
    // CPU writes in it, and the USB controller is a DMA master reading its
    // descriptors out of memory. On this linker script that is probably
    // harmless -- `imxrt_log`'s `ENDPOINT_MEMORY`, `ENDPOINT_STATE`, `BUFFER`
    // and `BACKEND` all land in `.bss`, which `build.rs` puts in DTCM, and TCM
    // is never cached -- but "probably harmless" is not a reason to enable a
    // cache underneath a live DMA master when the alternative is free.
    //
    // It also makes the enumeration below a clean probe: if the board never
    // appears on USB at all, the fault is in these two steps rather than in
    // anything after them.
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

    // Enumerate before anything expensive runs.
    //
    // `Interrupts::Disabled` means the USB device only makes progress inside
    // `poller.poll()`, so until the first poll the board is absent from the
    // bus -- not silent on it, absent. Anything that faults between here and
    // that first poll therefore looks, from the host, like a board that
    // flashed, said `Booting`, and then never appeared: no port to open, no
    // output to read, and nothing to say which of the 476 KB of engine
    // construction below went wrong. `benchloop.py` reports it as "no
    // /dev/cu.usbmodem* appeared", which is true and useless.
    //
    // Polling first costs the 3 s the host needed anyway to attach a terminal
    // (it used to be spent after init, at the bottom of this block) and turns
    // that failure into a port that opens and then goes quiet, which is a
    // symptom with a name.
    delay_blocking(&mut poller, &mut pit, 3_000);
    log::info!("usb up, caches on; constructing the engine");
    delay_blocking(&mut poller, &mut pit, 50);

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

    log::info!("engine constructed");
    delay_blocking(&mut poller, &mut pit, 50);

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

        // --- Decomposition: where the strip's cycles actually go ---
        //
        // `6 sounding` is 4.4x its September value and 290% of budget, and
        // the Phase 14 per-unit table cannot say which term is responsible
        // because three of its four strip terms were never measured. These
        // scenarios subtract one thing at a time using knobs that already
        // exist, so each delta is a component's whole cost at six tracks.

        // Scaling curve. If the engine were compute-bound, per-track cost
        // would be flat in the track count. It is not: 3 tracks cost
        // ~148,700 each and 6 cost ~188,200 each, +26% for identical work.
        // One track's slot state is 33,328 B (Plaits 12,288 + Warps 4,112 +
        // four Stages 16,736 + Peaks 196) against a 32 KB L1 D-cache, so a
        // single track already evicts itself and the engine streams from
        // OCRAM. These two fill in the bottom of that curve: if 1 and 2 are
        // much cheaper per track than 3 and 6, the cliff is locality and no
        // amount of per-unit arithmetic will predict it.
        let one = measure(engine, &mut left, &mut right, 1);
        report("1 sounding", one, &mut poller, &mut pit);

        let two = measure(engine, &mut left, &mut right, 2);
        report("2 sounding", two, &mut poller, &mut pit);

        // Tracks 0-3 of `DEFAULT_KIT` are the four Peaks drums; 4 and 5 are
        // Plaits (String, Modal). `retrigger` takes the first `n` tracks, so
        // `6 sounding - 4 sounding` is exactly the two Plaits voices and
        // `(4 sounding - idle) / 4` is one Peaks voice carrying the full
        // strip. Phase 14 estimated a Peaks voice at 5,000-15,000 cycles
        // against a *measured* 38,900 for Plaits, and that estimate is what
        // the six-track shape was signed off on.
        let peaks_only = measure(engine, &mut left, &mut right, 4);
        report("4 sounding", peaks_only, &mut poller, &mut pit);

        // The shaping stage, subtracted. `SHAPE.DRV = 0` is a discrete bypass
        // detent rather than a gain of zero: `shaper_drive_from_param` returns
        // `(true, 0.0)` and `Shaper::process_dual` copies its input and
        // returns. So `6 sounding - 6 no SHAPE` is the stage's whole cost,
        // with the strip's own scaffolding -- the dry copy, the modulator
        // source, the `SHAPE.MIX` blend -- left exactly where it was.
        //
        // This is the number that retired Warps. Measured the same way,
        // `warps::Modulator` cost 79,855 cycles per track; Phase 14 had
        // estimated 2,000-5,000. See `docs/warps-vendoring.md`.
        let no_shape = measure_without_shaper(engine, &mut left, &mut right);
        report("6 no SHAPE", no_shape, &mut poller, &mut pit);

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
        // Warps algorithm sweep: one point per integral entry of the xmod
        // table, against `6 no WARPS` as the floor. The question these answer
        // is whether a cheaper algorithm is worth reaching for -- and how much
        // of Warps' cost an algorithm choice can reach at all, given the
        // oversampling underneath it is the same whichever entry is selected.
        for &(label, value) in &SHAPER_ALGO_SWEEP {
            let s = measure_with_stage(engine, &mut left, &mut right, value);
            report(label, s, &mut poller, &mut pit);
        }

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
    /// Median block. With `peak` and `p99`, this is what separates "one
    /// freak block" from "every third block is expensive".
    p50: u32,
    p99: u32,
    min: u32,
    /// Blocks at or over the deadline. The number that actually says
    /// whether the device misses it, which `peak` alone cannot:
    /// `over == 1` is a measurement artefact to go and find, `over == 170`
    /// is a device that does not fit.
    over: u32,
    /// Mean block cost grouped by block index mod [`RENDER_PERIOD`].
    ///
    /// The direct test of the render-boundary hypothesis. A slot renders
    /// `VOICE_BLOCK` = 24 samples at a time into a `BLOCK` = 32 engine
    /// block, so over three engine blocks it renders four times: two
    /// renders land in one block and one in each of the others. If that
    /// is what makes the tail, cost is periodic with period 3 and these
    /// three numbers differ. If they are equal, the variation is coming
    /// from somewhere else and the boundary theory is wrong.
    phase: [u32; RENDER_PERIOD],
}

/// Engine blocks per voice-render cycle: `lcm(VOICE_BLOCK, BLOCK) / BLOCK`
/// = `lcm(24, 32) / 32` = 3.
const RENDER_PERIOD: usize = 3;

/// Per-block samples for one scenario, kept so the distribution can be
/// reported rather than just its two extremes.
///
/// `peak` is a max over [`RUNS`] blocks, so a single contaminated block
/// defines it while leaving `avg` untouched — which is exactly the
/// signature mi-drum shows (avg repeats to 0.7%, peak to only 8%). Keeping
/// every sample is 2 KB of stack and turns that ambiguity into a number.
struct Samples {
    cy: [u32; RUNS],
    n: usize,
}

impl Samples {
    const fn new() -> Self {
        Self {
            cy: [0; RUNS],
            n: 0,
        }
    }

    fn push(&mut self, elapsed: u32) {
        if self.n < RUNS {
            self.cy[self.n] = elapsed;
            self.n += 1;
        }
    }

    /// Sort in place and reduce to [`Stats`].
    ///
    /// `sort_unstable` is `core`, not `alloc` — pattern-defeating quicksort
    /// over a borrowed slice, no scratch allocation — so this stays inside
    /// the no-alloc contract.
    fn finish(&mut self) -> Stats {
        let n = self.n;
        if n == 0 {
            return Stats {
                avg: 0,
                peak: 0,
                p50: 0,
                p99: 0,
                min: 0,
                over: 0,
                phase: [0; RENDER_PERIOD],
            };
        }
        let total: u64 = self.cy[..n].iter().map(|&c| c as u64).sum();
        let budget = BUDGET as u32;
        let over = self.cy[..n].iter().filter(|&&c| c >= budget).count() as u32;

        // Group by position in the render cycle *before* sorting, which
        // destroys the ordering this depends on.
        let mut phase = [0u32; RENDER_PERIOD];
        let mut sums = [0u64; RENDER_PERIOD];
        let mut counts = [0u32; RENDER_PERIOD];
        for (i, &c) in self.cy[..n].iter().enumerate() {
            sums[i % RENDER_PERIOD] += c as u64;
            counts[i % RENDER_PERIOD] += 1;
        }
        for k in 0..RENDER_PERIOD {
            if counts[k] > 0 {
                phase[k] = (sums[k] / counts[k] as u64) as u32;
            }
        }

        let s = &mut self.cy[..n];
        s.sort_unstable();
        Stats {
            avg: (total / n as u64) as u32,
            peak: s[n - 1],
            p50: s[n / 2],
            // Nearest-rank, clamped: at RUNS = 512 this is the 507th sample,
            // so it ignores the top five blocks.
            p99: s[(n * 99 / 100).min(n - 1)],
            min: s[0],
            over,
            phase,
        }
    }
}

/// Set a track's delay/reverb send levels, refreshing the engine's
/// `eff_send_*` caches via `set_strip`.
///
/// Writing `strip.send_*` directly is not enough: `process_dry_wet` reads the
/// cached values, which only `set_strip` (or the param path) re-derives.
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
    // Silence whatever the previous scenario left behind, so this one
    // measures its own `retrigger_count` and not someone else's tail.
    //
    // Without this the scenarios contaminate each other, and the Phase 14
    // gate work made it much worse: a triggered voice now holds its gate open
    // until a note-off or the watchdog, so a track that `retrigger` stops
    // touching keeps sounding into the next scenario instead of decaying out
    // of it. The first run of the decomposition scenarios caught it -- the
    // track count went 3 < 2 < 1 in cost, and `1 sounding` had a peak/avg
    // ratio of 1.95x against 1.10-1.28x everywhere else, which is what a
    // scenario inheriting five ringing voices looks like.
    //
    // `panic()` resets every track and the send-FX tanks. Params, strip
    // params and `eff_send_*` all survive it (`Track::reset` touches voice
    // state only), so the scenarios that set those before calling in here are
    // unaffected -- see `measure_fx_only`, which already relied on that.
    engine.panic();

    // Reseed the noise generator every scenario.
    //
    // `stmlib::Random` is one process-global LCG shared by every Plaits
    // engine, and nothing in the firmware ever seeded it, so where a
    // scenario started in the sequence depended on how many draws the
    // scenarios before it had made -- which depends on how many voices
    // were sounding, which is the thing being measured. Seeding here,
    // after the reset and before the warm-up pass, removes that coupling:
    // each scenario renders the same noise whatever ran before it.
    //
    // Worth being precise about what this did *not* fix, because the
    // issue that prompted it (#1, finding 2) guessed wrong. The symptom
    // was the stochastic scenarios swinging ~5% between runs of an
    // identical binary, and that was blamed on carried-over generator
    // state. It is not: measured over three runs, per-scenario **avg**
    // varies by at most 0.68% and **peak** by up to 7.70%, both before
    // and after this call. The work each scenario does was always
    // repeatable; it is the peak statistic that is not. See the
    // avg-versus-peak section of BENCHMARKS.md.
    //
    // Seeding stays because the coupling it removes is real and a
    // scenario that renders different audio depending on its position in
    // the sweep is not measuring one thing. It just was not the cause.
    seed_random(DEFAULT_RANDOM_SEED);

    // One untimed pass so the branch predictor and caches are warm. Matters
    // more here than on the drum device: the first pass also forces each
    // Plaits voice's first block render.
    engine.process(left, right);

    let mut samples = Samples::new();

    for _ in 0..RUNS {
        retrigger(engine, retrigger_count);

        let start = DWT::cycle_count();
        engine.process(left, right);
        let end = DWT::cycle_count();

        // Wrapping subtract: CYCCNT is free-running 32-bit and rolls over
        // roughly every 7 seconds at 600 MHz.
        samples.push(end.wrapping_sub(start));

        // Defeat dead-code elimination. Without this the optimiser may notice
        // nothing reads the buffers and delete the call, giving a bench that
        // reports four cycles.
        core::hint::black_box(&*left);
        core::hint::black_box(&*right);
    }

    samples.finish()
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

/// The restore value for `SLOT_FILT_0` — also the shipped default, and (see
/// below) the most expensive entry in the table.
const SHAPER_ALGO_NONE: f32 = 0.0;

/// One param value per entry of the shaper's algorithm table.
///
/// `Shaper::set_parameters` scales the param across `ALGORITHM_COUNT` and
/// blends adjacent entries, so `i / (COUNT - 1)` lands on entry `i`.
///
/// These used to be six points named after Warps' `xmod_table_` pairs, which
/// stopped meaning anything twice over: 14.2 had already repointed
/// `SLOT_FILT_0` from the spike stage selector at the algorithm, and the
/// stage is no longer Warps. Four entries now, named for what they are.
const SHAPER_ALGO_SWEEP: [(&str, f32); 4] = [
    ("ALG0 xfade ", 0.0),       // equal-power blend of the two inputs
    ("ALG1 fold  ", 1.0 / 3.0), // ADAA sine folder
    ("ALG2 diode ", 2.0 / 3.0), // ADAA diode ring, Warps' analog model
    ("ALG3 ring  ", 1.0),       // digital ring, band-limited modulator
];

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
        engine.tracks_mut()[t].set_param(SLOT_FILT_0, stage);
    }

    let stats = measure(engine, left, right, TRACKS);

    for t in 0..TRACKS {
        engine.tracks_mut()[t].set_param(SLOT_FILT_0, SHAPER_ALGO_NONE);
    }

    stats
}

/// Worst case with the shaping stage bypassed on every track.
///
/// `SLOT_SHAPER_DRIVE` is `SHAPE.DRV` on this device, at the index the drum
/// device calls `SLOT_STRIP_HOLD`. It used to be *reached* through that name
/// — phase 14 built the fixed Warps -> Ripples strip on the strip AHD's four
/// slots and borrowed them — so this read as if it were reaching for a
/// bypassed strip. Phase E′ gave it its own constant at the same index.
///
/// Restores the default drive (0.2, `SHAPER_DRIVE_INFO`) on the way out, so a
/// later scenario in the same loop iteration does not silently measure a
/// bypassed strip -- the discipline `measure_with_stage` applies to its
/// algorithm and `measure_with_fx` to its sends.
fn measure_without_shaper(
    engine: &mut MiDrumEngine,
    left: &mut [f32; BLOCK],
    right: &mut [f32; BLOCK],
) -> Stats {
    for t in 0..TRACKS {
        engine.tracks_mut()[t].set_param(SLOT_SHAPER_DRIVE, SHAPER_DRIVE_BYPASS);
    }

    let stats = measure(engine, left, right, TRACKS);

    for t in 0..TRACKS {
        engine.tracks_mut()[t].set_param(SLOT_SHAPER_DRIVE, SHAPER_DRIVE_DEFAULT);
    }

    stats
}

/// `SHAPE.DRV` values: the bypass detent, and the shipped default.
const SHAPER_DRIVE_BYPASS: f32 = 0.0;
const SHAPER_DRIVE_DEFAULT: f32 = 0.2;

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
        SLOT_LFO_SHAPER_DEPTH,
        SLOT_AD_FILTER_DEPTH,
        SLOT_AD_SHAPER_DEPTH,
    ];
    for t in 0..TRACKS {
        for &slot in &ROUTES {
            engine.tracks_mut()[t].set_param(slot, depth);
        }
    }

    let stats = measure(engine, left, right, TRACKS);

    for t in 0..TRACKS {
        for &slot in &ROUTES {
            engine.tracks_mut()[t].set_param(slot, 0.0);
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

    let mut samples = Samples::new();

    for _ in 0..RUNS {
        retrigger(engine, TRACKS);

        let start = DWT::cycle_count();
        engine.process(left, right);
        // Mono send feed, as a send bus would be.
        resonator.process(0.01, 0.3, 0.5, 0.3, left, &mut wet);
        let end = DWT::cycle_count();
        samples.push(end.wrapping_sub(start));

        core::hint::black_box(&*left);
        core::hint::black_box(&*right);
        core::hint::black_box(&wet);
    }

    samples.finish()
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

    // The distribution, on its own line so the existing one keeps its
    // format. This is the line that says what the peak/average tail is
    // made of: a `p99` near `p50` with a high `max` is a handful of
    // contaminated blocks, while a `p99` near `max` is the workload.
    log::info!(
        "  dist min={} p50={} p99={} max={} over={}/{}",
        s.min,
        s.p50,
        s.p99,
        s.peak,
        s.over,
        RUNS,
    );
    log::info!(
        "  phase p0={} p1={} p2={}",
        s.phase[0],
        s.phase[1],
        s.phase[2],
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
