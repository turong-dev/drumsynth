//! Cheap approximations for the hot path.
//!
//! Everything here is called once or more per sample per voice. That is the
//! budget that matters, so this module is where you look first when
//! `firmware/src/bin/bench.rs` says you are over.
//!
//! The rule: anything the host renderer can hear the difference in belongs
//! elsewhere. These are all deliberately behind named functions so you can
//! change an implementation and immediately A/B it by re-rendering.
//!
//! [`sin_turns`] is the one that earned its keep: the original `libm::sinf`
//! body cost ~1,200 cycles on the M7 and ate 90% of the 3-voice budget. The
//! 512-entry quarter-wave table here drops that to ~10.

/// Sine of a phase expressed in turns, i.e. `0.0..1.0` maps to one cycle.
///
/// Table-driven: a 512-entry quarter-wave table with linear interpolation,
/// built at compile time. Roughly 10 cycles per call on an M7 vs ~1,200 for
/// `libm::sinf` — the difference that takes the engine from 31% of budget
/// for 3 voices to single-digit percent, and makes 8-track playback fit.
///
/// Input is assumed to lie in `[0, 1)` (a wrapping oscillator guarantees
/// this); broader positive inputs wrap cheaply via a truncating cast. The
/// interpolation error peaks near the quarter-wave crest at ~1e-6, inaudible
/// for percussion and below the level where a re-render is audibly different
/// from the `libm::sinf` reference.
///
/// The swap point is here, behind this one function, precisely so that A/B
/// against the old `libm::sinf` body is a single edit and the host renderer
/// immediately tells you whether you can hear the difference.
#[inline(always)]
pub fn sin_turns(turns: f32) -> f32 {
    // Cheap positive wrap: a truncating cast to i32 gives the integer part
    // for magnitudes well under 2^24, which covers any sane phase. No
    // `floorf` — that is itself a libm call we would rather not pay for.
    let phase = turns - (turns as i32) as f32;
    let q4 = phase * 4.0;
    let qi = q4 as i32 as usize & 3; // quadrant 0..3, edge-safe
    let f = q4 - (q4 as i32) as f32; // within-quadrant frac, [0, 1)
                                     // Q0,Q2 read the table forward; Q1,Q3 reflect it about the crest.
    let forward = (qi & 1) == 0;
    // Q2,Q3 invert the sign.
    let neg = qi >= 2;
    let p = if forward { f } else { 1.0 - f };
    let v = interp(p);
    if neg {
        -v
    } else {
        v
    }
}

/// Quarter-wave sine table: 512 intervals, 513 endpoints (index 512 = 1.0).
///
/// Built once at compile time by a Taylor series — see [`build_quarter`].
/// Lives in rodata, ~2 KB.
/// On the Teensy this lives in DTCM, not `.rodata`.
///
/// `t4link.x` aliases `REGION_RODATA` to OCRAM, which is reached over the AXI
/// bus with no L1 data cache enabled. This table is gathered one to six times
/// per sample per voice at a data-dependent index — close to the worst access
/// pattern for uncached memory there is. `REGION_DATA` is DTCM, so naming
/// `.data` moves it there; the runtime copies it out of flash at startup.
///
/// Gated on `target_os = "none"`, because section names are platform
/// specific: `.data` is meaningless to the Mach-O linker and the host build
/// of this crate (renderer, tests) must keep the default placement.
// `link_section` trips the crate-level `deny(unsafe_code)`. The placement is
// sound: this is an immutable table with a const initialiser, and `.data` is
// exactly where a non-zero initialised static would go anyway — only the
// region alias differs.
#[allow(unsafe_code)]
#[cfg_attr(target_os = "none", link_section = ".data")]
static QUARTER: [f32; 513] = build_quarter();

/// Compile-time quarter-wave sine via Taylor to the x^13 term.
///
/// Truncation error from the omitted x^15 term is below 3e-10 over
/// `[0, pi/2]`, well under f32 quantization; the table is as accurate as
/// the format allows.
const fn build_quarter() -> [f32; 513] {
    let mut t = [0.0f32; 513];
    let mut i = 0;
    while i <= 512 {
        let x = (i as f32) * (core::f32::consts::FRAC_PI_2 / 512.0);
        t[i] = sin_taylor(x);
        i += 1;
    }
    t
}

/// `sin(x)` for `x` in `[0, pi/2]` via Taylor series through `x^13/13!`.
const fn sin_taylor(x: f32) -> f32 {
    let x2 = x * x;
    let mut term = x; // x^1
    let mut sum = term; // + x
                        // 3!, 5!, 7!, 9!, 11!, 13!
    let denom = [6.0, 120.0, 5040.0, 362_880.0, 39_916_800.0, 6_227_020_800.0];
    let mut sign = -1.0;
    let mut k = 0;
    while k < 6 {
        term *= x2; // x^3, x^5, x^7, ...
        sum += (term / denom[k]) * sign;
        sign = -sign;
        k += 1;
    }
    sum
}

/// Linear interpolation into [`QUARTER`] at position `p` in `[0, 1]`, where
/// `p = 0` is the trough-side zero crossing and `p = 1` is the crest.
#[inline(always)]
fn interp(p: f32) -> f32 {
    let fidx = p * 512.0;
    let i0 = (fidx as i32 as usize).min(511);
    let frac = fidx - (i0 as f32);
    QUARTER[i0] + (QUARTER[i0 + 1] - QUARTER[i0]) * frac
}

/// Fast `1/x` for `x >= 2.0`: Newton iteration from an integer bit-hack seed.
///
/// The M7's `vdiv` has *data-dependent* latency (the hardware divider early-
/// exits on operand magnitudes), so a branchless loop's timing ends up
/// correlated with signal level — that is the measurable `8 FX idle` vs
/// `8 + FX` bench delta. This implementation is division-free: a fixed
/// sequence of multiplies and subtracts with identical latency for every
/// input, converging to within ~1.5 ulp of `1/x` across the range this
/// module actually uses (`[27, 108]`, the `soft_clip` denominator).
///
/// The seed uses the fast-inverse-square-root trick's reciprocal cousin:
/// `0x7EF311C3 - bits` is an estimate of `1/x` accurate to ~2^-8, and each
/// Newton step squares the error (2^-8 → 2^-16 → 2^-32 after three steps,
/// landing well below f32's 24-bit mantissa). For `x < 2.0` the seed degrades,
/// so the caller's domain must stay in the validated range — see [`soft_clip`].
#[inline(always)]
fn recip(x: f32) -> f32 {
    // Domain note: this is only called with x >= 2.0 (soft_clip's
    // denominator is 27 + 9x^2 >= 27). The seed's validity starts at ~2.
    debug_assert!(x >= 2.0, "recip: seed valid only for x >= 2.0, got {x}");
    let r0 = f32::from_bits(0x7EF3_11C3 - x.to_bits());
    let r1 = r0 * (2.0 - x * r0);
    let r2 = r1 * (2.0 - x * r1);
    r2 * (2.0 - x * r2)
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
    // ratios that misbehave. The reciprocal goes through [`recip`], which is
    // division-free — no data-dependent `vdiv` on the M7.
    let x = x.clamp(-3.0, 3.0);
    let x2 = x * x;
    x * (27.0 + x2) * recip(27.0 + 9.0 * x2)
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

/// Semitone-to-frequency ratio: `2^(semis / 12)`.
///
/// The transpose factor behind chromatic note tracking. One call per
/// trigger/control pass, never per sample. `semis` can be negative.
#[inline(always)]
pub fn semitone_ratio(semis: f32) -> f32 {
    exp2_approx(semis * (1.0 / 12.0))
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
    fn sin_turns_is_close_to_libm_across_a_cycle() {
        // The table with linear interpolation should stay well within 2e-6 of
        // the reference across a full turn. Worst case is near the crest where
        // the second derivative is largest.
        let mut max_err = 0.0f32;
        let mut i = 0;
        while i < 1000 {
            let turns = (i as f32) / 1000.0;
            let approx = sin_turns(turns);
            let exact = libm::sinf(turns * core::f32::consts::TAU);
            max_err = max_err.max(libm::fabsf(approx - exact));
            i += 1;
        }
        assert!(max_err < 2e-6, "table error exceeded 2e-6: {max_err:e}");
    }

    #[test]
    fn sin_turns_stays_bounded() {
        let mut i = 0;
        while i < 100_000 {
            let turns = (i as f32) / 100_000.0;
            let s = sin_turns(turns);
            assert!(s.abs() <= 1.0, "sine escaped: {s} at {turns}");
            i += 1;
        }
    }

    #[test]
    fn sin_turns_wraps_positive_input() {
        // Inputs beyond [0,1) should fold in by full turns.
        approx::assert_abs_diff_eq!(sin_turns(1.0), 0.0, epsilon = 1e-6);
        approx::assert_abs_diff_eq!(sin_turns(1.25), 1.0, epsilon = 1e-6);
        approx::assert_abs_diff_eq!(sin_turns(3.75), -1.0, epsilon = 1e-6);
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
    fn soft_clip_reciprocal_is_within_2_ulp() {
        // The division-free Newton `recip` must stay within ~2 ulp of the
        // true 1/x across the whole soft_clip denominator domain [27, 108].
        // This is what keeps the curve audibly identical to the rational
        // approximation while removing the M7's data-dependent vdiv.
        let mut max_ulp = 0.0f32;
        for i in 0..200_000 {
            let x = 27.0 + (i as f32) * 81.0 / 200_000.0;
            let exact = 1.0 / x;
            let approx = recip(x);
            let ulp = ((approx - exact) / exact).abs() / 1.19e-7;
            max_ulp = max_ulp.max(ulp);
        }
        assert!(
            max_ulp < 2.0,
            "recip escaped 2 ulp on soft_clip domain: {max_ulp}"
        );
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

    #[test]
    fn semitone_ratio_is_octave_accurate() {
        approx::assert_relative_eq!(semitone_ratio(0.0), 1.0, max_relative = 1e-4);
        approx::assert_relative_eq!(semitone_ratio(12.0), 2.0, max_relative = 0.002);
        approx::assert_relative_eq!(semitone_ratio(-12.0), 0.5, max_relative = 0.002);
        // A perfect fifth ≈ 1.4983.
        approx::assert_relative_eq!(semitone_ratio(7.0), 1.4983, max_relative = 0.003);
    }
}
