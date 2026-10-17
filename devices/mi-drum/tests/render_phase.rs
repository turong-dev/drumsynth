//! Render phase is a property of the track, and must survive a machine swap.
//!
//! `MiSlot` renders `VOICE_BLOCK` (24) samples at a time into a `BLOCK` (32)
//! engine block, so without staggering all six voices cross their render
//! boundary on the same tick and one block in three does every voice's work
//! at once. That block is the one that misses the deadline: aligned measured
//! 123.9% of budget against 100.3% staggered.
//!
//! `MiDrumEngine::new` staggers the phases. The bug this guards is that
//! `Slot::load_in_place` — the path *every* machine change takes — used to
//! reconstruct the slot and leave `render_phase` at the zero
//! `new_in_place` writes. So the device booted staggered and collapsed to
//! the aligned worst case the first time a kit was loaded or the machine
//! selector moved, and no bench noticed because none of them changes a
//! machine.

use mi_drum_engine::{DeviceEngine, MiDrumEngine, MiMachineId, DEFAULT_KIT};

fn engine_box() -> Box<MiDrumEngine> {
    use std::alloc::{alloc, Layout};
    unsafe {
        let layout = Layout::new::<MiDrumEngine>();
        let ptr = alloc(layout) as *mut MiDrumEngine;
        assert!(!ptr.is_null(), "failed to allocate MiDrumEngine");
        MiDrumEngine::new_in_place(ptr);
        Box::from_raw(ptr)
    }
}

fn phases(e: &MiDrumEngine) -> Vec<usize> {
    (0..e.tracks().len())
        .map(|t| e.tracks()[t].slot.render_phase())
        .collect()
}

#[test]
fn staggered_at_construction() {
    let e = engine_box();
    let p = phases(&e);
    assert_eq!(p.len(), 6);
    let mut sorted = p.clone();
    sorted.sort_unstable();
    sorted.dedup();
    assert_eq!(
        sorted.len(),
        p.len(),
        "every track needs its own phase, got {p:?}"
    );
}

#[test]
fn a_kit_load_does_not_realign_the_voices() {
    let mut e = engine_box();
    let before = phases(&e);
    e.load_kit(&DEFAULT_KIT);
    assert_eq!(
        phases(&e),
        before,
        "load_kit reset the render phases — all six voices now render on \
         the same tick, which is the aligned worst case"
    );
}

#[test]
fn a_machine_swap_does_not_realign_the_voices() {
    let mut e = engine_box();
    let before = phases(&e);
    // The path the NRPN machine selector and the grid both take.
    e.tracks_mut()[3].load_machine(MiMachineId::String);
    e.tracks_mut()[0].load_machine(MiMachineId::PeaksFmDrum);
    assert_eq!(
        phases(&e),
        before,
        "a machine swap reset that track's render phase"
    );
}
