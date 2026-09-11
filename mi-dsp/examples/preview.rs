// Preview all Plaits engines as a stereo WAV.
//
// Run: cargo run -p mi-dsp --example preview
// Output: plaits_preview.wav (48 kHz, 24-bit stereo, out=L aux=R)

use mi_dsp::plaits::{MiPlaitsModulations, MiPlaitsPatch, PlaitsVoice};

const SAMPLE_RATE: f32 = 48000.0;
const BLOCK_SIZE: usize = 24; // Plaits kMaxBlockSize
const OUTPUT: &str = "plaits_preview.wav";

const ENGINE_NAMES: &[&str] = &[
    "virtual_analog_vcf",
    "phase_distortion",
    "six_op_1",
    "six_op_2",
    "six_op_3",
    "wave_terrain",
    "string_machine",
    "chiptune",
    "virtual_analog",
    "waveshaping",
    "fm",
    "grain",
    "additive",
    "wavetable",
    "chord",
    "speech",
    "swarm",
    "noise",
    "particle",
    "string",
    "modal",
    "bass_drum",
    "snare_drum",
    "hi_hat",
];

fn main() {
    let mut voice = PlaitsVoice::new();

    let patch = MiPlaitsPatch {
        note: 60.0,
        harmonics: 0.5,
        timbre: 0.5,
        morph: 0.5,
        frequency_modulation_amount: 0.0,
        timbre_modulation_amount: 0.0,
        morph_modulation_amount: 0.0,
        engine: 0,
        decay: 0.5,
        lpg_colour: 0.5,
    };

    let mut out_block = [0.0f32; BLOCK_SIZE];
    let mut aux_block = [0.0f32; BLOCK_SIZE];

    let mut samples: Vec<f32> = Vec::new();

    let seconds_per_engine = 2.0f32;
    let hits_per_engine = 3usize;
    let hit_spacing = 0.35f32; // seconds between hits
    let gap_between_engines = 0.15f32; // seconds of silence

    // Trigger must drop below 0.1 between hits, so we keep it high for one
    // block then low until the next hit.
    let hit_period_blocks = (hit_spacing * SAMPLE_RATE / BLOCK_SIZE as f32).ceil() as usize;
    let gap_blocks = (gap_between_engines * SAMPLE_RATE / BLOCK_SIZE as f32).ceil() as usize;
    let total_blocks = ((seconds_per_engine * SAMPLE_RATE) / BLOCK_SIZE as f32).ceil() as usize;

    let mut modulations = MiPlaitsModulations {
        engine: 0.0,
        note: 0.0,
        frequency: 0.0,
        harmonics: 0.0,
        timbre: 0.0,
        morph: 0.0,
        trigger: 0.0,
        level: 0.0,
        frequency_patched: 0,
        timbre_patched: 0,
        morph_patched: 0,
        trigger_patched: 1,
        level_patched: 0,
    };

    for (idx, name) in ENGINE_NAMES.iter().enumerate() {
        println!("rendering {idx:2}: {name}");

        let mut patch = patch;
        patch.engine = idx as i32;

        let mut peak_engine = 0.0f32;

        for block in 0..total_blocks {
            // Fire a rising edge on the first block of each hit period.
            let is_hit =
                (block % hit_period_blocks) == 0 && (block / hit_period_blocks) < hits_per_engine;
            modulations.trigger = if is_hit { 1.0 } else { 0.0 };

            voice.render_f32(
                &patch,
                &modulations,
                &mut out_block,
                &mut aux_block,
                BLOCK_SIZE,
            );

            for i in 0..BLOCK_SIZE {
                peak_engine = peak_engine.max(out_block[i].abs());
                samples.push(out_block[i]);
                samples.push(aux_block[i]);
            }
        }

        println!("  peak={peak_engine:.6}");

        // Brief silence between engines so the WAV is easy to scan.
        modulations.trigger = 0.0;
        for _ in 0..gap_blocks {
            voice.render_f32(
                &patch,
                &modulations,
                &mut out_block,
                &mut aux_block,
                BLOCK_SIZE,
            );
            for i in 0..BLOCK_SIZE {
                samples.push(out_block[i]);
                samples.push(aux_block[i]);
            }
        }
    }

    write_wav(OUTPUT, &samples).expect("failed to write WAV");

    let peak_l = samples
        .iter()
        .step_by(2)
        .map(|s| s.abs())
        .fold(0.0f32, f32::max);
    let peak_r = samples
        .iter()
        .skip(1)
        .step_by(2)
        .map(|s| s.abs())
        .fold(0.0f32, f32::max);
    println!(
        "wrote {OUTPUT} ({:.2}s)  peak L={:.3} R={:.3}",
        samples.len() as f32 / 2.0 / SAMPLE_RATE,
        peak_l,
        peak_r
    );
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
        eprintln!("warning: {clipped} samples clipped");
    }
    Ok(())
}
