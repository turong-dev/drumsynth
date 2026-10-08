//! Host-side renderer.
//!
//! This is where you decide what things should sound like. It links the same
//! `drum-engine` crate the firmware does, so anything you tune here is
//! already validated by the time it reaches hardware.
//!
//! Modes:
//!
//! ```text
//! render   — write a demo pattern to a WAV file
//! sweep    — write one WAV per value of a parameter, for A/B-ing
//! play     — real-time playback (requires --features live)
//! device   — MIDI-in / audio-out device (requires --features live)
//! monitor  — MIDI input monitor (requires --features live)
//! ```
//!
//! The sweep mode is the one that earns its keep. Rendering sixteen kicks
//! with decay times from 100ms to 800ms takes a fraction of a second, and
//! flipping between them in an editor is a much faster way to find the right
//! one than turning a knob in real time.
//!
//! Performance counters are taken against a generic 8-track kit on track 0:
//! since machines are normalised over the same flat 32-macro map, you sweep
//! any knob of any machine the same way — `<machine> <macro-name> --from
//! --to --steps`.

use clap::Parser;
use drum_engine::{DrumEngine, SAMPLE_RATE};

#[cfg(feature = "live")]
use drum_engine::{BLOCK, TRACKS};

mod cheatsheet;
mod cli;
mod measure;
mod render;
mod shaper_probe;
mod verify;
mod wav;

/// The MIDI-in / audio-out device mode. Behind the `live` feature with the
/// rest of the host audio stack.
#[cfg(feature = "live")]
mod device;

/// The MIDI input monitor. Also behind the `live` feature because it needs
/// `midir` for port enumeration and connection.
#[cfg(feature = "live")]
mod monitor;
#[cfg(feature = "live")]
mod slack;

use cheatsheet::generate_cheatsheet;
use cli::{CheatsheetFormat, Cli, Command, MachineArg};
use render::{
    print_gate_timeline, render_gate_demo_drum, render_gate_demo_mi, render_mi_drum_marked,
    render_one_shot, render_pattern, render_shaper_alias_test, render_shaper_kit_comparison,
    render_stress, render_warps_drive_demo, render_warps_kit_comparison, Pattern,
};
use wav::write_wav;

#[cfg(feature = "live")]
use render::setup_kit_mix;

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let cli = Cli::parse();

    match cli.command {
        Command::Render { output, bpm, bars } => {
            check_bpm(bpm)?;
            let samples = render_pattern(&Pattern::demo(), bpm, bars);
            write_wav(&output, &samples)?;
            let seconds = samples.len() as f32 / 2.0 / SAMPLE_RATE;
            println!("wrote {output} ({seconds:.2}s)");
        }

        Command::MiDrum { output, stage } => {
            // Phase 14.2 replaced the spike stage selector with the Warps
            // algorithm on SLOT_FILT_0. Keep the CLI argument name but map it
            // to the new chain.
            let warps_algorithm = match stage.as_str() {
                "none" => 0.0,
                "lpg" => 0.25,
                "overdrive" => 0.5,
                "resonator" => 0.75,
                other => {
                    return Err(format!(
                        "unknown stage '{other}'. available: none, lpg, overdrive, resonator"
                    )
                    .into())
                }
            };
            let (samples, marks) = render_mi_drum_marked(warps_algorithm);
            write_wav(&output, &samples)?;
            let seconds = samples.len() as f32 / 2.0 / SAMPLE_RATE;
            println!("wrote {output} ({seconds:.2}s)");
            for (at, what) in &marks {
                println!("  {:>6.2}s  {what}", at);
            }
        }

        Command::Gate { output } => {
            let (mi_samples, mi_timeline) = render_gate_demo_mi();
            write_wav(&output, &mi_samples)?;
            print_gate_timeline(&output, mi_samples.len(), &mi_timeline);

            // The drum device's two newly-gated machines, in their own file so
            // the two engines do not share a limiter or a bus.
            let drum_path = sibling_path(&output, "-drum");
            let (drum_samples, drum_timeline) = render_gate_demo_drum();
            write_wav(&drum_path, &drum_samples)?;
            print_gate_timeline(&drum_path, drum_samples.len(), &drum_timeline);
        }

        Command::Shaper { output } => {
            let (samples, notes) = render_shaper_kit_comparison();
            write_wav(&output, &samples)?;
            println!(
                "\nwrote {output} ({:.2}s)",
                samples.len() as f32 / 2.0 / SAMPLE_RATE
            );
            for (t, what) in &notes {
                println!("  {t:>5.1}s  {what}");
            }

            let alias_path = sibling_path(&output, "-alias");
            let (alias_samples, alias_notes) = render_shaper_alias_test();
            write_wav(&alias_path, &alias_samples)?;
            println!(
                "\nwrote {alias_path} ({:.2}s)",
                alias_samples.len() as f32 / 2.0 / SAMPLE_RATE
            );
            println!("  full drive, full wet, chromatic rise over three octaves.");
            println!("  Aliasing sounds like partials moving DOWN as the note moves up.");
            for (t, what) in &alias_notes {
                println!("  {t:>5.1}s  {what}");
            }

            let sine_path = sibling_path(&output, "-sine");
            let (sine_samples, sine_notes) = shaper_probe::render_shaper_sine();
            write_wav(&sine_path, &sine_samples)?;
            println!(
                "\nwrote {sine_path} ({:.2}s)",
                sine_samples.len() as f32 / 2.0 / SAMPLE_RATE
            );
            println!("  the stage alone: 55 Hz carrier + 82.5 Hz modulator, timbre 0.5.");
            println!("  no strip, no limiter, nothing peak-normalised — levels are as shaped.");
            for (t, what) in &sine_notes {
                println!("  {t:>5.1}s  {what}");
            }
        }

        Command::Warps { output } => {
            let (samples, timeline) = render_warps_drive_demo();
            write_wav(&output, &samples)?;
            print_gate_timeline(&output, samples.len(), &timeline);

            // The practical version of the same question: Warps' `drive` is
            // *also* its wet/dry mix (`wet_dry = 1 - channel_drive[1]`, and the
            // shim sets both channels to the same value), so there is no way to
            // have heavy colour while keeping most of the dry signal. The
            // default of 0.2 is 60% cross-modulated. This file is the same kit
            // with the four drum tracks fully bypassed, which is the setting a
            // drum machine wants and a melodic voice does not.
            let kit_path = sibling_path(&output, "-kit");
            let (kit_samples, kit_notes) = render_warps_kit_comparison();
            write_wav(&kit_path, &kit_samples)?;
            println!(
                "\nwrote {kit_path} ({:.2}s)",
                kit_samples.len() as f32 / 2.0 / SAMPLE_RATE
            );
            println!("  A = default kit as shipped (all six tracks 60% warped)");
            println!("  B = same kit, four drum tracks fully bypassed");
            for (t, what) in &kit_notes {
                println!("  {t:>5.1}s  {what}");
            }
        }

        Command::Stress { output, seconds } => {
            let samples = render_stress(seconds);
            write_wav(&output, &samples)?;
            let rendered = samples.len() as f32 / 2.0 / SAMPLE_RATE;
            println!("wrote {output} ({rendered:.2}s)");
        }

        Command::Sweep {
            machine,
            macro_name,
            from,
            to,
            steps,
            output_dir,
            lufs,
        } => {
            let id = machine.id();
            let (idx, _) = id
                .macro_by_name(&macro_name)
                .ok_or_else(|| format!("machine {id:?} has no macro named '{macro_name}'"))?;
            check_finite("--from", from)?;
            check_finite("--to", to)?;
            std::fs::create_dir_all(&output_dir)?;
            let steps = steps.max(2);

            for i in 0..steps {
                let t = i as f32 / (steps - 1) as f32;
                let value = from + (to - from) * t;

                // Use track 0 to host the sweep, load that track with the
                // chosen machine, and set the chosen macro. Everything else
                // stays at kit defaults.
                let mut engine = DrumEngine::new();
                engine.tracks[0].load_machine(id);
                engine.tracks[0].set_macro(idx, value);

                let samples = render_one_shot(&mut engine, 0, 2.0);
                let path = format!("{output_dir}/{}_{:02}_{value:.4}.wav", id.name(), i);
                write_wav(&path, &samples)?;

                // Phase 10: log the step's loudness. The same samples that
                // went into the WAV get measured, so the number is exact.
                let m = measure::measure_buffer(&samples, lufs);
                if lufs {
                    println!("{path}  rms={:>6.1} dBFS  lufs={:>5.1}", m.rms_db(), m.lufs);
                } else {
                    println!("{path}  rms={:>6.1} dBFS", m.rms_db());
                }
            }
            println!(
                "\n{steps} files in {output_dir}/ — flip between them to compare \
                 (swept {} {} from {from:.4} to {to:.4})",
                id.name(),
                macro_name,
            );
        }

        Command::Trim {
            machine,
            macro_name,
            from,
            to,
            steps,
            seconds,
            lufs,
        } => {
            let id = machine.id();
            let (idx, info) = id
                .macro_by_name(&macro_name)
                .ok_or_else(|| format!("machine {id:?} has no macro named '{macro_name}'"))?;
            check_finite("--from", from)?;
            check_finite("--to", to)?;
            let steps = steps.max(2);

            let mut values = Vec::with_capacity(steps);
            let mut levels = Vec::with_capacity(steps);

            for i in 0..steps {
                let t = i as f32 / (steps - 1) as f32;
                let value = from + (to - from) * t;

                let mut engine = DrumEngine::new();
                engine.tracks[0].load_machine(id);
                engine.tracks[0].set_macro(idx, value);

                let samples = render_one_shot(&mut engine, 0, seconds);
                let m = measure::measure_buffer(&samples, lufs);
                let level = if lufs { m.lufs } else { m.rms_db() };
                values.push(value);
                levels.push(level);
            }

            // The reference is the step nearest the macro's default value —
            // the factory sound keeps gain 1.0 and the rest of the travel is
            // compensated around it. Steps louder than the reference are
            // attenuated (safe); the table shows the boost needed elsewhere.
            // `clamp` panics on a reversed range, and `--from` > `--to` is a
            // legal descending sweep, so order the bounds first.
            let anchor = info.default.clamp(from.min(to), from.max(to));
            let anchor_idx = (0..steps)
                .min_by(|&a, &b| {
                    (values[a] - anchor)
                        .abs()
                        .total_cmp(&(values[b] - anchor).abs())
                })
                .unwrap();
            let reference = levels[anchor_idx];

            let unit = if lufs { "LUFS" } else { "dBFS" };
            println!(
                "{} {} loudness trim ({} steps)",
                id.name(),
                macro_name,
                steps
            );
            println!(
                "reference: default value {:.4} at {reference:.2} {unit}\n",
                values[anchor_idx]
            );
            println!(
                "{:>8}  {:>8}  {:>8}  {:>8}  {:>10}",
                "value", "level", "trim_db", "gain", "trimmed"
            );
            println!(
                "{:─>8}  {:─>8}  {:─>8}  {:─>8}  {:─>10}",
                "", "", "", "", ""
            );
            for i in 0..steps {
                let trim_db = reference - levels[i];
                let gain = 10.0_f32.powf(trim_db / 20.0);
                let trimmed = levels[i] + trim_db;
                println!(
                    "{value:>8.4}  {level:>8.2}  {trim_db:>8.2}  {gain:>8.4}  {trimmed:>10.2}",
                    value = values[i],
                    level = levels[i],
                );
            }

            // Paste-ready Rust for the machine's set_macros: an array of
            // linear gains indexed by quantised macro value (step 0 = `from`,
            // last = `to`).
            println!(
                "\npaste-ready `const` for {} ({} linear gains):",
                id.name(),
                steps
            );
            println!(
                "// {}_{} loudness trim ({}): reference {reference:.2} {unit}",
                id.name().to_uppercase().replace('-', "_"),
                macro_name.to_uppercase(),
                info.name,
            );
            // Macro names can carry punctuation (`SEND.DLY`), which is not
            // legal in an identifier — flatten it so the block really is
            // paste-ready.
            let const_base = macro_name
                .to_uppercase()
                .replace(|c: char| !(c.is_ascii_alphanumeric() || c == '_'), "_");
            print!("const {const_base}_TRIM: [f32; {steps}] = [");
            for (i, &level) in levels.iter().enumerate().take(steps) {
                let trim_db = reference - level;
                let gain = 10.0_f32.powf(trim_db / 20.0);
                if i > 0 {
                    print!(", ");
                }
                print!("{gain:.4}");
            }
            println!("];");
        }

        Command::Verify {
            steps,
            machines,
            dump,
        } => {
            // The sweep divides by `steps - 1`; enforce the help text's
            // ">= 2" floor the same way Sweep and Trim do.
            let steps = steps.max(2);
            if let [machine, macro_name] = &dump[..] {
                let arg = MachineArg::from_name(machine)
                    .ok_or_else(|| format!("unknown machine '{machine}'"))?;
                let id = arg.id();
                let (idx, _) = id
                    .macro_by_name(macro_name)
                    .ok_or_else(|| format!("machine {id:?} has no macro '{macro_name}'"))?;
                verify::dump_knob(id, idx, steps);
                return Ok(());
            }
            let ids: Vec<drum_engine::machines::MachineId> = if machines.is_empty() {
                drum_engine::machines::MachineId::ALL.to_vec()
            } else {
                machines.into_iter().map(|m| m.id()).collect()
            };
            let totals = verify::run(&ids, steps);
            println!(
                "\n{} machines, {} knobs: {} ok, {} failed",
                ids.len(),
                totals.checked,
                totals.checked - totals.failed,
                totals.failed,
            );
            std::process::exit(if totals.failed == 0 { 0 } else { 1 });
        }

        Command::Machine {
            machine,
            macros,
            output,
            seconds,
        } => {
            let id = machine.id();
            let mut engine = DrumEngine::new();
            engine.tracks[0].load_machine(id);
            for spec in macros {
                apply_macro_arg(&mut engine, &spec)?;
            }
            let samples = render_one_shot(&mut engine, 0, seconds);
            write_wav(&output, &samples)?;
            let secs = samples.len() as f32 / 2.0 / SAMPLE_RATE;
            println!("wrote {output} ({secs:.2}s)");
        }

        Command::Catalog {
            output,
            per_machine,
        } => {
            use drum_engine::machines::MachineId;
            let mut all_samples = Vec::new();
            for id in MachineId::ALL {
                let mut engine = DrumEngine::new();
                engine.tracks[0].load_machine(id);
                let samples = render_one_shot(&mut engine, 0, per_machine);
                all_samples.extend(samples);
                println!("  {}", id.name());
            }
            write_wav(&output, &all_samples)?;
            let secs = all_samples.len() as f32 / 2.0 / SAMPLE_RATE;
            println!(
                "wrote {output} ({secs:.2}s) — {} machines x {per_machine:.1}s each",
                MachineId::COUNT
            );
        }

        Command::Cheatsheet { format, output } => {
            let fmt = match format {
                CheatsheetFormat::Markdown => "markdown",
                CheatsheetFormat::Html => "html",
            };
            let content = generate_cheatsheet(fmt);
            if let Some(path) = output {
                std::fs::write(&path, &content)?;
            } else {
                print!("{content}");
            }
        }

        #[cfg(feature = "live")]
        Command::Play { bpm } => {
            check_bpm(bpm)?;
            play_live(bpm)?
        }

        #[cfg(feature = "live")]
        Command::Device {
            device,
            out,
            port,
            list,
            multi_out,
        } => match device.as_str() {
            "drum" => device::run(Box::new(DrumEngine::new()), &out, &port, list, multi_out)?,
            // `device::run` hands the engine to the cpal audio-callback thread,
            // so it requires `E: Send`. `MiDrumEngine` is not: `PlaitsVoice`
            // holds a `*mut u8` into `mi-dsp`'s static scratch pool, whose
            // `static mut` free-bitmap is documented as single-threaded. Wiring
            // this up means making that pool thread-safe first — see issue
            // #9. Every other mi-drum path (render,
            // play, WAV) works today; only live device mode is blocked.
            "mi-drum" => {
                return Err(
                    "mi-drum device mode is not available yet: MiDrumEngine is not \
                            Send, because mi-dsp's Plaits scratch pool is single-threaded. \
                            Use `render device drum`, or the WAV render path for mi-drum."
                        .into(),
                )
            }
            other => {
                return Err(format!("unknown device '{other}'. available: drum, mi-drum").into())
            }
        },

        #[cfg(feature = "live")]
        Command::Monitor {
            port,
            channel,
            list,
            hex,
            realtime,
        } => monitor::run(&port, channel, list, hex, realtime)?,

        #[cfg(feature = "live")]
        Command::Slack {
            port,
            seconds,
            drive_hz,
            reboot,
        } => {
            if reboot {
                slack::reboot(port.as_deref())?
            } else {
                slack::run(port.as_deref(), seconds, drive_hz)?
            }
        }
    }

    Ok(())
}

/// Parse `NAME=VALUE` and apply to track 0 of the engine.
fn apply_macro_arg(engine: &mut DrumEngine, spec: &str) -> Result<(), Box<dyn std::error::Error>> {
    let (name, val) = spec
        .split_once('=')
        .ok_or_else(|| format!("bad --macro '{spec}', expected NAME=VALUE"))?;
    let id = engine.tracks[0].id();
    let (idx, _) = id
        .macro_by_name(name)
        .ok_or_else(|| format!("machine {id:?} has no macro named '{name}'"))?;
    let value: f32 = val.parse()?;
    if !value.is_finite() {
        return Err(format!("--macro {name} must be a finite number, got {value}").into());
    }
    engine.tracks[0].set_macro(idx, value);
    Ok(())
}

/// Reject `bpm <= 0` / non-finite before it reaches the step-timing math,
/// where `60.0 / 0.0` becomes `inf` and `inf as usize` overflows.
fn check_bpm(bpm: f32) -> Result<(), Box<dyn std::error::Error>> {
    if !bpm.is_finite() || bpm <= 0.0 {
        return Err(format!("--bpm must be a positive finite number, got {bpm}").into());
    }
    Ok(())
}

/// Reject a non-finite sweep bound: it flows straight into
/// `value = from + (to - from) * t` and every measured level would be NaN.
fn check_finite(arg: &str, v: f32) -> Result<(), Box<dyn std::error::Error>> {
    if !v.is_finite() {
        return Err(format!("{arg} must be a finite number, got {v}").into());
    }
    Ok(())
}

/// Sibling output path: `foo.wav` → `foo-alias.wav`.
///
/// Replaces the path's *trailing* `.wav` (case-insensitive) rather than the
/// first substring match (so a directory named `sweep.wav/` survives intact),
/// and a path with no `.wav` suffix still gets a distinct name — two renders
/// can never silently clobber each other's file.
fn sibling_path(path: &str, suffix: &str) -> String {
    let lower = path.to_lowercase();
    if let Some(stem) = lower.strip_suffix(".wav") {
        format!("{}{suffix}.wav", &path[..stem.len()])
    } else {
        format!("{path}{suffix}.wav")
    }
}

#[cfg(feature = "live")]
fn play_live(bpm: f32) -> Result<(), Box<dyn std::error::Error>> {
    use cpal::traits::{DeviceTrait, HostTrait, StreamTrait};
    use std::sync::mpsc;

    let host = cpal::default_host();
    let device = host
        .default_output_device()
        .ok_or("no output device available")?;
    let config = device.default_output_config()?;

    if config.sample_rate().0 as f32 != SAMPLE_RATE {
        eprintln!(
            "warning: device runs at {}Hz, engine is built for {}Hz — pitch will be off",
            config.sample_rate().0,
            SAMPLE_RATE
        );
        eprintln!("set your output device to {SAMPLE_RATE}Hz for an accurate audition");
    }

    let mut engine = DrumEngine::new();
    setup_kit_mix(&mut engine);
    let pattern = Pattern::demo();
    let samples_per_step = (SAMPLE_RATE * 60.0 / bpm / 4.0) as usize;

    let mut l = [0.0f32; BLOCK];
    let mut r = [0.0f32; BLOCK];
    let mut cursor = BLOCK; // force a render on first callback
    let mut sample_clock = 0usize;
    let mut next_step_at = 0usize;
    let mut step = 0usize;

    let (err_tx, err_rx) = mpsc::channel();
    let channels = config.channels() as usize;

    let stream = device.build_output_stream(
        &config.into(),
        move |data: &mut [f32], _| {
            for frame in data.chunks_mut(channels) {
                if cursor >= BLOCK {
                    while next_step_at <= sample_clock {
                        let s = step % 16;
                        let mut i = 0;
                        while i < TRACKS {
                            if pattern.tracks[i][s] {
                                // Same absolute transpose as `render_pattern`,
                                // applied before the trigger so `play` sounds
                                // like `render`: a `0` note must go through
                                // too, or the previous hit's transposition
                                // sticks.
                                engine.tracks[i].retune(pattern.notes[i][s] as f32);
                                engine.trigger(i, render::default_velocity_for_track(i, s));
                            }
                            i += 1;
                        }
                        step += 1;
                        next_step_at += samples_per_step;
                    }
                    engine.process(&mut l, &mut r);
                    cursor = 0;
                }

                let (sl, sr) = (l[cursor], r[cursor]);
                cursor += 1;
                sample_clock += 1;

                for (i, out) in frame.iter_mut().enumerate() {
                    *out = if i % 2 == 0 { sl } else { sr };
                }
            }
        },
        move |e| {
            let _ = err_tx.send(e);
        },
        None,
    )?;

    stream.play()?;
    println!("playing at {bpm} BPM — ctrl-c to stop");

    loop {
        if let Ok(e) = err_rx.try_recv() {
            eprintln!("stream error: {e}");
            break;
        }
        std::thread::sleep(std::time::Duration::from_millis(100));
    }
    Ok(())
}

/// A stable 64-bit digest of a rendered buffer.
///
/// FNV-1a over the raw `f32` bits. Not cryptographic — this is a tripwire, and
/// the thing it has to be is *stable*, which a hand-written hash over
/// `to_bits()` is and a float-formatting round-trip is not. Rendered WAVs are
/// gitignored, so a committed digest is how a baseline gets into the repo.
#[cfg(test)]
fn digest(samples: &[f32]) -> u64 {
    let mut h: u64 = 0xcbf2_9ce4_8422_2325;
    for s in samples {
        for byte in s.to_bits().to_le_bytes() {
            h ^= byte as u64;
            h = h.wrapping_mul(0x1000_0000_01b3);
        }
    }
    h
}

#[cfg(test)]
mod mi_drum_baseline {
    use super::*;

    /// The Phase 14 mi-drum render, pinned.
    ///
    /// Re-pinned once the render became reproducible at all. For most of
    /// Phase 14 this constant was decorative: two vendored voices read state
    /// their `Init` never wrote, so the same binary produced a different
    /// digest in every process and the gate passed or failed by luck. Both are
    /// fixed (`peaks::HighHat`'s oscillator phases and
    /// `plaits::SyntheticBassDrum`'s transient envelope states); this value is
    /// the first one that means anything.
    ///
    /// Because the failure mode was *between* processes rather than within
    /// one, a single green run does not prove much. The check that does:
    ///
    /// ```text
    /// for i in $(seq 1 20); do
    ///   cargo test -q -p render mi_drum_baseline 2>&1 | grep -oE "got 0x[0-9a-f]+"
    /// done | sort | uniq -c
    /// ```
    ///
    /// Silence means every run matched. More than one distinct digest means
    /// something is reading uninitialised memory again, and
    /// `mi-drum-engine`'s `slot_reuse` test is the place to start.
    ///
    /// Rendered WAVs are gitignored, so the digest is the committed artefact.
    /// Reproduce the audio with `cargo run -p render -- mi-drum`.
    // Re-pinned again when the per-sample `libm::powf(2.0, x)` in the Ripples
    // cutoff loop became `fast::exp2_approx`. Measured against the `powf`
    // render, the difference is **59.9 dB below the signal** (peak sample
    // delta 7.5e-3) — an approximation error on a filter cutoff, not added
    // noise, in exchange for a call that cost 1,214 cycles per sample per
    // track.
    //
    // Re-pinned before that when the modulation bus moved from four
    // `stages::SegmentGenerator`s to `Lfo` + `AhdEnv`, which also made the
    // `AD.ATK` map exponential — the linear one put the shipped default at
    // 51 ms, slower than the transient it shapes.
    //
    // Previously re-pinned when the strip's shaping stage changed from
    // `warps::Modulator` to `core::dsp::shaper`. That is a deliberate change to the sound, not a
    // regression: Warps cost 79,855 cycles per track against a 400,000-cycle
    // budget for the whole engine, and the replacement is roughly a tenth of
    // that. See `docs/warps-vendoring.md`.
    //
    // Verified identical across five separate processes before pinning — the
    // discipline `1633ca5` established after two uninitialised reads made this
    // digest drift between runs.
    const BASELINE_DIGEST: u64 = 0xfdcd_64b0_d89f_6cfd;

    /// One test, one render, deliberately.
    ///
    /// Splitting the silence check into its own `#[test]` renders twice, and
    /// two renders in the same process interleave their draws on the shared
    /// `stmlib::Random` generator — so both digests come out different, and
    /// different again on the next run. Seeding fixes the starting point; only
    /// not overlapping fixes the interleaving.
    #[test]
    fn mi_drum_baseline_is_unchanged() {
        let samples = render_mi_drum_marked(0.0).0;

        // The baseline has to exercise the machines, or the digest pins
        // silence and Phase 14.0 passes its gate by doing nothing.
        // `fast::soft_clip` is a rational approximation and carries `recip`'s
        // ~2 ulp of error, so a sample landing on its internal clamp can come
        // back as 1.0000001. That is -200 dB and converts to exactly full
        // scale in 24-bit rather than wrapping, so the bound to assert is
        // "cannot wrap the DAC", not "is bit-exactly 1.0" — the same tolerance
        // `output_never_exceeds_unity` uses in `mi-drum-engine`. 1 ulp at 1.0
        // is 1.19e-7.
        const TOL: f32 = 4.0 * 1.19e-7;
        let peak = samples.iter().fold(0.0f32, |m, s| m.max(s.abs()));
        assert!(peak > 0.1, "baseline is near-silent: peak={peak}");
        assert!(
            peak <= 1.0 + TOL,
            "baseline clips: peak={peak} — more than the clipper's own \
             approximation error, so this is a real overflow"
        );

        let actual = digest(&samples);
        assert_eq!(
            actual, BASELINE_DIGEST,
            "mi-drum baseline moved: expected {BASELINE_DIGEST:#018x}, got {actual:#018x}. \
             If this is Phase 14.0, the block restructure is not sample-exact. \
             If it is a later phase that changes the sound on purpose, re-pin the constant."
        );
    }
}

#[cfg(test)]
mod drum_demo_baseline {
    use super::*;

    /// The drum-device demo render, pinned — the test half of the
    /// `out.baseline.wav` bit-identity gate from AGENTS.md (rendered WAVs are
    /// gitignored, so this digest is the committed artefact).
    ///
    /// Renders exactly what `cargo run -p render -- render` writes by default:
    /// the demo pattern over the default kit at 130 BPM, two bars. Unlike the
    /// mi-drum baseline, the drum engine has no shared RNG, so the digest is
    /// stable across processes without seeding.
    ///
    /// If a change is intentional, re-pin the constant (and regenerate
    /// `out.baseline.wav`); do not silently move it.
    const DEMO_DIGEST: u64 = 0xadf9_b726_5e5f_d35c;

    #[test]
    fn demo_render_is_unchanged() {
        let samples = render_pattern(&Pattern::demo(), 130.0, 2);

        // The gate has to exercise the kit, or it would happily pin silence.
        let peak = samples.iter().fold(0.0f32, |m, s| m.max(s.abs()));
        assert!(peak > 0.1, "demo render is near-silent: peak={peak}");
        assert!(
            samples.iter().all(|s| s.is_finite()),
            "demo render contains non-finite samples"
        );

        let actual = digest(&samples);
        assert_eq!(
            actual, DEMO_DIGEST,
            "demo render moved: expected {DEMO_DIGEST:#018x}, got {actual:#018x}. \
             If the change is intentional, re-pin the constant and regenerate \
             `out.baseline.wav`; if not, a render path regressed."
        );
    }
}
