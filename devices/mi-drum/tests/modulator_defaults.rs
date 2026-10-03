//! Every machine must have a real second signal for Warps by default.
//!
//! Its own test binary for the same reason as `slot_reuse`: it compares two
//! renders exactly, and `stmlib::Random` is one process-global generator
//! shared by every Plaits engine. Run alongside the other rendering tests,
//! their draws interleave with these and the comparison fails for a reason
//! that has nothing to do with what is being tested. It passed in one full
//! run and failed in the next before being moved here.

use mi_drum_engine::{
    DeviceEngine, MiDrumEngine, MiMachineId, BLOCK, SAMPLE_RATE, SLOT_FILT_0, SLOT_WARPS_MIX,
    SLOT_WARPS_MOD_SRC,
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

/// Render track 0 wet, through a ring modulator so the modulator input is
/// plainly in the output rather than subtly colouring it.
///
/// 300 ms, because the three `SixOp` engines are FM voices whose operator
/// envelopes take tens of milliseconds to open: a shorter window catches them
/// before they sound, which reads as "no second signal" when the real answer
/// is "no signal yet".
fn render(id: MiMachineId, mod_src: Option<f32>) -> Vec<f32> {
    let mut e = engine_box();
    e.tracks_mut()[0].load_machine(id);
    e.tracks_mut()[0].set_macro(SLOT_WARPS_MIX, 1.0);
    e.tracks_mut()[0].set_macro(SLOT_FILT_0, 0.25); // analog ring mod
    if let Some(v) = mod_src {
        e.tracks_mut()[0].set_macro(SLOT_WARPS_MOD_SRC, v);
    }
    e.trigger_channel(0, 60, 1.0);
    let mut l = [0.0f32; BLOCK];
    let mut r = [0.0f32; BLOCK];
    let mut out = Vec::new();
    for _ in 0..((0.3 * SAMPLE_RATE / BLOCK as f32) as usize) {
        e.process(&mut l, &mut r);
        out.extend_from_slice(&l);
    }
    out
}

/// Warps cross-modulates input 1 against input 2, so handing it the same
/// signal twice is the degenerate case: a comparator with nothing to compare,
/// and `ALGORITHM_XFADE` reduced to a gain. `WARP.IN` exists so no track has
/// to sit there — but only if its *default* points at something real.
///
/// That is not automatic, because the honest default differs by machine. A
/// Plaits voice usually has a distinct aux output; the three `SixOp` engines
/// write the same samples to both outputs; a Peaks voice has no aux at all,
/// and the aux setting falls back to self. A single shared default therefore
/// leaves seven of the 28 machines — including four of the six tracks in
/// `DEFAULT_KIT` — exactly where they started.
///
/// Asserted by forcing `WARP.IN` to self and requiring each machine's default
/// to sound different from that.
#[test]
fn every_machine_has_a_real_modulator_by_default() {
    for &id in MiMachineId::ALL.iter() {
        let defaulted = render(id, None);
        let selfmod = render(id, Some(0.0));
        let delta = defaulted
            .iter()
            .zip(selfmod.iter())
            .map(|(a, b)| (a - b).abs())
            .fold(0.0f32, f32::max);
        assert!(
            delta > 1.0e-4,
            "{id:?} defaults to cross-modulating against itself (max delta \
             {delta}) — its WARP.IN default has no second signal to reach for"
        );
    }
}
