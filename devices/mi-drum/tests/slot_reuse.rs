//! A voice must not depend on what was in its memory beforehand.
//!
//! Its own test binary on purpose. `stmlib::Random` is one process-global LCG
//! shared by every Plaits engine, so a test that compares two renders
//! *exactly* cannot run alongside other rendering tests — their draws
//! interleave with its own and the comparison fails for a reason that has
//! nothing to do with what is being tested. Here it is the only test in the
//! process.

use mi_drum_engine::{
    seed_random, DeviceEngine, MiDrumEngine, MiMachineId, Slot, DEFAULT_RANDOM_SEED,
};

/// Heap-allocate the engine; the struct is far too large for a test stack.
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

const SAMPLES: usize = 4000;

/// Render one machine's raw voice output, optionally after running a
/// different machine over the same slot first.
///
/// Measured at the slot rather than at the engine output, deliberately: the
/// strip and the send buses legitimately carry state across a machine change,
/// and this is a question about the voice.
fn voice_out(previous: Option<MiMachineId>, id: MiMachineId) -> Vec<f32> {
    let mut e = engine_box();
    if let Some(prev) = previous {
        // Dirty the slot: run a different model over the same storage.
        e.tracks_mut()[0].load_machine(prev);
        e.trigger_channel(0, 60, 1.0);
        let slot = &mut e.tracks_mut()[0].slot;
        for _ in 0..SAMPLES {
            let _ = slot.tick();
        }
    }
    e.tracks_mut()[0].load_machine(id);
    // Re-seed immediately before the measured render: the dirtying pass above
    // draws from the shared generator, and without this every noise-using
    // voice would differ for a reason that is not the one under test.
    seed_random(DEFAULT_RANDOM_SEED);
    e.trigger_channel(0, 60, 1.0);
    let slot = &mut e.tracks_mut()[0].slot;
    (0..SAMPLES).map(|_| slot.tick()).collect()
}

/// A machine must sound the same whatever the slot held before it.
///
/// This is the invariant that catches an uninitialised read in a vendored
/// voice, and nearly the only kind of test that can. A fresh engine comes from
/// `mmap`, which hands out zeroed pages, so a field whose `Init` forgets it
/// reads as 0.0 and looks correct. Load a different model onto the same slot
/// first and that field reads whatever the previous one left — which, when
/// those bytes held a pointer, moves with ASLR and makes the render differ
/// *between processes*. That is invisible to any single run of the suite, and
/// it is what made the committed baseline digest meaningless for months: the
/// same binary produced a different digest every time it ran.
///
/// Verified to fail with the `peaks::HighHat::Init` fix reverted — that model
/// never initialised its six oscillator phases.
///
/// It does **not** catch every instance, which is worth knowing.
/// `plaits::SyntheticBassDrum::Init` also left out `transient_env_lp_`, a
/// one-pole accumulator read before first write; by the end of a previous hit
/// that field has decayed to approximately zero, so in-process the stale value
/// and the correct one are indistinguishable. Only genuinely foreign memory
/// shows it, which means across processes — running the baseline digest in a
/// loop, as `DESIGN.md` describes. Poisoning the allocation does not substitute:
/// a slot is built on the stack and moved into place, so the fill never
/// reaches the vendored storage.
#[test]
fn a_machine_sounds_the_same_whatever_the_slot_held_before() {
    for &id in MiMachineId::ALL.iter() {
        let clean = voice_out(None, id);
        // Three predecessors. A model only reads garbage that something
        // actually wrote at those addresses, so `id` before `id` covers its
        // own fields, and the other two cover state laid out differently by
        // the two voice types.
        for prev in [MiMachineId::VirtualAnalog, MiMachineId::PeaksFmDrum, id] {
            let dirty = voice_out(Some(prev), id);
            let delta = clean
                .iter()
                .zip(dirty.iter())
                .map(|(a, b)| (a - b).abs())
                .fold(0.0f32, f32::max);
            assert_eq!(
                delta, 0.0,
                "{id:?} depends on what the slot held before it ({prev:?}): \
                 max delta {delta} — something in its Init is reading memory \
                 it never wrote"
            );
        }
    }
}
