// Host benchmark for the mi-drum engine.
//
// Run: cargo bench -p mi-drum-engine

use mi_drum_engine::MiDrumEngine;
use std::alloc::{alloc, Layout};
use std::time::Instant;

const BLOCKS: usize = 1000;
const TRIGGER_PERIOD: usize = 12;

fn engine_box() -> Box<MiDrumEngine> {
    unsafe {
        let layout = Layout::new::<MiDrumEngine>();
        let ptr = alloc(layout) as *mut MiDrumEngine;
        assert!(!ptr.is_null(), "failed to allocate MiDrumEngine");
        MiDrumEngine::new_in_place(ptr);
        Box::from_raw(ptr)
    }
}

fn main() {
    let mut engine = engine_box();

    let mut l = [0.0f32; device_core::BLOCK];
    let mut r = [0.0f32; device_core::BLOCK];

    // Warm up.
    for _ in 0..16 {
        engine.process(&mut l, &mut r);
    }

    let start = Instant::now();
    for block in 0..BLOCKS {
        if block % TRIGGER_PERIOD == 0 {
            for i in 0..mi_drum_engine::TRACKS {
                engine.trigger(i, 1.0);
            }
        }
        engine.process(&mut l, &mut r);
    }
    let elapsed = start.elapsed();

    let total_samples = (BLOCKS * device_core::BLOCK) as f64;
    let seconds = elapsed.as_secs_f64();
    let samples_per_second = total_samples / seconds;
    let us_per_block = seconds * 1e6 / BLOCKS as f64;

    println!("mi-drum-engine benchmark");
    println!("  tracks:            {}", mi_drum_engine::TRACKS);
    println!("  blocks:            {BLOCKS}");
    println!("  total samples:     {total_samples}");
    println!("  elapsed:           {elapsed:?}");
    println!("  samples/sec:       {samples_per_second:.0}");
    println!("  us/block:          {us_per_block:.2}");

    // Sanity check: the last block should not be completely silent.
    let peak = l
        .iter()
        .chain(r.iter())
        .map(|s| s.abs())
        .fold(0.0f32, f32::max);
    assert!(peak > 0.0001, "benchmark produced silent output");
}
