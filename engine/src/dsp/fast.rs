//! Cheap approximations for the hot path.
//!
//! Everything here is called once or more per sample per voice. That is the
//! budget that matters, so this module is where you look first when
//! `firmware/src/bin/bench.rs` says you are over.
//!
//! The rule: anything the host renderer can hear the difference in belongs
//! elsewhere. These are all deliberately behind named functions so you can
//! change an implementation and immediately A/B it by re-rendering.

/// Sine of a phase expressed in turns, i.e. `0.0..1.0` maps to one cycle.
///
/// # Currently exact, deliberately
///
/// This forwards to `libm::sinf`. It is correct, portable, and the most
/// expensive thing in the voice code.
///
/// Two well-trodden replacements, in increasing order of effort:
///
/// 1. A 512-entry quarter-wave table with linear interpolation. Around 2KB of
///    flash, a handful of cycles, and inaudible for percussion.
/// 2. A minimax polynomial on the quarter wave. No table, slightly more
///    arithmetic, better accuracy than option 1.
///
/// Do not reach for either until the bench harness gives you a reason.
/// Premature approximation costs you sound quality for cycles you had spare.
#[inline(always)]
pub fn sin_turns(turns: f32) -> f32 {
    libm::sinf(turns * core::f32::consts::TAU)
}

/// Soft saturation, roughly tanh-shaped.
///
/// A rational approximation rather than the real thing: `libm::tanhf` is
/// expensive and this is called on every output sample. The curve is close
/// enough through the interesting region and asymptotes correctly.
///
/// Output is bounded within `[-1, 1]`, which is what makes this usable as the
/// final safety net before the DAC.
#[inline(always)]
pub fn soft_clip(x: f32) -> f32 {
    // x * (27 + x^2) / (27 + 9x^2) is the classic Padé-style tanh
    // approximation. Clamp first so that very large inputs cannot produce
    // ratios that misbehave.
    let x = x.clamp(-3.0, 3.0);
    let x2 = x * x;
    x * (27.0 + x2) / (27.0 + 9.0 * x2)
}

/// Hard clip to `[-1, 1]`.
///
/// Use when you want the distortion character rather than transparency.
#[inline(always)]
pub fn hard_clip(x: f32) -> f32 {
    x.clamp(-1.0, 1.0)
}

/// Approximate `2^x` for `x` in a modest range.
///
/// Useful for exponential pitch sweeps, where the alternative is `powf` per
/// sample. Accurate to a few cents across a couple of octaves, which is well
/// inside what you would notice on a pitch-swept drum transient.
#[inline(always)]
pub fn exp2_approx(x: f32) -> f32 {
    // Split into integer and fractional parts; the integer part becomes an
    // exponent bias, the fraction goes through a cubic.
    let clamped = x.clamp(-30.0, 30.0);
    let i = libm::floorf(clamped);
    let f = clamped - i;

    // Cubic fit to 2^f over f in [0, 1).
    let poly = 1.0 + f * (0.6960656 + f * (0.2240402 + f * 0.0792041));

    let bits = ((i as i32 + 127) as u32) << 23;
    poly * f32::from_bits(bits)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sin_turns_matches_expectations() {
        approx::assert_abs_diff_eq!(sin_turns(0.0), 0.0, epsilon = 1e-6);
        approx::assert_abs_diff_eq!(sin_turns(0.25), 1.0, epsilon = 1e-6);
        approx::assert_abs_diff_eq!(sin_turns(0.5), 0.0, epsilon = 1e-6);
        approx::assert_abs_diff_eq!(sin_turns(0.75), -1.0, epsilon = 1e-6);
    }

    #[test]
    fn soft_clip_is_bounded_and_odd() {
        for i in -1000..=1000 {
            let x = i as f32 * 0.05;
            let y = soft_clip(x);
            assert!(y.abs() <= 1.0, "soft_clip({x}) = {y}");
            approx::assert_abs_diff_eq!(soft_clip(-x), -y, epsilon = 1e-6);
        }
    }

    #[test]
    fn soft_clip_is_near_linear_when_quiet() {
        // Below about -20dBFS it should be effectively transparent.
        for i in 1..100 {
            let x = i as f32 * 0.001;
            approx::assert_relative_eq!(soft_clip(x), x, max_relative = 0.003);
        }
    }

    #[test]
    fn exp2_approx_is_close_enough() {
        for i in -40..=40 {
            let x = i as f32 * 0.25;
            let approx_val = exp2_approx(x);
            let exact = libm::exp2f(x);
            approx::assert_relative_eq!(approx_val, exact, max_relative = 0.002);
        }
    }
}
