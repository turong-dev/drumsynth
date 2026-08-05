//! A drum synthesis engine with no knowledge of hardware.
//!
//! # The contract
//!
//! This crate compiles for both your laptop and a Cortex-M7. It never
//! allocates, never blocks, never touches a peripheral, and never calls into
//! an OS. Everything it needs is owned by [`DrumEngine`] and sized at compile
//! time.
//!
//! That is enforced structurally rather than by discipline:
//!
//! - `#![no_std]` with no `alloc` — `Vec`, `Box` and `String` do not exist
//!   here, so you cannot accidentally allocate in the audio path.
//! - `f32` everywhere. On a host `f64` is free and slips in unnoticed; on
//!   target it is not.
//! - Block size is a const generic, so buffers are stack arrays with no
//!   runtime length checks in the inner loop.
//!
//! # Where it runs
//!
//! ```text
//!   render/     (host)      firmware/   (Teensy 4.1)
//!       │                        │
//!       └────────┬───────────────┘
//!                ▼
//!          drum-engine
//! ```
//!
//! The host renderer is for deciding what things should sound like. The
//! firmware is for finding out what they cost. Both link this crate
//! unmodified.

#![no_std]
#![deny(unsafe_code)]
#![warn(missing_docs)]

pub mod dsp;
pub mod midi;
pub mod voices;

use voices::{Hat, HatParams, Kick, KickParams, Snare, SnareParams};

/// Sample rate the engine is built for.
///
/// Fixed rather than runtime-configurable on purpose: every filter and
/// envelope coefficient is derived from it, and making it dynamic means
/// either recomputing coefficients on the fly or carrying a divide into the
/// hot path. Change it here and rebuild.
pub const SAMPLE_RATE: f32 = 48_000.0;

/// Reciprocal of the sample rate, precomputed.
///
/// Multiply by this instead of dividing by [`SAMPLE_RATE`]. A single-precision
/// divide is multi-cycle on an M7 and there is no reason to pay for one per
/// sample.
pub const INV_SAMPLE_RATE: f32 = 1.0 / SAMPLE_RATE;

/// Frames per processing block.
///
/// 32 frames at 48kHz is 667µs of headroom per callback. Smaller means lower
/// latency and more per-callback overhead; larger means the opposite. This is
/// the number your cycle budget is measured against — see `firmware/src/bin/bench.rs`.
pub const BLOCK: usize = 32;

/// Anything below this magnitude is flushed to zero.
///
/// Long envelope and filter tails decay toward denormal floats, which on some
/// cores trap to microcode and cost orders of magnitude more than a normal
/// operation. The symptom is a synth that gets slower the longer it runs.
/// Cheaper to clamp than to debug.
pub const DENORMAL_FLOOR: f32 = 1.0e-9;

/// Full parameter set for the engine.
///
/// Plain data, `Copy`, no interior state. The host renderer sweeps these to
/// audition variations; the firmware will eventually populate them from MIDI CC.
#[derive(Clone, Copy)]
#[cfg_attr(feature = "debug-params", derive(Debug))]
pub struct Params {
    /// Kick drum parameters.
    pub kick: KickParams,
    /// Snare parameters.
    pub snare: SnareParams,
    /// Closed hat parameters.
    pub hat: HatParams,
    /// Post-sum output gain, linear.
    pub master_gain: f32,
}

impl Default for Params {
    fn default() -> Self {
        Self {
            kick: KickParams::default(),
            snare: SnareParams::default(),
            hat: HatParams::default(),
            master_gain: 0.8,
        }
    }
}

/// Which voice a trigger is addressed to.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum VoiceId {
    /// Kick drum.
    Kick,
    /// Snare drum.
    Snare,
    /// Closed hi-hat.
    Hat,
}

impl VoiceId {
    /// Number of distinct voices.
    pub const COUNT: usize = 3;
}

/// The engine.
///
/// Construct once, hold for the lifetime of the program. Everything it needs
/// lives inside it.
pub struct DrumEngine {
    kick: Kick,
    snare: Snare,
    hat: Hat,
    params: Params,
}

impl Default for DrumEngine {
    fn default() -> Self {
        Self::new()
    }
}

impl DrumEngine {
    /// Build an engine with default parameters.
    ///
    /// Not `const fn` because coefficient tables need real maths. Call it once
    /// at startup, before the audio interrupt is enabled.
    pub fn new() -> Self {
        let params = Params::default();
        Self {
            kick: Kick::new(&params.kick),
            snare: Snare::new(&params.snare),
            hat: Hat::new(&params.hat),
            params,
        }
    }

    /// Replace the parameter set and recompute derived coefficients.
    ///
    /// Not real-time safe in the strict sense — it does a handful of `expf`
    /// calls. Call it from your main loop, not from the audio callback. On the
    /// firmware side that means double-buffering params and swapping a flag,
    /// which is left as an exercise because the right answer depends on
    /// whether you end up on RTIC or Embassy.
    pub fn set_params(&mut self, params: &Params) {
        self.params = *params;
        self.kick.set_params(&params.kick);
        self.snare.set_params(&params.snare);
        self.hat.set_params(&params.hat);
    }

    /// Current parameters.
    pub fn params(&self) -> &Params {
        &self.params
    }

    /// Trigger a voice.
    ///
    /// `velocity` is 0.0..=1.0. Retriggering a sounding voice restarts it —
    /// these are one-shots, there is no note-off.
    pub fn trigger(&mut self, voice: VoiceId, velocity: f32) {
        let v = velocity.clamp(0.0, 1.0);
        match voice {
            VoiceId::Kick => self.kick.trigger(v),
            VoiceId::Snare => self.snare.trigger(v),
            VoiceId::Hat => self.hat.trigger(v),
        }
    }

    /// Silence everything immediately.
    pub fn panic(&mut self) {
        self.kick.reset();
        self.snare.reset();
        self.hat.reset();
    }

    /// True if any voice is still producing output.
    ///
    /// Useful in the host renderer to know when a tail has finished; on target
    /// you could use it to skip work, though for three voices the branch
    /// probably costs more than it saves.
    pub fn is_active(&self) -> bool {
        self.kick.is_active() || self.snare.is_active() || self.hat.is_active()
    }

    /// Render one block into planar stereo buffers.
    ///
    /// This is the hot path. It allocates nothing, branches minimally, and
    /// contains no divides. Everything it calls is `#[inline]`.
    ///
    /// Planar rather than interleaved because the voices are mono and summing
    /// into two separate buffers avoids a stride-2 access pattern. The
    /// firmware interleaves on the way out to the SAI DMA buffer, which is one
    /// cheap pass rather than a scattered write per voice.
    ///
    /// # Panics
    ///
    /// Debug builds assert both slices are exactly [`BLOCK`] long. Release
    /// builds process `min(len_l, len_r, BLOCK)` frames.
    pub fn process(&mut self, out_l: &mut [f32], out_r: &mut [f32]) {
        debug_assert_eq!(out_l.len(), BLOCK, "left buffer must be BLOCK frames");
        debug_assert_eq!(out_r.len(), BLOCK, "right buffer must be BLOCK frames");

        let n = out_l.len().min(out_r.len()).min(BLOCK);
        let gain = self.params.master_gain;

        for i in 0..n {
            let mut sum = 0.0f32;
            sum += self.kick.tick();
            sum += self.snare.tick();
            sum += self.hat.tick();

            let s = dsp::fast::soft_clip(sum * gain);

            // Mono engine fanned out to stereo. Per-voice panning would go
            // here, at the cost of a second multiply per voice per sample.
            out_l[i] = s;
            out_r[i] = s;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn render_blocks(engine: &mut DrumEngine, blocks: usize) -> f32 {
        let mut l = [0.0f32; BLOCK];
        let mut r = [0.0f32; BLOCK];
        let mut peak = 0.0f32;
        for _ in 0..blocks {
            engine.process(&mut l, &mut r);
            for &s in l.iter() {
                peak = peak.max(libm::fabsf(s));
            }
        }
        peak
    }

    #[test]
    fn silent_until_triggered() {
        let mut e = DrumEngine::new();
        assert_eq!(render_blocks(&mut e, 16), 0.0);
        assert!(!e.is_active());
    }

    #[test]
    fn kick_makes_noise_then_stops() {
        let mut e = DrumEngine::new();
        e.trigger(VoiceId::Kick, 1.0);
        assert!(render_blocks(&mut e, 4) > 0.1, "kick should be audible");

        // Five seconds is far longer than any sane decay.
        render_blocks(&mut e, (5.0 * SAMPLE_RATE / BLOCK as f32) as usize);
        assert!(!e.is_active(), "kick should have decayed to silence");
    }

    #[test]
    fn output_never_exceeds_unity() {
        // Everything at once, full velocity, repeatedly retriggered. The soft
        // clipper should hold the bus inside [-1, 1] regardless.
        let mut e = DrumEngine::new();
        let mut l = [0.0f32; BLOCK];
        let mut r = [0.0f32; BLOCK];

        for _ in 0..200 {
            e.trigger(VoiceId::Kick, 1.0);
            e.trigger(VoiceId::Snare, 1.0);
            e.trigger(VoiceId::Hat, 1.0);
            e.process(&mut l, &mut r);
            for &s in l.iter() {
                assert!(s.abs() <= 1.0, "clipper let {s} through");
                assert!(s.is_finite(), "non-finite sample");
            }
        }
    }

    #[test]
    fn no_nans_from_extreme_params() {
        let mut e = DrumEngine::new();
        let mut p = Params::default();
        p.kick.decay_s = 0.0;
        p.kick.pitch_decay_s = 0.0;
        p.snare.decay_s = 0.0;
        p.hat.decay_s = 0.0;
        p.master_gain = 100.0;
        e.set_params(&p);

        e.trigger(VoiceId::Kick, 1.0);
        e.trigger(VoiceId::Snare, 1.0);
        e.trigger(VoiceId::Hat, 1.0);

        let mut l = [0.0f32; BLOCK];
        let mut r = [0.0f32; BLOCK];
        for _ in 0..64 {
            e.process(&mut l, &mut r);
            for &s in l.iter() {
                assert!(s.is_finite(), "degenerate params produced {s}");
            }
        }
    }
}
