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
//! 8+FX+CC   ...                                    (…plus a CC automation lane)
//! 8+FX+SWFX ...  (Phase 12: one track a sustained SweepFx, worst case)
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
//! `8+FX+CC` is the Phase 8-M regate: the first four scenarios are
//! steady-state, with macros parked at their values so `control()` takes its
//! single-branch early return every block. Real MIDI moves macros, and every
//! moving macro costs one coefficient recompute per block (the block-rate CC
//! smoother re-arms itself on every message). The regate drives `apply_cc` —
//! the exact main-loop API — every block on top of the full playing + FX
//! case, so the number it reports is what the Deluge actually asks for.
//!
//! `8 + BD VA` is the Phase 11 regate: track 0 swapped to the virtual-analogue
//! bridged-T kick, configured for its worst case — full pitch sweep, deep
//! decay, high Q — to expose `BridgedT::set_coeffs`'s per-sample divide + two
//! `fast::sin_turns` lookups. This is the path that could blow the budget if
//! Option A (per-sample retune) turns out wrong; the bench is what gates the
//! decision.
//!
//! `8+FX+SWFX` is the Phase 12 regate: the same 8-track + send-FX load, but
//! track 0 swapped to SweepFx sustaining its heaviest gesture — max sweep
//! depth, high resonance, HP mode. Two things are new in the budget model:
//! the per-sample `Svf::recalc` + `fast::exp2_approx` while the LFO moves the
//! cutoff (the SVF cost the strip only pays under modulation), and the fact
//! that a sustained machine defeats the per-track idle early-out for its whole
//! gesture — a kit with a sweep-FX track idles at "7 idle + 1 sounding" rather
//! than "8 idle".
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
use drum_engine::machines::{
    MachineId, SLOT_FILT_1, SLOT_MACH_1, SLOT_MACH_2, SLOT_MACH_5, SLOT_MACH_7,
};
use drum_engine::{DrumEngine, BLOCK, SAMPLE_RATE, TRACKS};
use teensy4_bsp as bsp;
use teensy4_bsp::board;

/// Nominal core clock. Used only to turn cycles into a percentage — if your
/// board is clocked differently, fix this or the percentages lie.
const CORE_HZ: f32 = 600_000_000.0;

/// The engine + its send-FX buffers (~256 KB for the delay line + reverb
/// tanks) live in DTCM via a plain `.bss` static, *not* on the stack: the
/// 16 KB stack configured by `t4link.x` cannot hold it, and Phase 5 grew
/// the engine past the point where a stack-allocated local was viable.
///
/// DTCM rather than the `.uninit` OCRAM this used to use. OCRAM sits behind
/// the AXI bus; DTCM is zero-wait-state, and `t4link.x` aliases `REGION_BSS`
/// to it. The engine is 269,056 bytes against 320 KB of DTCM, so it fits with
/// roughly 29 KB to spare once the 16 KB stack and the rest of `.bss` are
/// accounted for -- tight, but it is the linker that enforces it, and a build
/// that does not fit fails to link rather than misbehaving at run time.
/// Initialized below via [`DrumEngine::new_in_place`], not
/// `MaybeUninit::write(DrumEngine::new())` — see the comment at the call
/// site. Doing the write from a single-threaded `main` is sound here — the
/// bench is run-once with interrupts managed by `imxrt_log`.
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

    let mut pit = pit;
    let mut poller = imxrt_log::log::usbd(usb, imxrt_log::Interrupts::Disabled)
        .expect("failed to bring up USB logging");

    enable_cycle_counter();

    // FPSCR.FZ — flush denormals to zero, matching `main.rs`. The bench
    // otherwise leaves denormals in play, and the send-FX tanks are the one
    // place values decay freely below the engine's `DENORMAL_FLOOR`: on the
    // M7 VFP a subnormal result takes the slow trap path (~20-30 cycles), so
    // "8 FX idle" (tanks on exact zeros) vs "8 + FX" (tanks on real signal)
    // diverge by hundreds of cycles *solely because FZ is unset*. The
    // firmware never sees this — `main.rs` sets FZ at init.
    #[allow(unsafe_code)]
    unsafe {
        let mut fpscr: u32;
        core::arch::asm!("vmrs {}, fpscr", out(reg) fpscr);
        fpscr |= 1 << 24; // FZ
        core::arch::asm!("vmsr fpscr, {}", in(reg) fpscr);
    }

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
    delay_blocking(&mut poller, &mut pit, 3_000);

    log::info!("");
    log::info!("drum-engine cycle bench — {} tracks", TRACKS);
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

        // --- Idle: no track sounding, everything early-outs ---
        engine.panic();
        let idle = measure(&mut *engine, &mut left, &mut right, 0);
        report("idle    ", idle, &mut poller, &mut pit);

        // --- 3-voice sounding (the original 3-voice baseline; tracks 0,1,2) ---
        let sounding = measure(&mut *engine, &mut left, &mut right, 3);
        report("3 sounding", sounding, &mut poller, &mut pit);

        // --- 8-track worst case: every track retriggered continuously ---
        let all8 = measure(&mut *engine, &mut left, &mut right, TRACKS);
        report("8 sounding", all8, &mut poller, &mut pit);

        // --- Send-FX scenarios (Phase 5) ---
        // Two tracks send: snare (track 1) to reverb, clap (track 4) to
        // delay. This mirrors the renderer demo and is a realistic worst
        // case. The FX themselves are advanced every block regardless of
        // whether sends are routed, so "fx idle" isolates the per-block
        // bookkeeping cost of having FX in the engine.
        //
        // Each scenario sets and restores its own send state via `set_send`
        // — `eff_send_*` survives `panic()`/`reset()`, so without explicit
        // handling the first `measure_with_fx` would leave tracks 1/4
        // routed in every scenario of every later loop iteration.
        let fx_idle = measure_fxeffect_only(&mut *engine, &mut left, &mut right, TRACKS);
        report("8 FX idle ", fx_idle, &mut poller, &mut pit);

        let with_fx = measure_with_fx(&mut *engine, &mut left, &mut right);
        report("8 + FX    ", with_fx, &mut poller, &mut pit);

        // --- Phase 8-M regate: playing + FX + CC automation ---
        // Everything the groove box does at once: all 8 tracks retriggered,
        // sends routed, and a filter-macro automation lane moved every block
        // through `apply_cc`. This is the case the "must fit in the remaining
        // ~32%" gate is really about (see the first four scenarios, which are
        // steady-state and never exercise the CC recompute path).
        let with_cc = measure_with_cc_automation(&mut *engine, &mut left, &mut right);
        report("8+FX+CC   ", with_cc, &mut poller, &mut pit);

        // --- Phase 11 regate: 8 tracks with BdVa on track 0, worst case ---
        // Swap track 0 (BdClassic in the default kit) for BdVa configured
        // for its heaviest path — full pitch sweep (SLOT_MACH_1=1), deep
        // decay (SLOT_MACH_5=1), max Q (SLOT_FILT_1=1). This forces
        // `BridgedT::set_coeffs` to execute every sample with the maximum
        // pitch deflection (the full 0..120 Hz sweep range active), which
        // is the Option A path the Plan flagged as bench-gated. Every
        // other track keeps its default kit machine so the scenario is
        // comparable to "8 sounding" with one track substituted.
        let bdva = measure_with_bdva(&mut *engine, &mut left, &mut right);
        report("8 + BD VA ", bdva, &mut poller, &mut pit);

        // --- Phase 12 regate: one track sustaining a SweepFx gesture ---
        // The 8-track + send-FX load with track 0 swapped to SweepFx at its
        // heaviest sustained config. The gesture is long (6 s DEC), so the
        // machine stays `is_active()` across the whole measurement — the
        // sustained-load caveat the Phase 12 plan adds to the budget model.
        let swfx = measure_with_sweepfx_sustained(&mut *engine, &mut left, &mut right);
        report("8+FX+SWFX ", swfx, &mut poller, &mut pit);

        log::info!("");

        // With `autoboot`, one sweep per flash: emit a sentinel the host
        // harness can key on, drain the log, then drop into HalfKay so the
        // next `teensy_loader_cli -w` catches the board with no button press.
        // Without the feature the bench free-runs, which is what you want
        // when watching it in a terminal.
        #[cfg(feature = "autoboot")]
        {
            log::info!("=== BENCH END ===");
            // imxrt-log is a ring buffer drained by `poll()`. The sentinel has
            // to reach the host before the core stops executing, so pump the
            // poller for a while rather than rebooting straight away.
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

/// Reboot into the Teensy 4 HalfKay bootloader.
///
/// The Teensy 4.x carries a separate MKL02 chip as its bootloader. It watches
/// the i.MX RT's debug interface and takes over when the core executes
/// `bkpt #251` — the same mechanism Teensyduino's `_reboot_Teensyduino_` and
/// the `teensy4-selfrebootor` crate use.
///
/// This exists because `teensy_loader_cli`'s own reboot paths do not work
/// here: `-s` prints "Soft reboot is not implemented for OSX", and `-r` wants
/// a second Teensy running rebootor. `imxrt-log`'s USB backend discards every
/// host-to-device byte (`class.read_packet(&mut [])`), so there is no command
/// channel into this binary either.
#[cfg(feature = "autoboot")]
fn reboot_to_bootloader() -> ! {
    #[allow(unsafe_code)]
    unsafe {
        core::arch::asm!("bkpt #251");
    }
    // Only reached if the bootloader chip did not take over. Spin instead of
    // returning: falling back into the report loop would look like a reboot
    // that "worked" and then came back, which is the most confusing possible
    // failure mode for the host harness to diagnose.
    loop {
        core::hint::spin_loop();
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

/// Set a track's delay/reverb send levels, refreshing the engine's
/// `eff_send_*` caches via `set_strip`. Writing `strip.send_*` directly is
/// not enough — `process_dry_wet` reads the cached `eff_send_*` values, which
/// are only re-derived by `set_strip` (or the macro path). Every scenario
/// uses this so its send state is explicit and self-contained; without it a
/// prior scenario's sends leak into every later measurement.
fn set_send(engine: &mut DrumEngine, track: usize, delay: f32, reverb: f32) {
    engine.tracks[track].strip.send_delay = delay;
    engine.tracks[track].strip.send_reverb = reverb;
    let s = engine.tracks[track].strip;
    engine.tracks[track].set_strip(&s);
}

/// Same as `measure` but with all 8 tracks retriggered and the send-FX
/// buses punched in: track 1 → reverb, track 4 → delay. This is the
/// realistic Phase 5 worst case: dry path + delay line + plate tank.
///
/// Restores the default (dry) sends on the way out — `eff_send_*` otherwise
/// persists on the track, silently routing FX in every later scenario.
fn measure_with_fx(
    engine: &mut DrumEngine,
    left: &mut [f32; BLOCK],
    right: &mut [f32; BLOCK],
) -> Stats {
    // Punch in sends for tracks 1 and 4. `set_strip` refreshes the cache.
    // Snare (1) → reverb, Clap (4) → delay. The other tracks stay dry.
    set_send(engine, 1, 0.0, 0.5);
    set_send(engine, 4, 0.5, 0.0);

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

    set_send(engine, 1, 0.0, 0.0);
    set_send(engine, 4, 0.0, 0.0);

    Stats {
        avg: (total / RUNS as u64) as u32,
        peak,
    }
}

/// The Phase 8-M regate: playing + FX + CC automation, all at once.
///
/// The steady-state scenarios above park macros at their values, so
/// `control()` early-returns every block and the CC recompute path is never
/// exercised. Real MIDI moves macros, and every moving macro costs one
/// coefficient recompute per block — `set_macro_target` re-arms the
/// block-rate smoother on each message, so a lane that is driven every block
/// recomputes every block, forever.
///
/// This drives `apply_cc` — the exact main-loop API, routing through the
/// shared `midi` module just like the firmware and the host harness — on
/// track 0's filter-cutoff macro while all 8 tracks retrigger and the sends
/// stay routed, the composite case the "remaining ~32%" gate in the plan is
/// really about.
fn measure_with_cc_automation(
    engine: &mut DrumEngine,
    left: &mut [f32; BLOCK],
    right: &mut [f32; BLOCK],
) -> Stats {
    use drum_engine::midi::{apply_cc, CC_TRACK_BASE};

    // Punch in sends for tracks 1 and 4, mirroring `measure_with_fx`.
    set_send(engine, 1, 0.0, 0.5);
    set_send(engine, 4, 0.5, 0.0);

    let mut total: u64 = 0;
    let mut peak: u32 = 0;

    engine.process(left, right);

    // One filter-cutoff automation lane (macro 12 → CC 32 on the track's
    // channel). The value sweeps 0.1..0.9 in 1/127 steps — CC resolution,
    // bouncing at the ends — so the smoother is re-armed and mid-ramp on
    // every block, one recompute per block, exactly what a knob being spun
    // in real time does.
    let mut cc_val = 0.1f32;
    let mut rising = true;

    for _ in 0..RUNS {
        let mut t = 0;
        while t < TRACKS {
            engine.trigger(t, 1.0);
            t += 1;
        }

        apply_cc(engine, 0, CC_TRACK_BASE + 12, cc_val);
        cc_val += if rising { 1.0 / 127.0 } else { -(1.0 / 127.0) };
        if cc_val > 0.9 {
            cc_val = 0.9;
            rising = false;
        } else if cc_val < 0.1 {
            cc_val = 0.1;
            rising = true;
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

    set_send(engine, 1, 0.0, 0.0);
    set_send(engine, 4, 0.0, 0.0);

    Stats {
        avg: (total / RUNS as u64) as u32,
        peak,
    }
}

/// Phase 11 regate: track 0 swapped to `BdVa` configured for its heaviest
/// path — full sweep, deep decay, max Q. Exposes `BridgedT::set_coeffs`
/// running per-sample (Option A from `PLAN.md` Phase 11): two
/// `fast::sin_turns` lookups plus a divide every sample. The bench-gate
/// decision is whether this fits in the remaining headroom; if not, the
/// plan calls for splitting static/moving coefficients (Option B).
///
/// Restores the default kit on track 0 after the run so subsequent loops
/// see the unchanged engine.
fn measure_with_bdva(
    engine: &mut DrumEngine,
    left: &mut [f32; BLOCK],
    right: &mut [f32; BLOCK],
) -> Stats {
    // Save the original track-0 machine so we can restore it on the way out
    // — the next loop iteration expects the default kit.
    let original = engine.tracks[0].id();

    // Swap to BdVa and configure the worst case. Macros applied via
    // `set_macro` so the strip and the engine both see the change.
    engine.tracks[0].load_machine(MachineId::BdVa);
    engine.tracks[0].set_macro(SLOT_MACH_1, 1.0); // 0..120 Hz pitch deflection
    engine.tracks[0].set_macro(SLOT_MACH_2, 1.0); // 55 ms (sustains the sweep)
    engine.tracks[0].set_macro(SLOT_MACH_5, 1.0); // 1500 ms amp decay
    engine.tracks[0].set_macro(SLOT_FILT_1, 1.0); // Q = 10 (max resonance)

    let mut total: u64 = 0;
    let mut peak: u32 = 0;

    engine.process(left, right);

    for _ in 0..RUNS {
        // All 8 tracks retriggered — same load as the "8 sounding" scenario
        // so the delta vs that row is exactly the BdVa-per-sample cost on
        // track 0, no other variable.
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

    // Restore the original track-0 machine so the next loop's scenarios see
    // the unmodified kit.
    engine.tracks[0].load_machine(original);

    Stats {
        avg: (total / RUNS as u64) as u32,
        peak,
    }
}

/// The Phase 12 regate: the 8-track + send-FX load, but track 0 holds a
/// SweepFx machine sustaining its heaviest gesture.
///
/// The sustained-load caveat the Phase 12 plan adds to the budget model: a
/// sustained machine defeats the per-track idle early-out for its whole
/// gesture, so a kit with a sweep-FX track idles at "7 idle + 1 sounding"
/// rather than "8 idle". This scenario measures that number directly — all 8
/// tracks active, track 0 running the worst-case SVF path (max sweep depth,
/// high resonance, HP mode, the per-sample `Svf::recalc` + `exp2_approx` the
/// strip only pays under modulation) while the sends are routed.
///
/// The gesture is long (max DEC ≈ 6 s), so track 0 stays `is_active()` for
/// the whole run rather than retriggering — the engine's `trigger` on that
/// track would otherwise just restart the AHD and the measurement would
/// silently fall back to the one-shot load.
fn measure_with_sweepfx_sustained(
    engine: &mut DrumEngine,
    left: &mut [f32; BLOCK],
    right: &mut [f32; BLOCK],
) -> Stats {
    // Save the original track-0 machine so we can restore it on the way out.
    let original = engine.tracks[0].id();

    // Swap to SweepFx at its heaviest sustained config: max sweep depth
    // (DEPTH=1, 4 octaves), high resonance (RESO=1, Q≈8), HP mode
    // (MODE=1), start cutoff at the top of its range (START=1, 8 kHz),
    // and a long gesture (DEC=1, ≈6 s) so it never idles mid-run.
    engine.tracks[0].load_machine(MachineId::SweepFx);
    engine.tracks[0].set_macro(SLOT_MACH_1, 1.0); // DEPTH 4 oct
    engine.tracks[0].set_macro(SLOT_FILT_1, 1.0); // RESO Q≈8
    engine.tracks[0].set_macro(SLOT_MACH_7, 1.0); // MODE HP
    engine.tracks[0].set_macro(SLOT_MACH_2, 1.0); // START 8 kHz
    engine.tracks[0].set_macro(SLOT_MACH_5, 1.0); // DEC ≈6 s gesture

    // Punch in the same sends as the other FX scenarios: snare (1) →
    // reverb, clap (4) → delay.
    set_send(engine, 1, 0.0, 0.5);
    set_send(engine, 4, 0.5, 0.0);

    let mut total: u64 = 0;
    let mut peak: u32 = 0;

    engine.process(left, right);

    // Start the sustained gesture once. It must not be retriggered in the
    // loop — a retrigger would restart the AHD and the machine would run
    // its attack rather than its long hold, defeating the sustained load
    // this scenario exists to measure.
    engine.tracks[0].trigger(1.0);

    for _ in 0..RUNS {
        // Track 0 sustains (not retriggered); the other seven retrigger
        // every block as in the "8 sounding" scenarios.
        let mut t = 1;
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

    // Restore the kit and the dry sends.
    engine.tracks[0].load_machine(original);
    set_send(engine, 1, 0.0, 0.0);
    set_send(engine, 4, 0.0, 0.0);

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
///
/// Explicitly zeroes the sends first. Without this, a previous `measure_with_fx`
/// call leaves `eff_send_*` nonzero on tracks 1/4 and this scenario silently
/// measures FX on real signal instead of on silence.
fn measure_fxeffect_only(
    engine: &mut DrumEngine,
    left: &mut [f32; BLOCK],
    right: &mut [f32; BLOCK],
    retrigger_count: usize,
) -> Stats {
    // Force the strips dry (and refresh the `eff_send_*` caches) so this
    // scenario measures FX-on-silence regardless of what ran before it.
    set_send(engine, 1, 0.0, 0.0);
    set_send(engine, 4, 0.0, 0.0);

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

fn report(
    label: &str,
    s: Stats,
    poller: &mut imxrt_log::Poller,
    pit: &mut bsp::hal::pit::Pit,
) {
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

    // Push the line out before running the next scenario.
    //
    // `imxrt-log`'s bbqueue is 1024 bytes (`IMXRT_LOG_BUFFER_SIZE`) and is
    // only drained by `poll()`. The scenarios run back to back with nothing
    // else calling the poller, so without this the buffer fills partway
    // through the run and every later line is silently dropped — including
    // the `=== BENCH END ===` sentinel `tools/benchloop.py` keys on. The
    // measurement itself is unaffected: DWT brackets `engine.process()` only,
    // and this runs well outside it.
    delay_blocking(poller, pit, 50);
}

/// Turn on the DWT cycle counter.
///
/// It is gated behind the debug block, which is disabled out of reset, so
/// both steps are required. Skip the `DCB` enable and `cycle_count()` returns
/// a constant zero, which looks exactly like an implausibly fast engine.
fn enable_cycle_counter() {
    let mut core = cortex_m::Peripherals::take().expect("core peripherals already taken");
    #[cfg(feature = "cache")]
    firmware::enable_caches(&mut core.SCB, &mut core.CPUID);
    core.DCB.enable_trace();
    // The DWT block carries the lock-access register; writing the key is a
    // no-op on cores whose DWT is not locked.
    DWT::unlock();
    core.DWT.enable_cycle_counter();
}

/// Block for roughly `ms`, pumping the USB poller so logs keep flowing.
fn delay_blocking(poller: &mut imxrt_log::Poller, pit: &mut bsp::hal::pit::Pit, ms: u32) {
    use bsp::hal::pit::Channel;
    // PIT runs from the perclk root; board::PERCLK_FREQUENCY is the divisor
    // to use. Chunked so the poller runs often enough that USB does not stall.
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
