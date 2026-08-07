//! DSP primitives.
//!
//! Small, `#[inline]`, stateful structs. Each owns its coefficients and
//! recomputes them only when a setter is called, never per sample.
//!
//! The split that matters here is between *setup* maths and *hot path* maths.
//! Setup can call `libm::expf` freely — it happens when a parameter changes,
//! at UI rate. The hot path gets [`fast`], which trades accuracy for cycles.

pub mod ahd;
pub mod env;
pub mod fast;
pub mod filter;
pub mod fx;
pub mod lfo;
pub mod noise;
pub mod osc;
pub mod svf;

pub use ahd::AhdEnv;
pub use env::DecayEnv;
pub use filter::{OnePoleHp, OnePoleLp};
pub use fx::SendFx;
pub use lfo::{Lfo, LfoMode, LfoWave, ModDest};
pub use noise::Noise;
pub use osc::SineOsc;
pub use svf::{Svf, SvfMode};

/// Convert a decay time in seconds to a one-pole coefficient.
///
/// The result is the per-sample multiplier that takes a signal to roughly
/// -60dB after `seconds`. Setup-time only: this calls `expf`.
///
/// A zero or negative time yields 0.0, which decays instantly rather than
/// producing a NaN. Degenerate parameters should be boring, not explosive.
#[inline]
pub fn decay_coeff(seconds: f32, sample_rate: f32) -> f32 {
    if seconds <= 0.0 {
        return 0.0;
    }
    // -6.907755 is ln(0.001), i.e. -60dB.
    libm::expf(-6.907_755 / (seconds * sample_rate))
}

/// Linear interpolation.
#[inline(always)]
pub fn lerp(a: f32, b: f32, t: f32) -> f32 {
    a + (b - a) * t
}
