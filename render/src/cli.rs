use clap::{Parser, Subcommand, ValueEnum};
use drum_engine::machines::MachineId;

#[derive(Parser)]
#[command(name = "render", about = "Audition the drum engine without hardware")]
pub(crate) struct Cli {
    #[command(subcommand)]
    pub(crate) command: Command,
}

#[derive(Subcommand)]
pub(crate) enum Command {
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
        /// Force a strip waveshaper algorithm on every track (`SLOT_FILT_0`).
        ///
        /// `none` is `0.0` — the shipped default, and the only value the
        /// pinned baseline digest covers. The other names are inherited from
        /// the Phase 14 stage spike this selector replaced: they now pick
        /// positions on the shaper's algorithm axis (`lpg` → 0.25,
        /// `overdrive` → 0.5, `resonator` → 0.75), not MI stages. Anything
        /// but `none` changes the sound on purpose and will not match the
        /// baseline.
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
    ///
    /// The stage answering it now is `Shaper`, not Warps. The per-setting
    /// labels on this sweep have been restated for it — see
    /// `render_warps_drive_demo`.
    Warps {
        /// Output path.
        #[arg(short, long, default_value = "warps-drive.wav")]
        output: String,
    },
    /// A/B the ADAA waveshaper against its own bypass.
    ///
    /// Three files. The kit comparison is the musical question — does the cheaper
    /// stage still colour the kit, and by how much. The alias test is the
    /// honest one: first-order ADAA *attenuates* aliasing rather than removing
    /// it, and drums hide aliasing well, so the stage is also put under a
    /// pitched sweep at full drive and full wet, which is where a cheap
    /// antialiasing scheme fails audibly if it is going to. The sine test is
    /// the stage on its own — no strip, no limiter, one partial in the source —
    /// with every algorithm at every drive and then both knobs swept
    /// continuously. See `shaper_probe::render_shaper_sine`.
    Shaper {
        /// Output path.
        #[arg(short, long, default_value = "shaper-ab.wav")]
        output: String,
    },
    /// Render a retrigger/choke stress test to a WAV file.
    ///
    /// Dense rolls on the kick, snare, and closed hat — triggered between
    /// blocks, so they land on the block grid (see `render_pattern`) — plus
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
        /// Send the reboot-to-HalfKay command and exit, so the next flash
        /// needs no button press. `mi-drum` has no `autoboot`.
        #[arg(long)]
        reboot: bool,
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
pub(crate) enum CheatsheetFormat {
    /// Markdown tables.
    Markdown,
    /// Self-contained styled HTML page.
    Html,
}

#[derive(Clone, Copy, ValueEnum)]
pub(crate) enum MachineArg {
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
    /// The underlying engine id.
    ///
    /// Derived from clap's `ValueEnum` name, which defaults to kebab-case and
    /// is verified in `cli::tests::machine_arg_matches_machine_id` to match
    /// `MachineId::name()` for every catalogue entry.
    pub(crate) fn id(self) -> MachineId {
        let possible = self.to_possible_value().unwrap();
        let name = possible.get_name();
        MachineId::ALL
            .iter()
            .find(|&&m| m.name() == name)
            .copied()
            .expect("MachineArg name must match a MachineId name")
    }

    pub(crate) fn from_name(s: &str) -> Option<Self> {
        // Only accept names that are valid MachineId names.
        if MachineId::ALL.iter().any(|&m| m.name() == s) {
            Self::from_str(s, true).ok()
        } else {
            None
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn machine_arg_matches_machine_id() {
        for id in MachineId::ALL {
            let arg = MachineArg::from_name(id.name()).unwrap();
            assert_eq!(arg.id(), id);
        }
    }
}
