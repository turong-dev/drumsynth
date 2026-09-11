// 8x worst-case Plaits voice benchmark.
//
// Renders one block (kMaxBlockSize = 24 samples) from eight voices
// simultaneously, measuring host wall-clock time and estimating the
// per-sample cycle budget on a 600 MHz Cortex-M7.

use mi_dsp::plaits::{MiPlaitsModulations, MiPlaitsPatch, PlaitsVoice};
use std::time::Instant;

const BLOCK_SIZE: usize = 24;
const VOICES: usize = 8;
const ITERATIONS: usize = 1000;

fn main() {
    let mut voices: Vec<PlaitsVoice> = (0..VOICES).map(|_| PlaitsVoice::new()).collect();

    let patch = MiPlaitsPatch {
        note: 60.0,
        harmonics: 0.5,
        timbre: 0.5,
        morph: 0.5,
        frequency_modulation_amount: 0.0,
        timbre_modulation_amount: 0.0,
        morph_modulation_amount: 0.0,
        engine: 21, // bass drum
        decay: 0.5,
        lpg_colour: 0.5,
    };
    let mut modulations = MiPlaitsModulations {
        engine: 0.0,
        note: 0.0,
        frequency: 0.0,
        harmonics: 0.0,
        timbre: 0.0,
        morph: 0.0,
        trigger: 1.0,
        level: 0.8,
        frequency_patched: 0,
        timbre_patched: 0,
        morph_patched: 0,
        trigger_patched: 1,
        level_patched: 0,
    };

    let mut out = [0.0f32; BLOCK_SIZE];
    let mut aux = [0.0f32; BLOCK_SIZE];

    // Warm up.
    for voice in voices.iter_mut() {
        voice.render_f32(&patch, &modulations, &mut out, &mut aux, BLOCK_SIZE);
    }

    let start = Instant::now();
    let mut peak = 0.0f32;
    for i in 0..ITERATIONS {
        // Re-trigger every other block so the drum is audible throughout the
        // benchmark; this gives a rising edge while keeping the measurement
        // dominated by normal Render work.
        modulations.trigger = if i % 2 == 0 { 1.0 } else { 0.0 };
        for voice in voices.iter_mut() {
            voice.render_f32(&patch, &modulations, &mut out, &mut aux, BLOCK_SIZE);
            peak = peak.max(out.iter().map(|s| s.abs()).fold(0.0f32, f32::max));
        }
    }
    let elapsed = start.elapsed();

    let total_samples = (ITERATIONS * VOICES * BLOCK_SIZE) as f64;
    let seconds = elapsed.as_secs_f64();
    let samples_per_second = total_samples / seconds;
    let us_per_block = seconds * 1e6 / (ITERATIONS * VOICES) as f64;

    // Cortex-M7 @ 600 MHz budget estimate.
    let cm7_cycles_per_sample = 600e6 / samples_per_second;

    println!("mi-dsp 8x Plaits voice benchmark");
    println!("  block size:        {BLOCK_SIZE}");
    println!("  voices:            {VOICES}");
    println!("  iterations:        {ITERATIONS}");
    println!("  total samples:     {total_samples}");
    println!("  elapsed:           {elapsed:?}");
    println!("  samples/sec:       {samples_per_second:.0}");
    println!("  us/block/voice:    {us_per_block:.2}");
    println!("  CM7 cycles/sample: {cm7_cycles_per_sample:.1}");

    // Sanity check: ensure at least one rendered block is not silent.
    assert!(peak > 0.001, "benchmark produced silent output");
}
