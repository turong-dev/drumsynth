//! Host-side renderer.
//!
//! This is where you decide what things should sound like. It links the same
//! `drum-engine` crate the firmware does, so anything you tune here is
//! already validated by the time it reaches hardware.
//!
//! Three modes:
//!
//! ```text
//! render  — write a demo pattern to a WAV file
//! sweep   — write one WAV per value of a parameter, for A/B-ing
//! play    — real-time playback (requires --features live)
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
use drum_engine::machines::MachineId;
use drum_engine::{DrumEngine, BLOCK, SAMPLE_RATE, TRACKS};

/// The MIDI-in / audio-out device mode. Behind the `live` feature with the
/// rest of the host audio stack.
#[cfg(feature = "live")]
mod device;

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
    /// Render one file per value of a swept macro knob.
    ///
    /// `<machine>` is one of `bd-classic`, `sd-natural`, `hat-classic`.
    /// `<macro>` is the macro's name uppercased (`TUNE`, `SWEEP`, `DEC`, ...).
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
    #[cfg(feature = "live")]
    Device {
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
    },
}

#[derive(Clone, Copy, ValueEnum)]
enum MachineArg {
    BdClassic,
    BdFm,
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
}

impl MachineArg {
    fn id(self) -> MachineId {
        match self {
            Self::BdClassic => MachineId::BdClassic,
            Self::BdFm => MachineId::BdFm,
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
        }
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

        Command::Sweep {
            machine,
            macro_name,
            from,
            to,
            steps,
            output_dir,
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
                println!("{path}");
            }
            let _ = info;
            println!(
                "\n{steps} files in {output_dir}/ — flip between them to compare \
                 (swept {} {} from {from:.4} to {to:.4})",
                id.name(),
                macro_name,
            );
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

        #[cfg(feature = "live")]
        Command::Play { bpm } => play_live(bpm)?,

        #[cfg(feature = "live")]
        Command::Device { out, port, list } => device::run(&out, &port, list)?,
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

/// Configure per-track strip settings (pan, level, filter, drive) and a few
/// macro tweaks so the kit sounds balanced across the stereo field rather
/// than all-centre, all-equal. Preserves the choke/layer masks already set
/// by `DrumEngine::new` (open-hat ↔ closed-hat relation).
fn setup_kit_mix(engine: &mut DrumEngine) {
    use drum_engine::dsp::SvfMode;
    use drum_engine::StripParams;

    // (track, pan, level, filter_mode, cutoff_hz, reso_q, drive)
    let configs: [(usize, f32, f32, SvfMode, f32, f32, f32); TRACKS] = [
        (0, 0.00, 0.95, SvfMode::Off, 1000.0, 0.707, 1.0), // kick — centre
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
    engine.tracks[0].set_macro(3, 0.75); // DEC
    engine.tracks[0].set_macro(4, 0.5); // DRIVE

    // Clap: slightly more body, less pure noise (BAL default 0.80 noise-heavy)
    engine.tracks[4].set_macro(6, 0.65); // BAL

    // Tom: tune lower (~132 Hz, a mid tom) and add a bit more stick
    engine.tracks[5].set_macro(0, 0.50); // TUNE → ~132 Hz
    engine.tracks[5].set_macro(4, 0.40); // STICK

    // Cowbell: classic 808 tuning — ~540 Hz base, wider detune, short decay
    engine.tracks[6].set_macro(0, 0.34); // TUNE → ~538 Hz
    engine.tracks[6].set_macro(2, 0.86); // DET → ~1.43 ratio (the classic 540/800 pair)
    engine.tracks[6].set_macro(1, 0.15); // DEC → ~85ms

    // SY Tone: pitch it as a bassline — low note, moderate FM
    engine.tracks[7].set_macro(0, 0.20); // TUNE → ~132 Hz (bass range)
    engine.tracks[7].set_macro(4, 0.50); // MOD.AMT
    engine.tracks[7].set_macro(2, 0.30); // FDBK
    engine.tracks[7].set_macro(5, 0.40); // DEC → ~830ms (rings a bit)

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
