//! Mutable Instruments C++ DSP wrappers for synth devices.
//!
//! This crate bridges the no-alloc Rust device core to the open-source
//! Mutable Instruments (MI) C++ code. The FFI boundary is block-rate only:
//! Rust prepares parameter structs, calls an extern `render_block` once per
//! audio block, and reads back interleaved 16-bit frames.
//!
//! Object ownership is held by Rust: each wrapper is an aligned byte array
//! into which the C++ object is placement-new'd at construction time. No
//! heap allocation occurs on either side of the boundary.
//!
//! # Safety
//!
//! This is the only crate in the workspace that uses `unsafe`. It is confined
//! to the small amount needed for the C FFI calls and the placement-new
//! storage.

#![no_std]
#![warn(missing_docs)]

#[cfg(test)]
extern crate std;

pub mod clouds;
pub mod peaks;
pub mod plaits;
pub mod spike_stages;
pub mod gate;
pub mod sys;
pub mod warps;

/// Seed the noise generator shared by every Plaits engine.
///
/// Mutable's `stmlib::Random` is a single process-global LCG — one `rng_state_`
/// static for the whole library, not one per voice. On the target that is
/// harmless: there is one engine, rendering one track at a time, in a fixed
/// order. Off the target it means a render is only reproducible if nothing
/// else has drawn from the generator first, and two renders running
/// concurrently interleave their draws and both diverge.
///
/// Call this before a render that has to be bit-reproducible. It does not make
/// concurrent renders safe — nothing short of a per-voice generator would —
/// but it does make a sequential one repeatable.
pub fn seed_random(seed: u32) {
    // SAFETY: the C function assigns one `uint32_t` static and returns.
    #[allow(unsafe_code)]
    unsafe {
        sys::mi_dsp_seed_random(seed);
    }
}

/// The seed `stmlib::Random` starts at in the vendored source, and therefore
/// the one a render must use to reproduce a from-process-start result.
pub const DEFAULT_RANDOM_SEED: u32 = 0x21;
