//! Host-side renderer.
//!
//! This is where you decide what things should sound like. It links the same
//! `drum-engine` crate the firmware does, so anything you tune here is
//! already validated by the time it reaches hardware.
//!
//! Three modes:
//!
//! ```text
//! render  — write a pattern to a WAV file
//! sweep   — write one WAV per value of a parameter, for A/B-ing
//! play    — real-time playback (requires --features live)
//! ```
//!
//! The sweep mode is the one that earns its keep. Rendering sixteen kicks
//! with decay times from 100ms to 800ms takes a fraction of a second, and
//! flipping between them in an editor is a much faster way to find the right
//! one than turning a knob in real time.

use clap::{Parser, Subcommand, ValueEnum};
use drum_engine::{DrumEngine, Params, VoiceId, BLOCK, SAMPLE_RATE};

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
    /// Render one file per value of a swept parameter.
    Sweep {
        /// Which parameter to sweep.
        #[arg(value_enum)]
        param: SweepParam,
        /// Lowest value.
        #[arg(long)]
        from: f32,
        /// Highest value.
        #[arg(long)]
        to: f32,
        /// How many steps.
        #[arg(long, default_value_t = 8)]
        steps: usize,
        /// Directory for the output files.
        #[arg(short, long, default_value = "sweep")]
        output_dir: String,
    },
    /// Play the demo pattern in real time.
    #[cfg(feature = "live")]
    Play {
        /// Tempo in BPM.
        #[arg(short, long, default_value_t = 130.0)]
        bpm: f32,
    },
}

#[derive(Clone, Copy, ValueEnum)]
enum SweepParam {
    KickDecay,
    KickPitchDecay,
    KickStartHz,
    KickDrive,
    SnareNoiseMix,
    SnareDecay,
    HatDecay,
}

impl SweepParam {
    fn apply(self, p: &mut Params, v: f32) {
        match self {
            Self::KickDecay => p.kick.decay_s = v,
            Self::KickPitchDecay => p.kick.pitch_decay_s = v,
            Self::KickStartHz => p.kick.start_hz = v,
            Self::KickDrive => p.kick.drive = v,
            Self::SnareNoiseMix => p.snare.noise_mix = v,
            Self::SnareDecay => p.snare.decay_s = v,
            Self::HatDecay => p.hat.decay_s = v,
        }
    }

    /// Which voice to trigger when auditioning this parameter in isolation.
    fn voice(self) -> VoiceId {
        match self {
            Self::KickDecay | Self::KickPitchDecay | Self::KickStartHz | Self::KickDrive => {
                VoiceId::Kick
            }
            Self::SnareNoiseMix | Self::SnareDecay => VoiceId::Snare,
            Self::HatDecay => VoiceId::Hat,
        }
    }

    fn name(self) -> &'static str {
        match self {
            Self::KickDecay => "kick_decay",
            Self::KickPitchDecay => "kick_pitch_decay",
            Self::KickStartHz => "kick_start_hz",
            Self::KickDrive => "kick_drive",
            Self::SnareNoiseMix => "snare_noise_mix",
            Self::SnareDecay => "snare_decay",
            Self::HatDecay => "hat_decay",
        }
    }
}

/// A 16-step pattern per voice. `true` means trigger.
struct Pattern {
    kick: [bool; 16],
    snare: [bool; 16],
    hat: [bool; 16],
}

impl Default for Pattern {
    fn default() -> Self {
        // Nothing clever, just something with enough going on to hear the
        // voices interact and check the bus does not clip when they collide.
        Self {
            kick: [
                true, false, false, false, false, false, true, false, false, false, true, false,
                false, false, false, false,
            ],
            snare: [
                false, false, false, false, true, false, false, false, false, false, false, false,
                true, false, false, true,
            ],
            hat: [
                true, false, true, false, true, false, true, false, true, false, true, false, true,
                false, true, true,
            ],
        }
    }
}

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let cli = Cli::parse();

    match cli.command {
        Command::Render { output, bpm, bars } => {
            let samples = render_pattern(&Params::default(), &Pattern::default(), bpm, bars);
            write_wav(&output, &samples)?;
            let seconds = samples.len() as f32 / 2.0 / SAMPLE_RATE;
            println!("wrote {output} ({seconds:.2}s)");
        }

        Command::Sweep {
            param,
            from,
            to,
            steps,
            output_dir,
        } => {
            std::fs::create_dir_all(&output_dir)?;
            let steps = steps.max(2);

            for i in 0..steps {
                let t = i as f32 / (steps - 1) as f32;
                let value = from + (to - from) * t;

                let mut params = Params::default();
                param.apply(&mut params, value);

                let samples = render_one_shot(&params, param.voice(), 2.0);
                let path = format!("{output_dir}/{}_{:02}_{value:.4}.wav", param.name(), i);
                write_wav(&path, &samples)?;
                println!("{path}");
            }
            println!("\n{steps} files in {output_dir}/ — flip between them to compare");
        }

        #[cfg(feature = "live")]
        Command::Play { bpm } => play_live(bpm)?,
    }

    Ok(())
}

/// Render a single hit with a tail, for auditioning one voice.
fn render_one_shot(params: &Params, voice: VoiceId, seconds: f32) -> Vec<f32> {
    let mut engine = DrumEngine::new();
    engine.set_params(params);
    engine.trigger(voice, 1.0);

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

/// Render a pattern, block by block, exactly as the firmware will.
///
/// Note the structure: the sequencer decides what to trigger *between*
/// blocks, never inside `process`. That is the same discipline the firmware
/// needs, so keeping it here means the host and target behave identically —
/// including the up-to-667µs of timing quantisation that block processing
/// imposes on trigger timing.
fn render_pattern(params: &Params, pattern: &Pattern, bpm: f32, bars: usize) -> Vec<f32> {
    let mut engine = DrumEngine::new();
    engine.set_params(params);

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
            if pattern.kick[s] {
                engine.trigger(VoiceId::Kick, 1.0);
            }
            if pattern.snare[s] {
                engine.trigger(VoiceId::Snare, 0.9);
            }
            if pattern.hat[s] {
                // Accent the downbeats a little so it does not sound robotic.
                let vel = if s % 4 == 0 { 0.9 } else { 0.55 };
                engine.trigger(VoiceId::Hat, vel);
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
    let pattern = Pattern::default();
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
                        if pattern.kick[s] {
                            engine.trigger(VoiceId::Kick, 1.0);
                        }
                        if pattern.snare[s] {
                            engine.trigger(VoiceId::Snare, 0.9);
                        }
                        if pattern.hat[s] {
                            let vel = if s % 4 == 0 { 0.9 } else { 0.55 };
                            engine.trigger(VoiceId::Hat, vel);
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
