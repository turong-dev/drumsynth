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

use clap::{Parser, Subcommand, ValueEnum};
use drum_engine::machines::{
    MachineId, SLOT_MACH_0, SLOT_MACH_1, SLOT_MACH_2, SLOT_MACH_3, SLOT_MACH_5, SLOT_MACH_7,
};
use drum_engine::{DrumEngine, BLOCK, SAMPLE_RATE, TRACKS};
#[cfg(feature = "live")]
use mi_drum_engine::MiDrumEngine;

mod measure;
mod verify;

/// The MIDI-in / audio-out device mode. Behind the `live` feature with the
/// rest of the host audio stack.
#[cfg(feature = "live")]
mod device;

/// The MIDI input monitor. Also behind the `live` feature because it needs
/// `midir` for port enumeration and connection.
#[cfg(feature = "live")]
mod monitor;

/// The 8-track kit you get from `DrumEngine::new()`.
#[derive(Parser)]
#[command(name = "render", about = "Audition the drum engine without hardware")]
struct Cli {
    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand)]
enum Command {
    /// Render the demo pattern to a WAV file.
    Render {
        /// Output path.
        #[arg(short, long, default_value = "out.wav")]
        output: String,
        /// Tempo in BPM.
        #[arg(short, long, default_value_t = 130.0)]
        bpm: f32,
        /// Number of bars.
        #[arg(long, default_value_t = 2)]
        bars: usize,
    },
    /// Render a retrigger/choke stress test to a WAV file.
    ///
    /// Dense sample-accurate rolls on the kick, snare, and closed hat, plus
    /// an open hat being choked over and over — the "lots of notes quickly"
    /// scenario that used to produce clicks at the tail cuts. Regression
    /// audition: render it, play it, and confirm the cut-off tails are faded
    /// instead of stepped.
    Stress {
        /// Output path.
        #[arg(short, long, default_value = "stress.wav")]
        output: String,
        /// Length in seconds.
        #[arg(short, long, default_value_t = 6.0)]
        seconds: f32,
    },
    /// Render one file per value of a swept macro knob.
    ///
    /// `<machine>` is one of `bd-classic`, `sd-natural`, `hat-classic`.
    /// `<macro>` is the macro's name uppercased (`TUNE`, `SWEEP`, `DEC`, ...).
    ///
    /// Each step logs its integrated RMS in dBFS, so you can see how a knob
    /// changes loudness as well as sound — the Phase 10 tuning workflow.
    /// Add `--lufs` for K-weighted loudness (ITU-R BS.1770), which tracks
    /// perceived level where spectral knobs (filter) understate it.
    Sweep {
        /// Machine to sweep a knob of.
        #[arg(value_enum, default_value = "bd-classic")]
        machine: MachineArg,
        /// Macro knob to sweep, by name (case-insensitive).
        #[arg(value_name = "macro", default_value = "DEC")]
        macro_name: String,
        /// Lowest value.
        #[arg(long, default_value_t = 0.0)]
        from: f32,
        /// Highest value.
        #[arg(long, default_value_t = 1.0)]
        to: f32,
        /// How many steps.
        #[arg(long, default_value_t = 8)]
        steps: usize,
        /// Directory for the output files.
        #[arg(short, long, default_value = "sweep")]
        output_dir: String,
        /// Also log K-weighted loudness (LUFS) per step.
        #[arg(long)]
        lufs: bool,
    },
    /// Sweep a macro, measure each step, and derive its loudness trim.
    ///
    /// Prints a per-step table of RMS (dBFS) and the inverse gain that would
    /// flatten loudness across the travel, plus a paste-ready Rust `const`
    /// array you can drop into the machine's `set_macros` to compensate.
    /// The reference is the step nearest the macro's *default* value: the
    /// factory sound stays exactly as-is (gain 1.0), louder steps are
    /// attenuated safely, and quieter steps report a boost gain so you can
    /// see where compensation would push toward clipping.
    ///
    /// This is the automated half of "macros are loudness-compensated along
    /// their travel": the tool derives the numbers, you paste them in.
    Trim {
        /// Machine to sweep a knob of.
        #[arg(value_enum)]
        machine: MachineArg,
        /// Macro knob to trim, by name (case-insensitive).
        #[arg(value_name = "macro")]
        macro_name: String,
        /// Lowest value.
        #[arg(long, default_value_t = 0.0)]
        from: f32,
        /// Highest value.
        #[arg(long, default_value_t = 1.0)]
        to: f32,
        /// How many steps (must be >= 2).
        #[arg(long, default_value_t = 8)]
        steps: usize,
        /// Length of each rendered hit in seconds.
        #[arg(long, default_value_t = 2.0)]
        seconds: f32,
        /// Measure K-weighted loudness (LUFS) instead of plain RMS.
        #[arg(long)]
        lufs: bool,
    },
    /// Sweep every knob on every machine and check monotonicity.
    ///
    /// The design rule is "macros are tuned paths, not renamed params — no
    /// dead zones, monotonic, loudness-compensated". This is the automated
    /// form of that: for each machine × voice knob it sweeps 0..1, measures
    /// the hit's integrated RMS and tail length at each step, and flags
    /// non-monotonic travel or dead zones. Exits non-zero if anything fails.
    ///
    /// Spatial/routing knobs (PAN, SEND.*, the MACH selector) are excluded —
    /// they are not loudness paths and mono-sum RMS would read them as dead
    /// zones.
    Verify {
        /// Steps per sweep (>= 2).
        #[arg(long, default_value_t = 9)]
        steps: usize,
        /// Only check these machines (repeatable, by name).
        #[arg(long, value_enum)]
        machines: Vec<MachineArg>,
        /// Print the per-step curve of one machine's knob and exit
        /// (e.g. `--dump rs TUNE`). The Phase 10 tuning view behind a
        /// failing verdict.
        #[arg(long, num_args = 2)]
        dump: Vec<String>,
    },
    /// Render a single one-shot of the given machine with custom macros.
    Machine {
        /// Machine to render.
        #[arg(value_enum)]
        machine: MachineArg,
        /// Override macros, in the form `TUNE=0.5 DEC=0.2 ...`.
        #[arg(long = "macro", value_name = "NAME=VALUE", num_args = 1)]
        macros: Vec<String>,
        /// Output WAV path.
        #[arg(short, long, default_value = "out.wav")]
        output: String,
        /// Length in seconds.
        #[arg(long, default_value_t = 2.0)]
        seconds: f32,
    },
    /// Render one hit per machine in the catalogue, in order.
    ///
    /// Each machine gets its own short segment, so you can audition the
    /// whole catalogue in one pass without clicking through individual files.
    Catalog {
        /// Output WAV path.
        #[arg(short, long, default_value = "out.wav")]
        output: String,
        /// Seconds per machine.
        #[arg(long, default_value_t = 1.5)]
        per_machine: f32,
    },
    /// Print a cheat-sheet of every macro slot, split into one table for the
    /// common (track-routed) parameters and one table per machine for the
    /// machine-internal parameters. Generated from `MACHINE_INFO` in the
    /// engine, so it can't drift from the code.
    ///
    /// `--format html` produces a self-contained styled HTML page; the
    /// default is markdown.
    Cheatsheet {
        /// Output format.
        #[arg(short, long, value_enum, default_value = "markdown")]
        format: CheatsheetFormat,
        /// Output path. If omitted, prints to stdout.
        #[arg(short, long)]
        output: Option<String>,
    },
    /// Render a kit pattern in real time through the sound card.
    #[cfg(feature = "live")]
    Play {
        /// Tempo in BPM.
        #[arg(short, long, default_value_t = 130.0)]
        bpm: f32,
    },
    /// Run the engine as a MIDI-in / audio-out device.
    ///
    /// Exposes a virtual CoreMIDI input port (so any DAW or controller can
    /// sequence it) and renders through the audio device named by `--out` —
    /// BlackHole, your speakers, or an aggregate device you created. The
    /// engine is fixed at 48 kHz, so the device must run at that rate.
    ///
    /// `--multi-out` opens an 18-channel session (8 stereo track pairs +
    /// one stereo wet-FX return) instead of the default stereo sum, so a
    /// DAW can mix each drum on its own channel. Each track's dry signal is
    /// on channels `2*t, 2*t+1`; the shared wet return (delay + reverb sum,
    /// pre-drive/pre-clip) is on channels 16/17. The engine primitive
    /// behind this is `DrumEngine::process_dry_wet`, which the firmware's
    /// future multi-DAC/TDM path will route the same way.
    #[cfg(feature = "live")]
    Device {
        /// Device to run. `drum` (the default) or `mi-drum`.
        #[arg(value_name = "DEVICE", default_value = "drum")]
        device: String,
        /// Substring to match against output device names. Defaults to
        /// "BlackHole" if one is installed, else the system default device.
        #[arg(short, long)]
        out: Option<String>,
        /// Name of the virtual MIDI input port other apps see.
        #[arg(long, default_value = "Drumkit Engine")]
        port: String,
        /// Print available output devices and exit.
        #[arg(long)]
        list: bool,
        /// Per-track multi-out: 18 channels (8 × stereo dry pairs + 1 ×
        /// stereo wet). The device must support 18ch @ 48kHz F32.
        #[arg(long)]
        multi_out: bool,
    },
    /// Monitor a MIDI input port, printing each message with a timestamp.
    ///
    /// Useful for checking what a controller or sequencer is sending before
    /// the bytes reach the engine. `--channel` filters to one MIDI channel
    /// (1–16). `--hex` also shows the raw bytes. `--realtime` shows clock
    /// and transport bytes (hidden by default to keep dense drum streams
    /// readable).
    #[cfg(feature = "live")]
    Monitor {
        /// MIDI input port name (substring match) or numeric index.
        #[arg(short, long)]
        port: Option<String>,
        /// MIDI channel to filter on, 1–16.
        #[arg(short, long, value_parser = clap::value_parser!(u8).range(1..=16))]
        channel: Option<u8>,
        /// Print available input ports and exit.
        #[arg(long)]
        list: bool,
        /// Also print raw message bytes in hex.
        #[arg(long)]
        hex: bool,
        /// Show real-time messages (clock, start, stop, continue).
        #[arg(long)]
        realtime: bool,
    },
}

#[derive(Clone, Copy, ValueEnum)]
enum CheatsheetFormat {
    /// Markdown tables.
    Markdown,
    /// Self-contained styled HTML page.
    Html,
}

#[derive(Clone, Copy, ValueEnum)]
enum MachineArg {
    BdClassic,
    BdFm,
    BdVa,
    Tom,
    SdNatural,
    SdFm,
    Rs,
    Cp,
    HatClassic,
    HhBasic,
    CyMetallic,
    CbClassic,
    SyTone,
    DubSiren,
    SweepFx,
}

impl MachineArg {
    fn id(self) -> MachineId {
        match self {
            Self::BdClassic => MachineId::BdClassic,
            Self::BdFm => MachineId::BdFm,
            Self::BdVa => MachineId::BdVa,
            Self::Tom => MachineId::Tom,
            Self::SdNatural => MachineId::SdNatural,
            Self::SdFm => MachineId::SdFm,
            Self::Rs => MachineId::Rs,
            Self::Cp => MachineId::Cp,
            Self::HatClassic => MachineId::HatClassic,
            Self::HhBasic => MachineId::HhBasic,
            Self::CyMetallic => MachineId::CyMetallic,
            Self::CbClassic => MachineId::CbClassic,
            Self::SyTone => MachineId::SyTone,
            Self::DubSiren => MachineId::DubSiren,
            Self::SweepFx => MachineId::SweepFx,
        }
    }

    fn from_name(s: &str) -> Option<Self> {
        for m in MachineId::ALL {
            if m.name() == s {
                return Some(MachineArg::from_machine(m));
            }
        }
        None
    }

    fn from_machine(id: MachineId) -> Self {
        match id {
            MachineId::BdClassic => Self::BdClassic,
            MachineId::BdFm => Self::BdFm,
            MachineId::BdVa => Self::BdVa,
            MachineId::Tom => Self::Tom,
            MachineId::SdNatural => Self::SdNatural,
            MachineId::SdFm => Self::SdFm,
            MachineId::Rs => Self::Rs,
            MachineId::Cp => Self::Cp,
            MachineId::HatClassic => Self::HatClassic,
            MachineId::HhBasic => Self::HhBasic,
            MachineId::CyMetallic => Self::CyMetallic,
            MachineId::CbClassic => Self::CbClassic,
            MachineId::SyTone => Self::SyTone,
            MachineId::DubSiren => Self::DubSiren,
            MachineId::SweepFx => Self::SweepFx,
        }
    }
}

/// Allocate a `MiDrumEngine` on the heap and construct it in-place.
///
/// The engine is too large for the default test thread stack, so this helper
/// uses `alloc` + `new_in_place` instead of `MiDrumEngine::new()`.
#[cfg(feature = "live")]
#[allow(dead_code)] // Re-enabled by the "mi-drum" device arm once MiDrumEngine is Send.
fn mi_drum_in_place() -> Box<MiDrumEngine> {
    use std::alloc::{alloc, Layout};

    unsafe {
        let layout = Layout::new::<MiDrumEngine>();
        let ptr = alloc(layout) as *mut MiDrumEngine;
        assert!(!ptr.is_null(), "failed to allocate MiDrumEngine");
        MiDrumEngine::new_in_place(ptr);
        Box::from_raw(ptr)
    }
}

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let cli = Cli::parse();

    match cli.command {
        Command::Render { output, bpm, bars } => {
            let samples = render_pattern(&Pattern::demo(), bpm, bars);
            write_wav(&output, &samples)?;
            let seconds = samples.len() as f32 / 2.0 / SAMPLE_RATE;
            println!("wrote {output} ({seconds:.2}s)");
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
            let (idx, info) = id
                .macro_by_name(&macro_name)
                .ok_or_else(|| format!("machine {id:?} has no macro named '{macro_name}'"))?;
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
            let _ = info;
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
            let anchor = info.default.clamp(from, to);
            let anchor_idx = (0..steps)
                .min_by(|&a, &b| {
                    (values[a] - anchor)
                        .abs()
                        .partial_cmp(&(values[b] - anchor).abs())
                        .unwrap()
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
            print!(
                "const {}_TRIM: [f32; {steps}] = [",
                macro_name.to_uppercase()
            );
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
            let ids: Vec<MachineId> = if machines.is_empty() {
                MachineId::ALL.to_vec()
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
        Command::Play { bpm } => play_live(bpm)?,

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
            // this up means making that pool thread-safe first — see the
            // known-issue note in PLAN.md. Every other mi-drum path (render,
            // play, WAV) works today; only live device mode is blocked.
            "mi-drum" => {
                return Err(
                    "mi-drum device mode is not available yet: MiDrumEngine is not \
                            Send, because mi-dsp's Plaits scratch pool is single-threaded. \
                            Use `render --device drum`, or the WAV render path for mi-drum."
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
    engine.tracks[0].set_macro(idx, value);
    Ok(())
}

/// One row in the cheat-sheet table.
struct CheatRow {
    slot: usize,
    cc: usize,
    bank: &'static str,
    name: &'static str,
    abbrev: &'static str,
    default: f32,
    is_resv: bool,
}

impl CheatsheetRow for CheatRow {
    fn slot(&self) -> usize {
        self.slot
    }
    fn cc(&self) -> usize {
        self.cc
    }
    fn bank(&self) -> &str {
        self.bank
    }
    fn name(&self) -> &str {
        self.name
    }
    fn abbrev(&self) -> &str {
        self.abbrev
    }
    fn default(&self) -> f32 {
        self.default
    }
    fn is_resv(&self) -> bool {
        self.is_resv
    }
}

/// Generate the cheat-sheet in the requested format (`"markdown"` or `"html"`).
///
/// The output is split into:
/// - one **Common Parameters** table for track-routed slots (same on every
///   machine), and
/// - one table per machine for its machine-internal slots.
///
/// Both are derived from `MACHINE_INFO` in the engine, so they can't drift.
fn generate_cheatsheet(format: &str) -> String {
    use drum_engine::machines::{MachineId, MACROS_PER_BANK, NUM_MACROS};

    let machines = MachineId::ALL;

    // Classify each slot as track-routed (same name on every machine, not
    // RESV) or machine-internal.
    let mut is_track = [false; NUM_MACROS];
    for (slot, track) in is_track.iter_mut().enumerate() {
        let first = machines[0].macros()[slot].name;
        if first != "RESV" && machines.iter().all(|m| m.macros()[slot].name == first) {
            *track = true;
        }
    }

    let bank_name = |slot: usize| match slot / MACROS_PER_BANK {
        0 => "MACH",
        1 => "FILT",
        2 => "TRACK",
        3 => "MOD",
        _ => "????",
    };

    let rows_for = |machine: Option<MachineId>, slots: &[usize]| -> Vec<CheatRow> {
        slots
            .iter()
            .map(|&slot| {
                let m = machine.unwrap_or(machines[0]);
                let info = m.macros()[slot];
                CheatRow {
                    slot,
                    cc: 20 + slot,
                    bank: bank_name(slot),
                    name: info.name,
                    abbrev: info.abbrev,
                    default: info.default,
                    is_resv: info.name == "RESV",
                }
            })
            .collect()
    };

    let track_slots: Vec<usize> = (0..NUM_MACROS).filter(|&s| is_track[s]).collect();
    let machine_slots: Vec<usize> = (0..NUM_MACROS).filter(|&s| !is_track[s]).collect();

    let common_rows = rows_for(None, &track_slots);

    if format == "html" {
        generate_cheatsheet_html(machines, &common_rows, &machine_slots, &rows_for)
    } else {
        generate_cheatsheet_markdown(machines, &common_rows, &machine_slots, &rows_for)
    }
}

/// Markdown output: one heading + table per section.
fn generate_cheatsheet_markdown(
    machines: [MachineId; MachineId::COUNT],
    common_rows: &[impl CheatsheetRow],
    machine_slots: &[usize],
    rows_for: &impl Fn(Option<MachineId>, &[usize]) -> Vec<CheatRow>,
) -> String {
    let mut out = String::new();

    out.push_str("# Drum Synth Macro Cheat-Sheet\n\n");
    out.push_str("Generated from `MACHINE_INFO` in the engine.\n\n");

    // Common table
    out.push_str("## Common Parameters (track-routed)\n\n");
    out.push_str("Same meaning on every machine. Values are factory defaults (0..1).\n\n");
    out.push_str("| Slot | CC | Bank | Name | Abbrev | Default |\n");
    out.push_str("|------|-----|------|------|--------|---------|\n");
    for row in common_rows {
        out.push_str(&format!(
            "| {} | {} | {} | {} | {} | {} |\n",
            row.slot(),
            row.cc(),
            row.bank(),
            row.name(),
            row.abbrev(),
            if row.is_resv() {
                "RESV".to_string()
            } else {
                format!("{:.2}", row.default())
            }
        ));
    }
    out.push('\n');

    // Per-machine tables
    for m in machines {
        let rows = rows_for(Some(m), machine_slots);
        out.push_str(&format!("## {}\n\n", m.label()));
        out.push_str("| Slot | CC | Bank | Name | Abbrev | Default |\n");
        out.push_str("|------|-----|------|------|--------|---------|\n");
        for row in &rows {
            out.push_str(&format!(
                "| {} | {} | {} | {} | {} | {} |\n",
                row.slot(),
                row.cc(),
                row.bank(),
                row.name(),
                row.abbrev(),
                if row.is_resv() {
                    "RESV".to_string()
                } else {
                    format!("{:.2}", row.default())
                }
            ));
        }
        out.push('\n');
    }

    out.push_str("---\n\n");
    out.push_str("**RESV** = reserved/unused on that machine.  \n");
    out.push_str("CC = MIDI CC number (20 + slot).  \n");
    out.push_str("Bank: MACH (0–7), FILT (8–15), TRACK (16–23), MOD (24–31).\n");

    out
}

/// HTML output: self-contained styled page.
fn generate_cheatsheet_html(
    machines: [MachineId; MachineId::COUNT],
    common_rows: &[impl CheatsheetRow],
    machine_slots: &[usize],
    rows_for: &impl Fn(Option<MachineId>, &[usize]) -> Vec<CheatRow>,
) -> String {
    let mut out = String::new();

    out.push_str("<!DOCTYPE html>\n<html lang=\"en\">\n<head>\n");
    out.push_str("<meta charset=\"utf-8\">\n");
    out.push_str("<meta name=\"viewport\" content=\"width=device-width, initial-scale=1\">\n");
    out.push_str("<title>Drum Synth Macro Cheat-Sheet</title>\n");
    out.push_str("<style>\n");
    out.push_str(CHEATSHEET_CSS);
    out.push_str("</style>\n");
    out.push_str("</head>\n<body>\n");

    out.push_str("<header>\n");
    out.push_str("<h1>Drum Synth Macro Cheat-Sheet</h1>\n");
    out.push_str("<p>Generated from <code>MACHINE_INFO</code> in the engine.</p>\n");
    out.push_str("</header>\n\n");

    // Nav
    out.push_str("<nav class=\"toc\">\n<h2>Contents</h2>\n<ul>\n");
    out.push_str("<li><a href=\"#common\">Common Parameters</a></li>\n");
    for m in machines {
        let id = machine_anchor(m);
        out.push_str(&format!("<li><a href=\"#{id}\">{}</a></li>\n", m.label()));
    }
    out.push_str("</ul>\n</nav>\n\n");

    // Common table
    out.push_str("<section id=\"common\">\n");
    out.push_str("<h2>Common Parameters <span class=\"badge\">track-routed</span></h2>\n");
    out.push_str("<p>Same meaning on every machine. Values are factory defaults (0..1).</p>\n");
    out.push_str("<table>\n<thead>\n<tr><th>Slot</th><th>CC</th><th>Bank</th><th>Name</th><th>Abbrev</th><th>Default</th></tr>\n</thead>\n<tbody>\n");
    for row in common_rows {
        out.push_str(&html_row(row));
    }
    out.push_str("</tbody>\n</table>\n</section>\n\n");

    // Per-machine tables
    for m in machines {
        let rows = rows_for(Some(m), machine_slots);
        let id = machine_anchor(m);
        out.push_str(&format!("<section id=\"{id}\">\n"));
        out.push_str(&format!("<h2>{}</h2>\n", m.label()));
        out.push_str("<table>\n<thead>\n<tr><th>Slot</th><th>CC</th><th>Bank</th><th>Name</th><th>Abbrev</th><th>Default</th></tr>\n</thead>\n<tbody>\n");
        for row in &rows {
            out.push_str(&html_row(row));
        }
        out.push_str("</tbody>\n</table>\n</section>\n\n");
    }

    out.push_str("<footer>\n<p><strong>RESV</strong> = reserved/unused. CC = MIDI CC (20 + slot). Bank: MACH (0–7), FILT (8–15), TRACK (16–23), MOD (24–31).</p>\n");
    out.push_str("</footer>\n");
    out.push_str("</body>\n</html>\n");

    out
}

trait CheatsheetRow {
    fn slot(&self) -> usize;
    fn cc(&self) -> usize;
    fn bank(&self) -> &str;
    fn name(&self) -> &str;
    fn abbrev(&self) -> &str;
    fn default(&self) -> f32;
    fn is_resv(&self) -> bool;
}

fn html_row(row: &impl CheatsheetRow) -> String {
    let class = if row.is_resv() { " class=\"resv\"" } else { "" };
    let default = if row.is_resv() {
        "RESV".to_string()
    } else {
        format!("{:.2}", row.default())
    };
    format!(
        "<tr{}><td>{}</td><td>{}</td><td>{}</td><td>{}</td><td>{}</td><td>{}</td></tr>\n",
        class,
        row.slot(),
        row.cc(),
        row.bank(),
        row.name(),
        row.abbrev(),
        default
    )
}

fn machine_anchor(m: MachineId) -> String {
    m.name().replace('-', "_")
}

const CHEATSHEET_CSS: &str = r#"
:root {
  --bg: #0f1117;
  --surface: #1a1d27;
  --border: #2a2e3a;
  --text: #e0e0e6;
  --muted: #8b8fa3;
  --accent: #7aa2f7;
  --resv: #4a4e5a;
  --badge-bg: #2a3a5a;
  --badge-fg: #7aa2f7;
}
* { box-sizing: border-box; margin: 0; padding: 0; }
body {
  font-family: -apple-system, BlinkMacSystemFont, "Segoe UI", system-ui, sans-serif;
  background: var(--bg);
  color: var(--text);
  line-height: 1.6;
  max-width: 960px;
  margin: 0 auto;
  padding: 2rem 1.5rem;
}
header { margin-bottom: 2rem; }
h1 { font-size: 1.75rem; font-weight: 700; }
h2 { font-size: 1.25rem; font-weight: 600; margin-top: 2.5rem; margin-bottom: 0.75rem; }
p { color: var(--muted); margin-bottom: 1rem; }
code { font-family: "SF Mono", "Fira Code", monospace; font-size: 0.9em; }
.badge {
  display: inline-block;
  font-size: 0.7rem;
  font-weight: 600;
  padding: 0.15rem 0.5rem;
  border-radius: 4px;
  background: var(--badge-bg);
  color: var(--badge-fg);
  vertical-align: middle;
  margin-left: 0.5rem;
}
nav.toc {
  background: var(--surface);
  border: 1px solid var(--border);
  border-radius: 8px;
  padding: 1rem 1.5rem;
  margin-bottom: 2rem;
}
nav.toc h2 { margin-top: 0; font-size: 1rem; }
nav.toc ul { list-style: none; columns: 2; }
nav.toc a { color: var(--accent); text-decoration: none; font-size: 0.9rem; }
nav.toc a:hover { text-decoration: underline; }
section {
  background: var(--surface);
  border: 1px solid var(--border);
  border-radius: 8px;
  padding: 1.5rem;
  margin-bottom: 1.5rem;
}
table { width: 100%; border-collapse: collapse; font-size: 0.875rem; }
th, td { text-align: left; padding: 0.5rem 0.75rem; border-bottom: 1px solid var(--border); }
th { color: var(--muted); font-weight: 600; font-size: 0.8rem; text-transform: uppercase; letter-spacing: 0.05em; }
td { font-variant-numeric: tabular-nums; }
tr.resv { color: var(--resv); }
tr.resv td { font-style: italic; }
tr:hover { background: rgba(122, 162, 247, 0.05); }
footer { margin-top: 2rem; padding-top: 1rem; border-top: 1px solid var(--border); }
footer p { font-size: 0.8rem; }
"#;

/// Render a single hit with a tail, for auditioning one track.
fn render_one_shot(engine: &mut DrumEngine, track: usize, seconds: f32) -> Vec<f32> {
    engine.trigger(track, 1.0);

    let total_blocks = (seconds * SAMPLE_RATE / BLOCK as f32) as usize;
    let mut out = Vec::with_capacity(total_blocks * BLOCK * 2);
    let mut l = [0.0f32; BLOCK];
    let mut r = [0.0f32; BLOCK];

    for _ in 0..total_blocks {
        engine.process(&mut l, &mut r);
        for i in 0..BLOCK {
            out.push(l[i]);
            out.push(r[i]);
        }
    }
    out
}

/// A 16-step pattern per track. `true` means trigger.
#[derive(Default)]
struct Pattern {
    tracks: [[bool; 16]; TRACKS],
    /// Per-step semitone offsets, applied via [`Track::retune`] before each
    /// trigger. 0 = the track's macro pitch; anything else transposes that
    /// hit. All-zero (the default) is the pitchless kit. Row `i` only means
    /// anything when `tracks[i][s]` is `true`.
    notes: [[i8; 16]; TRACKS],
}

impl Pattern {
    /// A full 8-track groove: kick, snare, closed + open metallic hat, clap,
    /// tom fill, cowbell accent, and a tonal synth pulse.
    fn demo() -> Self {
        let mut p = Self::default();
        p.set_melody(
            7,
            &[
                (0, 0),
                (4, -3),
                (8, -5),
                (12, -3),
                (16, -2),
                (20, -3),
                (24, 0),
                (28, 5),
            ],
        );

        // 0 — BdClassic (kick): syncopated groove
        p.tracks[0] = [
            true, false, false, false, false, false, true, false, false, false, true, false, false,
            false, false, false,
        ];

        // 1 — SdNatural (snare): backbeat + ghost
        p.tracks[1] = [
            false, false, false, false, true, false, false, false, false, false, false, false,
            true, false, false, true,
        ];

        // 2 — HatClassic (closed hat): 8th notes + flam
        p.tracks[2] = [
            true, false, true, false, true, false, true, false, true, false, true, false, true,
            false, true, true,
        ];

        // 3 — HH Basic (metallic open hat): the "and" of beat 2
        p.tracks[3] = [
            false, false, false, false, false, true, false, false, false, false, false, false,
            false, false, false, false,
        ];

        // 4 — Cp (clap): doubles the second backbeat
        p.tracks[4] = [
            false, false, false, false, false, false, false, false, false, false, false, false,
            true, false, false, false,
        ];

        // 5 — Tom: end-of-bar fill
        p.tracks[5] = [
            false, false, false, false, false, false, false, false, false, false, false, false,
            false, false, true, true,
        ];

        // 6 — CbClassic (cowbell): off-beat accents
        p.tracks[6] = [
            false, false, true, false, false, false, true, false, false, false, true, false, false,
            false, true, false,
        ];

        // 7 — SyTone: bassline pulse on the downbeats + a high note
        p.tracks[7] = [
            true, false, false, false, true, false, false, false, true, false, false, false, true,
            false, false, false,
        ];

        p
    }

    /// Set a melody lane: `(step, semitones)` pairs. Steps run 0..32 across
    /// two bars; offsets are clamped into the 16-step row by step % 16.
    fn set_melody(&mut self, track: usize, notes: &[(usize, i8)]) {
        for &(step, semis) in notes {
            self.notes[track][step % 16] = semis;
        }
    }
}

/// Render a pattern, block by block, exactly as the firmware will.
///
/// Note the structure: the sequencer decides what to trigger *between*
/// blocks, never inside `process`. That is the same discipline the firmware
/// needs, so keeping it here means the host and target behave identically —
/// including the up-to-667µs of timing quantisation that block processing
/// imposes on trigger timing.
fn render_pattern(pattern: &Pattern, bpm: f32, bars: usize) -> Vec<f32> {
    let mut engine = DrumEngine::new();
    setup_kit_mix(&mut engine);

    let samples_per_step = (SAMPLE_RATE * 60.0 / bpm / 4.0) as usize;
    let total_steps = bars * 16;
    // A couple of seconds of tail so the last hit is not truncated.
    let total_samples = total_steps * samples_per_step + (2.0 * SAMPLE_RATE) as usize;
    let total_blocks = total_samples / BLOCK;

    let mut out = Vec::with_capacity(total_blocks * BLOCK * 2);
    let mut l = [0.0f32; BLOCK];
    let mut r = [0.0f32; BLOCK];

    let mut next_step = 0usize;
    let mut next_step_at = 0usize;

    for block in 0..total_blocks {
        let block_start = block * BLOCK;

        while next_step_at < block_start + BLOCK && next_step < total_steps {
            let s = next_step % 16;
            // Trigger whichever tracks have a step this row.
            let mut i = 0;
            while i < TRACKS {
                if pattern.tracks[i][s] {
                    let vel = default_velocity_for_track(i, s);
                    // Melodic lane: transpose the track to the step's note
                    // *before* the trigger so the hit lands in pitch.
                    let semis = pattern.notes[i][s];
                    if semis != 0 {
                        engine.tracks[i].retune(semis as f32);
                    }
                    engine.trigger(i, vel);
                }
                i += 1;
            }
            next_step += 1;
            next_step_at += samples_per_step;
        }

        engine.process(&mut l, &mut r);
        for i in 0..BLOCK {
            out.push(l[i]);
            out.push(r[i]);
        }
    }

    out
}

/// Render a retrigger/choke stress test: sample-accurate dense rolls that
/// repeatedly land on sounding voices (the "lots of notes quickly" click
/// scenario), including the closed-hat hitting on top of a ringing open hat
/// so the choke fade is exercised every iteration.
fn render_stress(seconds: f32) -> Vec<f32> {
    let mut engine = DrumEngine::new();
    setup_kit_mix(&mut engine);

    let total_samples = (seconds * SAMPLE_RATE) as usize;
    let total_blocks = total_samples / BLOCK;
    let mut out = Vec::with_capacity(total_blocks * BLOCK * 2);
    let mut l = [0.0f32; BLOCK];
    let mut r = [0.0f32; BLOCK];

    // (track, trigger period in samples): kick ~137 Hz roll, snare roll,
    // closed hat ~107 Hz roll (each hit chokes the ringing open hat), open
    // hat re-triggered every ~38 ms so there is always a tail to cut, and a
    // fast SyTone pulse.
    let schedule = [
        (0, 350usize),
        (1, 300usize),
        (2, 450usize),
        (3, 1800usize),
        (7, 400usize),
    ];
    let mut next = [0usize; TRACKS];

    for block in 0..total_blocks {
        let block_start = block * BLOCK;
        for &(track, period) in &schedule {
            while next[track] < block_start + BLOCK {
                engine.trigger(track, 1.0);
                next[track] += period;
            }
        }
        engine.process(&mut l, &mut r);
        for i in 0..BLOCK {
            out.push(l[i]);
            out.push(r[i]);
        }
    }

    out
}

/// Configure per-track strip settings (pan, level, filter, drive) and a few
/// macro tweaks so the kit sounds balanced across the stereo field rather
/// than all-centre, all-equal. Preserves the choke/layer masks already set
/// by `DrumEngine::new` (open-hat ↔ closed-hat relation).
fn setup_kit_mix(engine: &mut DrumEngine) {
    use drum_engine::dsp::SvfMode;
    use drum_engine::StripParams;

    // (track, pan, level, filter_mode, cutoff_hz, reso_q, drive)
    let configs: [(usize, f32, f32, SvfMode, f32, f32, f32); TRACKS] = [
        (0, 0.00, 0.95, SvfMode::Off, 1000.0, 0.707, 3.0), // kick — centre
        (1, -0.25, 0.75, SvfMode::Off, 1000.0, 0.707, 1.0), // snare — slightly L
        (2, 0.35, 0.45, SvfMode::Off, 1000.0, 0.707, 1.0), // closed hat — R
        (3, 0.40, 0.40, SvfMode::Off, 1000.0, 0.707, 1.0), // HH Basic metallic — R
        (4, -0.35, 0.65, SvfMode::Off, 1000.0, 0.707, 1.0), // clap — L
        (5, -0.50, 0.75, SvfMode::Lp, 3000.0, 0.707, 1.0), // tom — far L, warm LP
        (6, 0.15, 0.50, SvfMode::Off, 1000.0, 0.707, 1.0), // cowbell — near centre
        (7, -0.10, 0.55, SvfMode::Off, 1000.0, 0.707, 1.2), // SY Tone — near centre, drive
    ];

    for &(track, pan, level, f_mode, f_cutoff_hz, f_reso_q, drive) in &configs {
        let choke = engine.tracks[track].strip.choke_mask;
        let layer = engine.tracks[track].strip.layer_mask;
        let strip = StripParams {
            f_mode,
            f_cutoff_hz,
            f_reso_q,
            drive,
            pan,
            level,
            choke_mask: choke,
            layer_mask: layer,
            ..StripParams::default()
        };
        engine.tracks[track].set_strip(&strip);
    }

    // Macro tweaks — a few beyond the defaults to get more musical results:

    // Kick: Driven with decay
    engine.tracks[0].set_macro(SLOT_MACH_5, 0.75); // DEC
    engine.tracks[0].set_macro(SLOT_MACH_7, 0.5); // DRIVE

    // Clap: slightly more body, less pure noise (BAL default 0.80 noise-heavy)
    engine.tracks[4].set_macro(SLOT_MACH_7, 0.65); // BAL

    // Tom: tune lower (~132 Hz, a mid tom) and add a bit more stick
    engine.tracks[5].set_macro(SLOT_MACH_0, 0.50); // TUNE → ~132 Hz
    engine.tracks[5].set_macro(SLOT_MACH_7, 0.40); // STICK

    // Cowbell: classic 808 tuning — ~540 Hz base, wider detune, short decay
    engine.tracks[6].set_macro(SLOT_MACH_0, 0.34); // TUNE → ~538 Hz
    engine.tracks[6].set_macro(SLOT_MACH_1, 0.86); // DET → ~1.43 ratio (the classic 540/800 pair)
    engine.tracks[6].set_macro(SLOT_MACH_5, 0.15); // DEC → ~85ms

    // SY Tone: pitch it as a bassline — low note, moderate FM
    engine.tracks[7].set_macro(SLOT_MACH_0, 0.20); // TUNE → ~132 Hz (bass range)
    engine.tracks[7].set_macro(SLOT_MACH_3, 0.50); // MOD.AMT
    engine.tracks[7].set_macro(SLOT_MACH_2, 0.30); // FDBK
    engine.tracks[7].set_macro(SLOT_MACH_5, 0.40); // DEC → ~830ms (rings a bit)

    // --- LFO modulation demo ---
    // Snare (track 1): slow sine LFO on a LP filter, adds movement.
    engine.tracks[1].mod_state.lfos[0].set_params(
        0.3, // 0.3 Hz — very slow sweep
        drum_engine::dsp::LfoWave::Sine,
        drum_engine::dsp::LfoMode::Trig,
        0.5, // depth: ±0.5
        drum_engine::dsp::ModDest::FilterCutoff,
        0.0,
    );
    // The snare needs a filter for the LFO to act on.
    let snare_strip = drum_engine::StripParams {
        f_mode: drum_engine::dsp::SvfMode::Lp,
        f_cutoff_hz: 3000.0,
        f_reso_q: 1.5,
        pan: -0.25,
        level: 0.75,
        ..drum_engine::StripParams::default()
    };
    engine.tracks[1].set_strip(&snare_strip);

    // --- Velocity modulation demo ---
    // Kick (track 0): velocity → drive. Hard hits are gutsier.
    engine.tracks[0].mod_state.vel_mods[0] = drum_engine::VelMod {
        dest: drum_engine::dsp::ModDest::Drive,
        depth: 0.3,
    };

    // --- Send FX demo (Phase 5) ---
    // Snare (track 1): send to reverb for ambient backbeat space.
    engine.tracks[1].strip.send_reverb = 0.5;
    engine.tracks[1].strip.send_delay = 0.25;
    // Clap (track 4): send to delay for a reggae-ish ghost echo.
    engine.tracks[4].strip.send_delay = 0.25;
    // Cowbell (track 6): light reverb send for an "other room" accent.
    engine.tracks[6].strip.send_reverb = 0.5;

    // Refresh the strip caches so the per-block fast path (no mod active for
    // clap/cowbell) sees the new send levels. Copy each strip before calling
    // `set_strip` so we don't simultaneously borrow the track mut and immut.
    for t in [1usize, 4, 6] {
        let strip = engine.tracks[t].strip;
        engine.tracks[t].set_strip(&strip);
    }

    // FX bus configuration: a medium hall reverb and a slap-back delay.
    engine.send_fx.delay.set_params(0.346, 0.45, 5_000.0, 1.0);
    engine.send_fx.reverb.set_params(0.022, 0.86, 4_500.0, 1.0);
    engine.send_fx.drive = 1.0;
}

/// Per-row accent patterns so the kit pattern sounds less robotic on the
/// demo. Maps track index → step → velocity.
fn default_velocity_for_track(track: usize, step: usize) -> f32 {
    match (track, step) {
        // Kick — strong on downbeats, softer on syncopation
        (0, 0) | (0, 8) => 0.95,
        (0, 6) | (0, 10) => 0.82,
        // Snare — backbeat strong, ghost note soft
        (1, 4) | (1, 12) => 0.90,
        (1, 15) => 0.50,
        // Closed hat — accent on even steps (downbeats), lighter off-beats
        (2, _) if step.is_multiple_of(4) => 0.85,
        (2, 15) => 0.70, // flam at the end
        (2, _) => 0.50,
        // HH Basic metallic hat
        (3, _) => 0.75,
        // Clap
        (4, _) => 0.85,
        // Tom fill — crescendo into the next bar
        (5, 14) => 0.70,
        (5, 15) => 0.85,
        // Cowbell — short and punchy
        (6, _) => 0.65,
        // SY Tone — bassline pulse, steady
        (7, _) => 0.80,
        _ => 0.80,
    }
}

/// Write interleaved stereo f32 to a 24-bit WAV.
fn write_wav(path: &str, interleaved: &[f32]) -> Result<(), Box<dyn std::error::Error>> {
    let spec = hound::WavSpec {
        channels: 2,
        sample_rate: SAMPLE_RATE as u32,
        bits_per_sample: 24,
        sample_format: hound::SampleFormat::Int,
    };

    let mut writer = hound::WavWriter::create(path, spec)?;
    let scale = (1i32 << 23) as f32 - 1.0;

    let mut clipped = 0usize;
    for &s in interleaved {
        if s.abs() > 1.0 {
            clipped += 1;
        }
        writer.write_sample((s.clamp(-1.0, 1.0) * scale) as i32)?;
    }
    writer.finalize()?;

    if clipped > 0 {
        eprintln!("warning: {clipped} samples clipped — the engine should not allow this");
    }
    Ok(())
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
                                engine.trigger(i, default_velocity_for_track(i, s));
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
