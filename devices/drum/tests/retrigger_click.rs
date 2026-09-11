//! Regression: a fast retrigger of a sounding voice must not add a
//! discontinuity beyond the machine's own natural attack, and a choke cut
//! must not step. `max_step` compares a single hit against a retrigger roll;
//! `retrigger_click_fixed` asserts the roll's worst inter-sample step stays
//! within ~1.6x the machine's single-hit character (bright machines like
//! SyTone legitimately swing ~0.5/sample — that is their tone, not a click).

use drum_engine::{DrumEngine, MachineId, BLOCK};

fn max_step(machine: MachineId, retrigger_every: Option<usize>) -> f32 {
    let mut engine = DrumEngine::new();
    engine.tracks[0].load_machine(machine);
    let mut l = [0.0f32; BLOCK];
    let mut r = [0.0f32; BLOCK];
    let total = 48000usize;
    let mut prev: Option<f32> = None;
    let mut worst: f32 = 0.0;
    let mut n = 0usize;

    engine.panic();

    if retrigger_every.is_none() {
        engine.trigger(0, 1.0);
    }
    for block in 0..total / BLOCK {
        if let Some(every) = retrigger_every {
            let block_start = block * BLOCK;
            while n <= block_start + BLOCK {
                engine.trigger(0, 1.0);
                n += every;
            }
        }
        engine.process(&mut l, &mut r);
        for i in 0..BLOCK {
            let s = l[i];
            if let Some(p) = prev {
                let d = (s - p).abs();
                if d > worst {
                    worst = d;
                }
            }
            prev = Some(s);
        }
    }
    worst
}

/// Worst inter-sample step of the default-kit OH (track 3) in `[start, end)`,
/// optionally choked at `choke_at`. Compares the natural decay against the
/// fade-out to prove the choke adds no discontinuity.
fn oh_worst_step(start: usize, len: usize, choke_at: Option<usize>) -> f32 {
    let mut engine = DrumEngine::new();
    let mut l = [0.0f32; BLOCK];
    let mut r = [0.0f32; BLOCK];
    let mut prev: Option<f32> = None;
    let mut worst = 0.0f32;
    let mut choked = false;

    engine.trigger(3, 1.0);
    for block in 0..((start + len) / BLOCK + 2) {
        let block_start = block * BLOCK;
        if let Some(at) = choke_at {
            if block_start >= at && !choked {
                engine.tracks[3].choke();
                choked = true;
            }
        }
        engine.process(&mut l, &mut r);
        for i in 0..BLOCK {
            let sample = block_start + i;
            if sample >= start && sample < start + len {
                let s = l[i];
                if let Some(p) = prev {
                    let d = (s - p).abs();
                    if d > worst {
                        worst = d;
                    }
                }
                prev = Some(s);
            }
        }
    }
    worst
}

#[test]
fn retrigger_click_fixed() {
    for m in MachineId::ALL {
        let single = max_step(m, None);
        let rapid = max_step(m, Some(200));
        let bound = (single * 1.6).max(0.005);
        println!("{m:?}: single={single:.6}  retrigger={rapid:.6}  bound={bound:.6}");
        assert!(
            rapid <= bound,
            "{m:?} retrigger step {rapid} exceeds 1.6x single-hit ({single}) — de-click gap"
        );
    }
}

#[test]
fn choke_click_fixed() {
    // Choke at 500 (OH still ringing hard); compare the same window without.
    let natural = oh_worst_step(500, 500, None);
    let choked = oh_worst_step(500, 500, Some(500));
    println!("natural=[{natural:.6}] choked=[{choked:.6}]");
    assert!(
        choked <= natural * 1.5 + 1e-6,
        "choke added a discontinuity: natural={natural} choked={choked}"
    );
}
