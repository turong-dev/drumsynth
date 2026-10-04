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
#[cfg(feature = "live")]
mod slack;

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
    /// Render the mi-drum baseline to a WAV file.
    ///
    /// A fixed, deterministic pass over the mi-drum device, in two halves:
    /// one hit per catalogued Plaits machine on track 0, then a six-track
    /// pattern on the default kit with the strip fully exercised (filter on,
    /// drive, pan spread, sends). The second half is the point — Phase 14
    /// replaces the strip stages, and a baseline that left them at their
    /// defaults would not notice.
    ///
    /// This is the reference render for Phase 14.0's bit-identity gate. It
    /// takes no tuning arguments on purpose: a gate you can accidentally
    /// re-parameterise is not a gate.
    MiDrum {
        /// Output path.
        #[arg(short, long, default_value = "mi-drum.baseline.wav")]
        output: String,
        /// Phase 14 spike: force an MI stage on every track. `none` (the
        /// default, and what the pinned baseline digest covers), `lpg`,
        /// `overdrive`, or `resonator`. Anything but `none` changes the sound
        /// on purpose and will not match the baseline.
        #[arg(long, default_value = "none")]
        stage: String,
    },
    /// Render a gate open/close demo: every engine that sustains, one at a
    /// time, each with an explicit note-on, hold, note-off and release.
    ///
    /// This is the listenable version of the note-off work. `mi-drum` buries
    /// its sustain pass between the machine sweep and the carrier pass, two
    /// engines deep; this puts every gated engine in sequence with silence
    /// around it, so a gate that never closes is obvious by ear.
    ///
    /// The one-shot drum machines are in the list on purpose, as a control: a
    /// note-off must not shorten them.
    Gate {
        /// Output path.
        #[arg(short, long, default_value = "gate-demo.wav")]
        output: String,
    },
    /// Render the Warps drive sweep: one held note per drive setting, from the
    /// clean bypass to full destruction.
    ///
    /// This is the listenable version of `WARPS_DRIVE_INFO`. Warps' own drive
    /// knob is `0.5·drive` blended towards `24·drive⁵`, so its top half covers
    /// 48× of gain and `drive = 0` is silence rather than clean — neither is
    /// visible in a number, and both are the difference between a kit that
    /// sounds like itself and one that does not.
    /// A/B the ADAA waveshaper against the vendored Warps modulator.
    ///
    /// Two files. The kit comparison is the musical question — does the
    /// cheaper stage still sound like the instrument. The alias test is the
    /// honest one: first-order ADAA *attenuates* aliasing rather than
    /// removing it, and drums hide aliasing well, so the stage is also put
    /// under a pitched sweep at full drive and full wet, which is where a
    /// cheap antialiasing scheme fails audibly if it is going to.
    Shaper {
        /// Output path.
        #[arg(short, long, default_value = "shaper-ab.wav")]
        output: String,
    },
    Warps {
        /// Output path.
        #[arg(short, long, default_value = "warps-drive.wav")]
        output: String,
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
    /// Read the firmware's headroom report off its USB MIDI port.
    ///
    /// The real measure of whether the engine fits: cycles from a block
    /// boundary to that block's render completing, on the real binary, as a
    /// fraction of the 400,000-cycle block period. `mi-bench` times
    /// `engine.process()` with USB interrupts off, nothing on MIDI, no grid
    /// and a warm cache — this includes the SAI ISR preempting the render,
    /// the main loop's polling, the grid pass and the cache damage all of
    /// that does. Those are exactly what the informal `~70%` ceiling stands
    /// in for, and what nothing has ever measured.
    ///
    /// Drive the board hard while this runs — every track sounding, MIDI
    /// streaming in, the grid active — and watch the worst case.
    #[cfg(feature = "live")]
    Slack {
        /// Substring of the MIDI input port name. Defaults to "Teensy", then
        /// to the first port available.
        #[arg(short, long)]
        port: Option<String>,
        /// Stop after this many seconds instead of running until Ctrl-C.
        #[arg(short, long)]
        seconds: Option<u64>,
        /// Retrigger all six tracks at this rate (Hz) while measuring, rather
        /// than waiting for someone to play the board. Worst case has to be
        /// produced, not waited for.
        #[arg(long)]
        drive_hz: Option<f32>,
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
            let drum_path = output.replacen(".wav", "-drum.wav", 1);
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

            let alias_path = output.replacen(".wav", "-alias.wav", 1);
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
            let kit_path = output.replacen(".wav", "-kit.wav", 1);
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

        #[cfg(feature = "live")]
        Command::Slack {
            port,
            seconds,
            drive_hz,
        } => slack::run(port.as_deref(), seconds, drive_hz)?,
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
///
/// A sustained machine (Dub Siren, Sweep FX) is released partway through the
/// window rather than left to ring out the gate watchdog, so the audition
/// shows the release and the file still terminates when it says it will. A
/// one-shot machine ignores the release, so this changes nothing for it.
fn render_one_shot(engine: &mut DrumEngine, track: usize, seconds: f32) -> Vec<f32> {
    engine.trigger(track, 1.0);

    let total_blocks = (seconds * SAMPLE_RATE / BLOCK as f32) as usize;
    let mut out = Vec::with_capacity(total_blocks * BLOCK * 2);
    let mut l = [0.0f32; BLOCK];
    let mut r = [0.0f32; BLOCK];

    for b in 0..total_blocks {
        if b == total_blocks / 2 {
            engine.release(track);
        }
        engine.process(&mut l, &mut r);
        for i in 0..BLOCK {
            out.push(l[i]);
            out.push(r[i]);
        }
    }

    out
}

/// Per-track strip settings for the mi-drum kit pattern.
///
/// `(warps timbre, ripples cutoff, ripples resonance, warps drive, lfo rate,
///  lfo depth, lfo->filter, lfo->warps, ad attack, ad decay, ad->filter,
///  ad->warps, warps oscillator shape)`
///
/// Depths stay moderate: the full-scale end of the filter depth is three
/// octaves, and six tracks all modulating at once drives the master sum into
/// the clipper, which would make the baseline a test of the limiter rather
/// than of the modulation map.
///
/// The last field is the shape of the strip's modulator oscillator, which
/// only sounds when a track's `WARP.IN` selects it. The kit leaves `WARP.IN`
/// at its default, so these are inert here; the oscillator gets a dedicated
/// pass instead.
///
/// Shared by the baseline's bypassed and engaged passes, which have to drive
/// the *same* kit or the A/B is not a comparison.
type MiStripSpec = (
    f32,
    f32,
    f32,
    f32,
    f32,
    f32,
    f32,
    f32,
    f32,
    f32,
    f32,
    f32,
    f32,
);
const MI_KIT_STRIPS: [MiStripSpec; mi_drum_engine::TRACKS] = [
    (
        0.40, 0.55, 0.20, 0.60, 0.45, 0.80, 0.30, 0.20, 0.05, 0.35, 0.25, 0.20, 0.0,
    ),
    (
        0.55, 0.35, 0.40, 0.75, 0.55, 0.60, 0.20, 0.15, 0.10, 0.50, 0.20, 0.15, 0.0,
    ),
    (
        0.30, 0.80, 0.10, 0.50, 0.35, 0.90, 0.35, 0.10, 0.02, 0.25, 0.15, 0.10, 0.0,
    ),
    (
        0.65, 0.45, 0.55, 0.80, 0.60, 0.70, 0.15, 0.30, 0.15, 0.60, 0.30, 0.20, 0.0,
    ),
    (
        0.25, 0.70, 0.30, 0.65, 0.40, 0.75, 0.25, 0.25, 0.08, 0.40, 0.20, 0.25, 0.0,
    ),
    (
        0.50, 0.60, 0.15, 0.55, 0.50, 0.65, 0.20, 0.15, 0.12, 0.30, 0.25, 0.15, 0.0,
    ),
];

/// A 16-step pattern, two bars at 130 BPM. Rows are kick / snare / hat /
/// modal / noise / string against the default kit.
const MI_KIT_PATTERN: [[bool; 16]; mi_drum_engine::TRACKS] = [
    [
        true, false, false, false, true, false, false, false, true, false, false, true, true,
        false, false, false,
    ],
    [
        false, false, false, false, true, false, false, false, false, false, false, false, true,
        false, true, false,
    ],
    [
        true, false, true, false, true, false, true, false, true, false, true, false, true, false,
        true, true,
    ],
    [
        false, false, true, false, false, false, false, true, false, false, true, false, false,
        false, false, false,
    ],
    [
        false, false, false, true, false, false, false, false, false, true, false, false, false,
        false, true, false,
    ],
    [
        true, false, false, false, false, false, true, false, false, false, false, false, true,
        false, false, false,
    ],
];

/// Apply [`MI_KIT_STRIPS`] to the engine's tracks.
///
/// `drive_override` replaces every track's `WARP.DRV`. `Some(0.0)` is the
/// detent that takes Warps out of the path via `Modulator::set_bypass`, which
/// is what the baseline's reference bar uses.
fn apply_mi_kit_strips(
    engine: &mut MiDrumEngine,
    warps_algorithm: f32,
    drive_override: Option<f32>,
) {
    use mi_drum_engine::{
        DeviceEngine, SLOT_AD_ATTACK, SLOT_AD_DECAY, SLOT_AD_FILTER_DEPTH, SLOT_AD_WARPS_DEPTH,
        SLOT_FILT_0, SLOT_FILT_1, SLOT_LFO_DEPTH, SLOT_LFO_FILTER_DEPTH, SLOT_LFO_RATE,
        SLOT_LFO_WARPS_DEPTH, SLOT_STRIP_CUT, SLOT_STRIP_HOLD, SLOT_STRIP_RESO,
        SLOT_WARPS_OSC_SHAPE,
    };

    for (idx, &spec) in MI_KIT_STRIPS.iter().enumerate() {
        let track = &mut engine.tracks_mut()[idx];
        track.set_macro(SLOT_FILT_0, warps_algorithm);
        track.set_macro(SLOT_FILT_1, spec.0);
        track.set_macro(SLOT_STRIP_CUT, spec.1);
        track.set_macro(SLOT_STRIP_RESO, spec.2);
        track.set_macro(SLOT_STRIP_HOLD, drive_override.unwrap_or(spec.3));
        track.set_macro(SLOT_LFO_RATE, spec.4);
        track.set_macro(SLOT_LFO_DEPTH, spec.5);
        track.set_macro(SLOT_LFO_FILTER_DEPTH, spec.6);
        track.set_macro(SLOT_LFO_WARPS_DEPTH, spec.7);
        track.set_macro(SLOT_AD_ATTACK, spec.8);
        track.set_macro(SLOT_AD_DECAY, spec.9);
        track.set_macro(SLOT_AD_FILTER_DEPTH, spec.10);
        track.set_macro(SLOT_AD_WARPS_DEPTH, spec.11);
        track.set_macro(SLOT_WARPS_OSC_SHAPE, spec.12);
    }
}

/// Play [`MI_KIT_PATTERN`] once through, appending to `out`.
///
/// `bars` of 16 steps at 130 BPM, then `tail_s` seconds for the last hit and
/// the send-FX decay.
fn play_mi_kit_pattern(engine: &mut MiDrumEngine, out: &mut Vec<f32>, bars: usize, tail_s: f32) {
    use mi_drum_engine::DeviceEngine;

    let mut l = [0.0f32; BLOCK];
    let mut r = [0.0f32; BLOCK];

    let bpm = 130.0f32;
    let samples_per_step = (SAMPLE_RATE * 60.0 / bpm / 4.0) as usize;
    let total_steps = bars * 16;
    let total_blocks = (total_steps * samples_per_step + (tail_s * SAMPLE_RATE) as usize) / BLOCK;

    let mut next_step = 0usize;
    let mut next_step_at = 0usize;
    for block in 0..total_blocks {
        let block_start = block * BLOCK;
        while next_step_at < block_start + BLOCK && next_step < total_steps {
            let s = next_step % 16;
            for (track, row) in MI_KIT_PATTERN.iter().enumerate() {
                if row[s] {
                    // Velocity varies with the step so the velocity-mod path
                    // is not stuck at full scale for the whole render.
                    let vel = if s.is_multiple_of(4) { 1.0 } else { 0.7 };
                    engine.trigger(track, vel);
                }
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
}

/// The mi-drum baseline render, with the offset of each section in seconds.
///
/// The marks exist so the Warps A/B inside the render is findable: it sits
/// 21 seconds in, behind the machine sweep, and a reference you have to hunt
/// for is a reference nobody uses.
fn render_mi_drum_marked(warps_algorithm: f32) -> (Vec<f32>, Vec<(f32, &'static str)>) {
    use mi_drum_engine::{
        DeviceEngine, MiMachineId, SLOT_AD_ATTACK, SLOT_AD_DECAY, SLOT_AD_FILTER_DEPTH,
        SLOT_AD_WARPS_DEPTH, SLOT_FILT_0, SLOT_FILT_1, SLOT_LFO_DEPTH, SLOT_LFO_FILTER_DEPTH,
        SLOT_LFO_RATE, SLOT_LFO_WARPS_DEPTH, SLOT_STRIP_CUT, SLOT_STRIP_HOLD, SLOT_STRIP_RESO,
        SLOT_WARPS_MOD_SRC, SLOT_WARPS_OSC_SHAPE,
    };

    // Every Plaits engine draws noise from one process-global LCG, so the
    // render is only reproducible from a known seed. This does not make two
    // concurrent renders safe — they would interleave their draws — which is
    // why the baseline test below is a single test doing a single render.
    mi_drum_engine::seed_random(mi_drum_engine::DEFAULT_RANDOM_SEED);

    let mut engine = mi_drum_in_place();
    let mut out: Vec<f32> = Vec::new();
    let mut marks: Vec<(f32, &'static str)> = Vec::new();
    let mut l = [0.0f32; BLOCK];
    let mut r = [0.0f32; BLOCK];
    macro_rules! mark {
        ($what:expr) => {
            marks.push((out.len() as f32 / 2.0 / SAMPLE_RATE, $what));
        };
    }

    mark!("machine sweep — one hit per catalogued machine");
    // Half one: one hit per machine on track 0, 0.75 s apart, so every engine
    // in the catalogue contributes to the hash.
    let blocks_per_hit = (0.75 * SAMPLE_RATE / BLOCK as f32) as usize;
    for &id in MiMachineId::ALL.iter() {
        engine.tracks_mut()[0].load_machine(id);
        engine.tracks_mut()[0].set_macro(SLOT_FILT_0, warps_algorithm);
        engine.tracks_mut()[0].set_macro(SLOT_FILT_1, 0.5);
        engine.tracks_mut()[0].set_macro(SLOT_STRIP_CUT, 0.5);
        engine.tracks_mut()[0].set_macro(SLOT_STRIP_RESO, 0.25);
        engine.tracks_mut()[0].set_macro(SLOT_STRIP_HOLD, 0.7);
        // Modulation live, so the Stages path is inside the hash.
        engine.tracks_mut()[0].set_macro(SLOT_LFO_RATE, 0.45);
        engine.tracks_mut()[0].set_macro(SLOT_LFO_DEPTH, 0.8);
        engine.tracks_mut()[0].set_macro(SLOT_AD_ATTACK, 0.05);
        engine.tracks_mut()[0].set_macro(SLOT_AD_DECAY, 0.35);
        engine.tracks_mut()[0].set_macro(SLOT_LFO_FILTER_DEPTH, 0.30);
        engine.tracks_mut()[0].set_macro(SLOT_LFO_WARPS_DEPTH, 0.20);
        engine.tracks_mut()[0].set_macro(SLOT_AD_FILTER_DEPTH, 0.25);
        engine.tracks_mut()[0].set_macro(SLOT_AD_WARPS_DEPTH, 0.20);
        engine.trigger(0, 1.0);
        // Release at two-thirds so a sustained engine's release is in the
        // hash and the window still ends in silence. A one-shot model ignores
        // the release, so this only affects the engines that hold.
        let release_at = blocks_per_hit * 2 / 3;
        for b in 0..blocks_per_hit {
            if b == release_at {
                engine.release(0);
            }
            engine.process(&mut l, &mut r);
            for i in 0..BLOCK {
                out.push(l[i]);
                out.push(r[i]);
            }
        }
    }

    // Half two: back to the default kit, with per-track Warps/Ripples settings
    // so the strip is actually exercised.
    engine.load_kit(&mi_drum_engine::DEFAULT_KIT);

    // Half two-a: the kit with Warps bypassed, so the baseline carries its own
    // reference for what the strip is doing. The settings themselves are
    // documented on `MI_KIT_STRIPS`.
    //
    // `WARP.DRV = 0` is a detent onto `Modulator::set_bypass`, not a quiet
    // drive setting. It is not the only uncoloured setting any more —
    // `WARP.MIX = 0` is too — but it is the one that takes the whole stage out
    // rather than mixing it away, which is what makes it the right reference.
    // One bar bypassed and then the same bar as the kit is actually set makes
    // the difference audible instead of a claim in a comment, and if the two
    // bars ever sound the same the strip has stopped working.
    mark!("kit, WARPS BYPASSED (WARP.DRV 0) — the reference");
    apply_mi_kit_strips(&mut engine, warps_algorithm, Some(0.0));
    play_mi_kit_pattern(&mut engine, &mut out, 1, 0.75);

    // Half two-b: the same bars with each track's own `WARP.DRV`, which the
    // kit sets between 0.50 and 0.80 — Warps drive 0.75 to 0.90, three to ten
    // times overdriven.
    mark!("kit, WARPS ENGAGED (WARP.DRV 0.50-0.80) — same bars");
    apply_mi_kit_strips(&mut engine, warps_algorithm, None);
    play_mi_kit_pattern(&mut engine, &mut out, 2, 2.0);

    mark!("gate — held notes and their releases");
    // Half three: the gate itself. A held note and its release, on the two
    // engines that read the Plaits gate as a level rather than an edge. This
    // is the capability the whole note-off path exists for, so it belongs in
    // the baseline rather than only in a unit test: before the gate was real,
    // `SixOp1`/`2`/`3` emitted digital silence for an entire hit and three of
    // the 28 machine windows in half one were exactly -inf.
    for &id in &[
        mi_drum_engine::MiMachineId::SixOp1,
        mi_drum_engine::MiMachineId::SixOp3,
    ] {
        engine.tracks_mut()[0].load_machine(id);
        engine.trigger(0, 1.0);

        // Two seconds held, then two seconds of tail after the release.
        for &do_release in &[false, true] {
            if do_release {
                engine.release(0);
            }
            for _ in 0..(2.0 * SAMPLE_RATE / BLOCK as f32) as usize {
                engine.process(&mut l, &mut r);
                for j in 0..BLOCK {
                    out.push(l[j]);
                    out.push(r[j]);
                }
            }
        }
    }

    mark!("Warps modulator oscillator — five shapes against the kick");
    // Half four: the strip's own oscillator on Warps' modulator input, one
    // shape per hit, on the kick.
    //
    // This pass used to sweep `WARP.CAR`, which put the same five shapes on
    // Warps' *carrier* input — where they replaced the voice rather than
    // modulating it, so the kick became a drone for the length of its own
    // decay and the machine was only an FM index. On the modulator side the
    // kick stays the kick and the oscillator ring-modulates it, which is the
    // thing worth auditioning. A ring-mod algorithm, because that is where a
    // modulator is most plainly audible.
    engine.tracks_mut()[0].load_machine(mi_drum_engine::MiMachineId::PeaksBassDrum);
    engine.tracks_mut()[0].set_macro(SLOT_WARPS_MOD_SRC, 1.0); // oscillator
    engine.tracks_mut()[0].set_macro(SLOT_FILT_0, 0.25); // analog ring mod
    engine.tracks_mut()[0].set_macro(SLOT_STRIP_HOLD, 0.4);
    for step in 0..5 {
        engine.tracks_mut()[0].set_macro(SLOT_WARPS_OSC_SHAPE, step as f32 / 5.0 + 0.05);
        engine.trigger(0, 1.0);
        for b in 0..blocks_per_hit {
            if b == blocks_per_hit * 2 / 3 {
                engine.release(0);
            }
            engine.process(&mut l, &mut r);
            for i in 0..BLOCK {
                out.push(l[i]);
                out.push(r[i]);
            }
        }
    }
    engine.tracks_mut()[0].set_macro(SLOT_WARPS_OSC_SHAPE, 0.0);

    (out, marks)
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
/// A one-entry manifest of the gate demo, printed alongside the WAV so the
/// file is navigable without scrubbing.
type GateTimeline = Vec<(f32, String, &'static str)>;

/// Render the gate open/close demo for mi-drum.
///
/// Each engine gets: a short silence, a note-on, a two-second hold, a note-off,
/// then two seconds for the release to run out. The hold is what the gate fix
/// makes possible — before it, the Plaits trigger was a one-block pulse and
/// these voices died 0.5 ms in.
///
/// The three `SixOp` engines lead, because they were the ones emitting digital
/// silence. `chiptune` is last and labelled, because measurement says it is
/// the one Plaits engine that ignores a falling gate entirely, and a demo that
/// hid that would be dishonest about the state of the work.
fn render_gate_demo_mi() -> (Vec<f32>, GateTimeline) {
    use mi_drum_engine::{DeviceEngine, MiMachineId, BLOCK};

    // Every Plaits engine draws from one process-global LCG, so the render is
    // only reproducible from a known seed.
    mi_drum_engine::seed_random(mi_drum_engine::DEFAULT_RANDOM_SEED);

    let hold_s = 2.0f32;
    let release_s = 2.0f32;
    let gap_s = 0.35f32;
    let per_engine = hold_s + release_s + gap_s;

    // (machine, why it is in the list)
    let entries: &[(MiMachineId, &str)] = &[
        (
            MiMachineId::SixOp1,
            "was DIGITAL SILENCE before the gate was real",
        ),
        (
            MiMachineId::SixOp2,
            "was DIGITAL SILENCE before the gate was real",
        ),
        (
            MiMachineId::SixOp3,
            "was DIGITAL SILENCE before the gate was real",
        ),
        (
            MiMachineId::String,
            "sustained engine; the gate hold is the note",
        ),
        (
            MiMachineId::Modal,
            "sustained engine; the gate hold is the note",
        ),
        (
            MiMachineId::VirtualAnalog,
            "outer LPG; release follows MACH 7 decay",
        ),
        (
            MiMachineId::PeaksBassDrum,
            "PEAKS drum: gate is per-sample flags",
        ),
        (
            MiMachineId::PeaksSnareDrum,
            "PEAKS drum: gate is per-sample flags",
        ),
        (
            MiMachineId::BassDrum,
            "one-shot: the note-off must NOT cut it",
        ),
        (MiMachineId::HiHat, "one-shot: the note-off must NOT cut it"),
        (
            MiMachineId::Chiptune,
            "ignores a falling gate - rings on past it",
        ),
    ];

    let mut engine = mi_drum_engine::MiDrumEngine::new();
    // Headroom. At the default master gain every voice in this list hits the
    // output clipper, which flattens exactly the envelope shape the demo
    // exists to show. Backing the master off keeps the hold and the release
    // both audible as level rather than as time spent against a limiter.
    engine.set_master_gain(0.35);
    let mut out: Vec<f32> =
        Vec::with_capacity((entries.len() as f32 * per_engine * SAMPLE_RATE * 2.0) as usize);
    let mut l = [0.0f32; BLOCK];
    let mut r = [0.0f32; BLOCK];
    let mut timeline = GateTimeline::new();

    let blocks = |s: f32| (s * SAMPLE_RATE / BLOCK as f32) as usize;

    for (i, (id, why)) in entries.iter().enumerate() {
        // Start each engine on a fresh voice so the previous release cannot
        // bleed into the silence that frames this one.
        engine.tracks_mut()[0].load_machine(*id);
        timeline.push((i as f32 * per_engine, id.name().to_string(), why));

        // Leading silence, so the note-on is an attack you can hear start.
        for _ in 0..blocks(gap_s) {
            engine.process(&mut l, &mut r);
            out.extend_from_slice(&l);
            out.extend_from_slice(&r);
        }

        // Note-on, then hold.
        engine.trigger(0, 1.0);
        for _ in 0..blocks(hold_s) {
            engine.process(&mut l, &mut r);
            out.extend_from_slice(&l);
            out.extend_from_slice(&r);
        }

        // Note-off, then let the release run.
        engine.release(0);
        for _ in 0..blocks(release_s) {
            engine.process(&mut l, &mut r);
            out.extend_from_slice(&l);
            out.extend_from_slice(&r);
        }
    }

    (out, timeline)
}

/// Render the gate open/close demo for the drum device.
///
/// [`DubSiren`](drum_engine::machines::DubSiren) and
/// [`SweepFx`](drum_engine::machines::SweepFx) were the two self-timed gesture
/// machines and are now gated, so they belong in the same demo as the mi-drum
/// voices. The rest of the catalogue is here as a control: a note-off must not
/// shorten a one-shot drum hit.
fn render_gate_demo_drum() -> (Vec<f32>, GateTimeline) {
    use drum_engine::{DeviceEngine, MachineId};

    let hold_s = 2.0f32;
    let release_s = 2.0f32;
    let gap_s = 0.35f32;
    let per_engine = hold_s + release_s + gap_s;

    let entries: &[(MachineId, &str)] = &[
        (
            MachineId::DubSiren,
            "was self-timed; now holds until note-off",
        ),
        (
            MachineId::SweepFx,
            "was self-timed; now holds until note-off",
        ),
        (
            MachineId::BdClassic,
            "one-shot kick: the note-off must NOT cut it",
        ),
        (
            MachineId::SdNatural,
            "one-shot snare: the note-off must NOT cut it",
        ),
        (
            MachineId::CyMetallic,
            "one-shot cymbal: the note-off must NOT cut it",
        ),
    ];

    let mut engine = DrumEngine::new();
    // Headroom, for the same reason as the mi-drum demo.
    engine.set_master_gain(0.7);
    let mut out: Vec<f32> =
        Vec::with_capacity((entries.len() as f32 * per_engine * SAMPLE_RATE * 2.0) as usize);
    let mut l = [0.0f32; BLOCK];
    let mut r = [0.0f32; BLOCK];
    let mut timeline = GateTimeline::new();

    let blocks = |s: f32| (s * SAMPLE_RATE / BLOCK as f32) as usize;

    for (i, (id, why)) in entries.iter().enumerate() {
        engine.tracks[0].load_machine(*id);
        timeline.push((i as f32 * per_engine, id.name().to_string(), why));

        for _ in 0..blocks(gap_s) {
            engine.process(&mut l, &mut r);
            out.extend_from_slice(&l);
            out.extend_from_slice(&r);
        }
        engine.trigger(0, 1.0);
        for _ in 0..blocks(hold_s) {
            engine.process(&mut l, &mut r);
            out.extend_from_slice(&l);
            out.extend_from_slice(&r);
        }
        engine.release(0);
        for _ in 0..blocks(release_s) {
            engine.process(&mut l, &mut r);
            out.extend_from_slice(&l);
            out.extend_from_slice(&r);
        }
    }

    (out, timeline)
}

/// Render the default kit twice: as shipped, then with Warps bypassed on the
/// four drum tracks.
///
/// This is the decision the drive sweep sets up but cannot answer on its own.
/// Warps' `drive` doubles as the wet/dry mix, so "a lot of Warps" and "almost no
/// dry signal" are the same setting — there is no in-between where a track is
/// coloured but still mostly itself. On a simple kick that reads as character;
/// on a six-operator FM voice, cross-modulating it with itself turns 19
/// partials into hundreds of inharmonic sum-and-difference products, which is
/// what "gritty" is.
///
/// So the split is per track: drum tracks bypassed, melodic tracks left alone.
fn render_warps_kit_comparison() -> (Vec<f32>, Vec<(f32, String)>) {
    use drum_engine::machines::SLOT_STRIP_HOLD as WARP_DRV_SLOT;
    use mi_drum_engine::{DeviceEngine, BLOCK, SAMPLE_RATE as SR};

    mi_drum_engine::seed_random(mi_drum_engine::DEFAULT_RANDOM_SEED);

    let bpm = 130.0f32;
    let step_s = 60.0 / bpm / 4.0; // 16th notes
    let steps = 16usize;
    let bars = 2usize;
    let tail_s = 2.0f32;
    let total_s = steps as f32 * step_s * bars as f32 + tail_s;

    // A plain backbeat so the difference is the strip, not the groove.
    const PATTERN: [[bool; 16]; 6] = [
        [
            true, false, false, false, true, false, false, false, true, false, false, true, true,
            false, false, false,
        ],
        [
            false, false, false, false, true, false, false, false, false, false, false, false,
            true, false, true, false,
        ],
        [
            true, false, true, false, true, false, true, false, true, false, true, false, true,
            false, true, true,
        ],
        [
            false, false, true, false, false, false, false, true, false, false, true, false, false,
            false, false, false,
        ],
        [
            false, false, false, true, false, false, false, false, false, true, false, false,
            false, false, true, false,
        ],
        [
            true, false, false, false, false, false, true, false, false, false, false, false, true,
            false, false, false,
        ],
    ];

    let total_steps = steps * bars;
    let total_blocks = (total_s * SR / BLOCK as f32) as usize;
    let mut out: Vec<f32> = Vec::with_capacity(total_blocks * BLOCK * 2 * 2);
    let mut notes: Vec<(f32, String)> = Vec::new();

    for bypass_drums in [false, true] {
        let mut engine = mi_drum_engine::MiDrumEngine::new();
        engine.set_master_gain(0.5);
        if bypass_drums {
            // Tracks 0-3 are the Peaks drums in the default kit, 4-5 the Plaits
            // voices. 0.0 is a true bypass, not a zero drive.
            for t in 0..4usize {
                engine.tracks_mut()[t].set_macro(WARP_DRV_SLOT, 0.0);
            }
        }

        notes.push((
            out.len() as f32 / 2.0 / SR,
            if bypass_drums {
                "B - drum tracks BYPASSED, melodic tracks still warped".to_string()
            } else {
                "A - default kit as shipped (all tracks 60% warped)".to_string()
            },
        ));

        let mut next_step = 0usize;
        let mut next_step_at = 0.0f32;
        let mut l = [0.0f32; BLOCK];
        let mut r = [0.0f32; BLOCK];

        for b in 0..total_blocks {
            let block_end = (b + 1) as f32 * BLOCK as f32 / SR;
            while next_step < total_steps && next_step_at < block_end {
                let s = next_step % steps;
                for (track, row) in PATTERN.iter().enumerate() {
                    if row[s] {
                        engine.trigger(track, if s.is_multiple_of(4) { 1.0 } else { 0.7 });
                    }
                }
                next_step += 1;
                next_step_at += step_s;
            }
            engine.process(&mut l, &mut r);
            out.extend_from_slice(&l);
            out.extend_from_slice(&r);
        }
    }

    (out, notes)
}

/// Print a manifest next to its WAV so the file is navigable without scrubbing.
fn print_gate_timeline(path: &str, samples: usize, timeline: &GateTimeline) {
    println!(
        "\nwrote {path} ({:.2}s)",
        samples as f32 / 2.0 / SAMPLE_RATE
    );
    println!("  time    engine          what you are hearing");
    println!("  ------  --------------  ----------------------------------------");
    for (t, name, what) in timeline {
        println!("  {t:>5.1}s  {name:<14}  {what}");
    }
}

/// Render the Warps drive sweep: the kick at each point on the `WARP.DRV` axis,
/// from the clean bypass to full destruction.
///
/// Every setting gets the same eight hits at a steady tempo, so the only thing
/// changing between them is Warps.
///
/// # Why a kick and not a tonal voice
///
/// The source has to be *clean* for the drive to be legible. Warps is a
/// saturating waveshaper, so what you hear is the harmonic structure of
/// whatever you feed it — and a six-operator FM voice is dense in partials from
/// the first millisecond. Warping one produces something already complex, and
/// the drive change is masked by the source rather than revealed by it.
///
/// A kick is a pitch-swept sine plus a click: nearly all of its energy sits in
/// one partial, so the shape of the transfer function is most of what you hear.
/// As the drive comes up the sine flattens toward a square, the click smears,
/// and the crest factor falls — three separate symptoms of the same change,
/// which is what makes the sweep readable at a glance.
fn render_warps_drive_demo() -> (Vec<f32>, GateTimeline) {
    use drum_engine::machines::SLOT_LEVEL;
    use drum_engine::machines::SLOT_STRIP_HOLD as WARP_DRV_SLOT;
    use mi_drum_engine::{DeviceEngine, MiMachineId, BLOCK, SAMPLE_RATE as SR};

    mi_drum_engine::seed_random(mi_drum_engine::DEFAULT_RANDOM_SEED);

    let hits = 8usize;
    let step_s = 0.25f32; // 8th notes at 120 BPM
    let gap_s = 0.45f32;
    let per_setting = hits as f32 * step_s + gap_s;

    // (drive macro, what it should sound like)
    let entries: &[(f32, &str)] = &[
        (0.0, "BYPASS - bit transparent, kick uncoloured"),
        (0.1, "light - Warps barely engaged, pre-gain ~0.4"),
        (0.25, "unity - cleanest saturation point, pre-gain ~1.0"),
        (0.5, "3x overdriven - the old default sat here"),
        (0.75, "hard - pre-gain ~10, sine visibly flattening"),
        (1.0, "destroyed - Warps at full drive, pre-gain 24"),
    ];

    let mut engine = mi_drum_engine::MiDrumEngine::new();
    // Headroom. The top of this sweep is loud by design and the point is to hear
    // the drive, not the limiter.
    engine.set_master_gain(0.3);

    // FILT 5 is `WARP.DRV` on mi-drum. Re-exported from `drum_engine` only so
    // this reads next to `SLOT_LEVEL`, which is genuinely per-device.
    {
        let track = &mut engine.tracks_mut()[0];
        track.load_machine(MiMachineId::PeaksBassDrum);
        // The Peaks kick peaks at full scale by design (see
        // docs/peaks-vendoring.md), which would slam Warps' input and hide the
        // transfer curve. Back it off so Warps has somewhere to go.
        track.set_macro(SLOT_LEVEL, 0.5);
    }

    let mut out: Vec<f32> =
        Vec::with_capacity((entries.len() as f32 * per_setting * SR * 2.0) as usize);
    let mut l = [0.0f32; BLOCK];
    let mut r = [0.0f32; BLOCK];
    let mut timeline = GateTimeline::new();
    let blocks = |s: f32| (s * SR / BLOCK as f32) as usize;

    for (i, (drive, why)) in entries.iter().enumerate() {
        engine.tracks_mut()[0].set_macro(WARP_DRV_SLOT, *drive);
        timeline.push((i as f32 * per_setting, format!("WARP.DRV {drive:.2}"), why));

        for hit in 0..hits {
            engine.trigger(0, 1.0);
            // The last hit carries the gap so the settings are separated by
            // silence rather than butting into each other.
            let tail = if hit + 1 == hits {
                step_s + gap_s
            } else {
                step_s
            };
            for _ in 0..blocks(tail) {
                engine.process(&mut l, &mut r);
                out.extend_from_slice(&l);
                out.extend_from_slice(&r);
            }
        }
    }

    (out, timeline)
}

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

/// The shipped kit, twice: with the shaping stage, and with it bypassed.
///
/// Same seed, same pattern, same macros, same two bars, so the only variable
/// is the stage. This used to carry a third pass for `warps::Modulator`; that
/// comparison is in `docs/warps-vendoring.md` and the module is gone.
fn render_shaper_kit_comparison() -> (Vec<f32>, Vec<(f32, String)>) {
    use mi_drum_engine::{DeviceEngine, BLOCK, SAMPLE_RATE as SR};

    let bpm = 130.0f32;
    let step_s = 60.0 / bpm / 4.0;
    let steps = 16usize;
    let bars = 2usize;
    let tail_s = 2.0f32;
    let total_s = steps as f32 * step_s * bars as f32 + tail_s;

    // The same backbeat `render_warps_kit_comparison` uses, so the two
    // auditions are directly comparable to each other as well.
    const PATTERN: [[bool; 16]; 6] = [
        [
            true, false, false, false, true, false, false, false, true, false, false, true, true,
            false, false, false,
        ],
        [
            false, false, false, false, true, false, false, false, false, false, false, false,
            true, false, true, false,
        ],
        [
            true, false, true, false, true, false, true, false, true, false, true, false, true,
            false, true, true,
        ],
        [
            false, false, true, false, false, false, false, true, false, false, true, false, false,
            false, false, false,
        ],
        [
            false, false, false, true, false, false, false, false, false, true, false, false,
            false, false, true, false,
        ],
        [
            true, false, false, false, false, false, true, false, false, false, false, false, true,
            false, false, false,
        ],
    ];

    let total_steps = steps * bars;
    let total_blocks = (total_s * SR / BLOCK as f32) as usize;
    let mut out: Vec<f32> = Vec::new();
    let mut notes: Vec<(f32, String)> = Vec::new();

    // Warps is gone; the comparison that remains is the stage against its own
    // bypass, which is the one that says whether it is doing anything.
    let passes: &[(bool, &str)] = &[
        (true, "A - ADAA shaper, kit defaults"),
        (false, "B - no shaping at all (WARP.DRV bypass), the reference"),
    ];

    for (shaped, label) in passes {
        // Re-seeding per pass rather than once for the file: `stmlib::Random`
        // is a process-global generator, so three passes sharing one seed
        // would have the noise diverge between them and the comparison would
        // be of two different performances.
        mi_drum_engine::seed_random(mi_drum_engine::DEFAULT_RANDOM_SEED);

        // Heap, not stack: `MiDrumEngine` is ~476 KB and returning one by
        // value overflows the main thread's stack. See `mi_drum_in_place`.
        let mut engine = mi_drum_in_place();
        engine.set_master_gain(0.5);
        if !*shaped {
            for t in 0..mi_drum_engine::TRACKS {
                engine.tracks_mut()[t].set_macro(mi_drum_engine::SLOT_STRIP_HOLD, 0.0);
            }
        }

        notes.push((out.len() as f32 / 2.0 / SR, (*label).to_string()));

        let mut next_step = 0usize;
        let mut next_step_at = 0.0f32;
        let mut l = [0.0f32; BLOCK];
        let mut r = [0.0f32; BLOCK];

        for b in 0..total_blocks {
            let block_end = (b + 1) as f32 * BLOCK as f32 / SR;
            while next_step < total_steps && next_step_at < block_end {
                let s = next_step % steps;
                for (track, row) in PATTERN.iter().enumerate() {
                    if row[s] {
                        engine.trigger(track, if s.is_multiple_of(4) { 1.0 } else { 0.7 });
                    }
                }
                next_step += 1;
                next_step_at += step_s;
            }
            engine.process(&mut l, &mut r);
            out.extend_from_slice(&l);
            out.extend_from_slice(&r);
        }
    }

    (out, notes)
}

/// The stage under the conditions that expose a cheap antialiasing scheme.
///
/// Track 4 of `DEFAULT_KIT` is a Plaits `String` — sustained and pitched,
/// which is the opposite of what drums are and exactly what aliasing shows up
/// on. Driven hard and taken fully wet so nothing is hidden behind
/// `WARP.MIX`, then walked up three octaves a semitone at a time.
///
/// What to listen for: alias partials move *down* as the fundamental moves
/// up, because they are reflections about Nyquist. A stage that stays
/// harmonically coherent across the rise is working; one that grows a
/// descending metallic shadow is not.
fn render_shaper_alias_test() -> (Vec<f32>, Vec<(f32, String)>) {
    use mi_drum_engine::{DeviceEngine, BLOCK, SAMPLE_RATE as SR};

    const MELODIC_TRACK: usize = 4;
    const SEMIS: i32 = 36;
    let note_s = 0.22f32;
    let tail_s = 1.5f32;

    let mut out: Vec<f32> = Vec::new();
    let mut notes: Vec<(f32, String)> = Vec::new();

    // One pass per algorithm. ADAA is first order, so it attenuates aliasing
    // rather than removing it, and the four algorithms do not stress it
    // equally: the crossfade is linear and generates nothing, while the fold
    // is the most harmonically violent thing in the set. If any of them is
    // going to fall apart on a pitched sweep it will be audible here.
    for (algo, label) in [
        (0.0f32, "A - crossfade (linear, nothing to alias)"),
        (1.0 / 3.0, "B - sine fold (the violent one)"),
        (2.0 / 3.0, "C - diode ring (Warps' analog model)"),
        (1.0, "D - digital ring (band-limited modulator)"),
    ] {
        mi_drum_engine::seed_random(mi_drum_engine::DEFAULT_RANDOM_SEED);

        // Heap, not stack: `MiDrumEngine` is ~476 KB and returning one by
        // value overflows the main thread's stack. See `mi_drum_in_place`.
        let mut engine = mi_drum_in_place();
        // Low, because this pass is deliberately driven into the region where
        // both stages are at their most extreme and neither may be allowed to
        // hit the output clipper: a clipped pass generates its own harmonics
        // and the comparison stops being about either shaper. The passes are
        // loudness-matched below instead.
        engine.set_master_gain(0.12);
        {
            let track = &mut engine.tracks_mut()[MELODIC_TRACK];
            // Hard drive, fully wet: the worst case for aliasing.
            track.set_macro(mi_drum_engine::SLOT_STRIP_HOLD, 0.9);
            track.set_macro(mi_drum_engine::SLOT_WARPS_MIX, 1.0);
            track.set_macro(mi_drum_engine::SLOT_FILT_0, algo);
        }
        // Everything else silent: one voice, one variable.
        for t in 0..mi_drum_engine::TRACKS {
            if t != MELODIC_TRACK {
                engine.tracks_mut()[t].set_macro(drum_engine::machines::SLOT_LEVEL, 0.0);
            }
        }

        notes.push((out.len() as f32 / 2.0 / SR, label.to_string()));

        let mut pass: Vec<f32> = Vec::new();
        let mut l = [0.0f32; BLOCK];
        let mut r = [0.0f32; BLOCK];
        let blocks_per_note = (note_s * SR / BLOCK as f32) as usize;

        for semi in 0..=SEMIS {
            engine.tracks_mut()[MELODIC_TRACK].retune(semi as f32);
            engine.trigger(MELODIC_TRACK, 1.0);
            for _ in 0..blocks_per_note {
                engine.process(&mut l, &mut r);
                pass.extend_from_slice(&l);
                pass.extend_from_slice(&r);
            }
        }
        for _ in 0..((tail_s * SR / BLOCK as f32) as usize) {
            engine.process(&mut l, &mut r);
            pass.extend_from_slice(&l);
            pass.extend_from_slice(&r);
        }

        // Normalise each pass to a fixed *peak*, not a fixed RMS.
        //
        // RMS matching was right when this file compared two implementations
        // and loudness was the confound. Comparing four algorithms of one
        // stage, it is actively wrong: a plucked string has a ~26 dB crest
        // factor, so scaling it to a common RMS drives the transients into
        // the clamp and every pass comes back reading as clipped. The
        // algorithms are already level-matched to within 8 dB by
        // construction (`the_algorithms_are_level_matched_to_each_other`), so
        // peak-normalising with headroom keeps them comparable and keeps the
        // file honest about what the stage actually does.
        let peak = pass.iter().fold(0.0f32, |m, v| m.max(v.abs()));
        let g = if peak > 1.0e-9 { 0.7 / peak } else { 1.0 };
        out.extend(pass.iter().map(|v| v * g));
    }

    (out, notes)
}
