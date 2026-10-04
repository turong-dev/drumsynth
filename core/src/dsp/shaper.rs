//! First-order antiderivative-antialiased (ADAA) waveshaper.
//!
//! A float-native alternative to the vendored `warps::Modulator` in the
//! mi-drum strip, built to the same parameter surface so the two can be
//! swapped at runtime and measured against each other in one bench pass.
//!
//! # Why this exists
//!
//! Measured on hardware, the Warps stage costs **82,183 cycles per track per
//! 32-sample block** at the shipped default algorithm — 2,568 cycles per
//! sample — and **43,952** even with the cheapest algorithm it has. Six
//! tracks of it is 290% of the per-block budget on its own.
//!
//! Almost none of that is the sound. Three things make it expensive, and all
//! three are properties of Warps being a *module* rather than a strip stage:
//!
//! 1. **It oversamples 6x unconditionally.** Two 6x upsamplers and one
//!    downsampler, 48-tap polyphase, which is `3 * 48 = 144` multiply-
//!    accumulates per sample before any cross-modulation happens. It runs
//!    even for `ALGORITHM_XFADE`, which is linear and generates nothing to
//!    alias.
//! 2. **It is a 16-bit module.** `Modulator::Process` takes and returns
//!    `ShortFrame`, so the shim round-trips `f32 -> int16 -> f32` per sample
//!    per channel — cycles spent, and a 32-bit signal path quantised to 16
//!    bits, to model an ADC and a DAC that are not there.
//! 3. **It assumes a whole CPU.** One stereo pair at 96 kHz on an STM32F4
//!    with nothing else running, against six instances at 48 kHz sharing a
//!    core with MIDI, USB, the grid and the sequencer.
//!
//! # How ADAA replaces the oversampling
//!
//! Oversampling is not doing anything audible; it is there so the
//! nonlinearity's harmonics have somewhere to go other than folding back down
//! the spectrum. First-order ADAA buys the same thing analytically. For a
//! memoryless nonlinearity `f` with antiderivative `F`:
//!
//! ```text
//!            F(x[n]) - F(x[n-1])
//!     y[n] = -------------------
//!              x[n] - x[n-1]
//! ```
//!
//! which is the average of `f` over the segment between consecutive samples
//! rather than its instantaneous value — a one-sample box filter applied
//! before the sampling, where it still helps. It suppresses aliasing by
//! roughly what 2-4x oversampling gets, for one extra `F` evaluation (the
//! previous one is cached), a subtract and a divide. No sample rate
//! conversion at all, so the 144 MACs/sample go away entirely.
//!
//! Two caveats, both handled below. The quotient is `0/0` when consecutive
//! samples are equal and loses precision when they are merely close, so under
//! [`ADAA_EPS`] it falls back to evaluating `f` at the midpoint. And it is
//! *first* order: it does not remove aliasing, it attenuates it. For
//! percussion through a stage mixed at `WARP.MIX = 0.35` that is a different
//! trade than for a sustained tone, which is the trade this stage is for.
//!
//! # Choosing nonlinearities that are cheap to integrate
//!
//! ADAA is only cheap if `F` is. That rules some shapes out: the Padé tanh in
//! [`fast::soft_clip`] integrates to a logarithm, which is exactly the kind of
//! libm call in an audio path this engine has been paying for elsewhere. The
//! two here were picked for their antiderivatives:
//!
//! - **Cubic soft clip**, `x - x³/3`, flat at `±2/3`. Polynomial, so `F` is
//!   polynomial: `x²/2 - x⁴/12`, and a line beyond the knee.
//! - **Sine folder**, `sin(πx)`. `F` is `-cos(πx)/π`, and this engine already
//!   has a quarter-wave sine table — so `F` is the same table lookup as `f`,
//!   a quarter turn along. [`fast::sin_turns`] costs one interpolation.
//!
//! Ring modulation is not a one-dimensional memoryless nonlinearity, so ADAA
//! does not apply to it. It aliases only when its inputs carry content above
//! `fs/4`, so the modulator gets a one-pole low-pass instead — three
//! operations in place of 144.

use super::fast;
use super::filter::{cutoff_coeff, OnePoleLp};

/// Below this separation between consecutive samples, the ADAA difference
/// quotient is replaced by evaluating the nonlinearity at the midpoint.
///
/// The quotient is `0/0` at `dx == 0` and loses significance long before
/// that: `F(x) - F(x_prev)` is a subtraction of two nearly equal `f32`, so
/// the numerator's relative error grows as the denominator shrinks. `1e-4`
/// is well above where that bites in `f32` and well below a step size any
/// real signal spends time at, so the fallback fires on silence and on the
/// flat part of a clipped waveform, which is where it is also exactly right.
const ADAA_EPS: f32 = 1.0e-4;

/// Keeps the argument handed to [`fast::sin_turns`] positive.
///
/// `sin_turns` wraps its input with a truncating cast, which is correct for
/// positive arguments and *not* for negative ones — `-0.3` truncates to `0`
/// and leaves a negative phase that walks off the end of the quarter-wave
/// table. A whole number of turns changes nothing about the sine, so biasing
/// by 16 costs one add and makes every input this stage can produce safe.
const SIN_BIAS: f32 = 16.0;

/// Maximum input gain into the saturator at full drive.
const MAX_DRIVE_GAIN: f32 = 8.0;

/// How many times the folder wraps at full timbre.
const MAX_FOLD_DEPTH: f32 = 4.0;

/// One first-order ADAA state: the previous input and the previous `F`.
///
/// `F(x_prev)` is cached rather than recomputed, which halves the number of
/// antiderivative evaluations — the whole technique costs one `F` per sample,
/// not two.
#[derive(Clone, Copy, Default)]
struct Adaa1 {
    x_prev: f32,
    big_f_prev: f32,
}

impl Adaa1 {
    #[inline(always)]
    fn tick(&mut self, x: f32, f: impl Fn(f32) -> f32, big_f: impl Fn(f32) -> f32) -> f32 {
        let big_f_x = big_f(x);
        let dx = x - self.x_prev;
        // `abs` rather than `libm::fabsf`: this is a sign-bit mask the
        // compiler emits as `vabs`, not a call.
        let y = if (if dx < 0.0 { -dx } else { dx }) < ADAA_EPS {
            f(0.5 * (x + self.x_prev))
        } else {
            (big_f_x - self.big_f_prev) / dx
        };
        self.x_prev = x;
        self.big_f_prev = big_f_x;
        y
    }

    #[inline]
    fn reset(&mut self) {
        self.x_prev = 0.0;
        self.big_f_prev = 0.0;
    }
}

/// Cubic soft clip: `x - x³/3`, saturating to `±2/3`.
#[inline(always)]
fn clip3(x: f32) -> f32 {
    if x <= -1.0 {
        -2.0 / 3.0
    } else if x >= 1.0 {
        2.0 / 3.0
    } else {
        x - x * x * x * (1.0 / 3.0)
    }
}

/// Antiderivative of [`clip3`].
///
/// Even, because `clip3` is odd. Inside the knee it is `x²/2 - x⁴/12`;
/// outside, `clip3` is constant so its integral is the line `|x|·2/3 - 1/4`,
/// where `-1/4` is the constant that makes the two pieces meet at `|x| = 1`.
#[inline(always)]
fn clip3_int(x: f32) -> f32 {
    let a = if x < 0.0 { -x } else { x };
    if a >= 1.0 {
        a * (2.0 / 3.0) - 0.25
    } else {
        let x2 = x * x;
        x2 * 0.5 - x2 * x2 * (1.0 / 12.0)
    }
}

/// Sine wavefolder: `sin(πx)`.
#[inline(always)]
fn fold(x: f32) -> f32 {
    fast::sin_turns(0.5 * x + SIN_BIAS)
}

/// Antiderivative of [`fold`]: `-cos(πx)/π`.
///
/// `cos(θ) = sin(θ + π/2)`, and `sin_turns` takes turns, so the quarter turn
/// is a `+0.25` on the argument: the same table lookup as [`fold`], offset.
#[inline(always)]
fn fold_int(x: f32) -> f32 {
    -fast::sin_turns(0.5 * x + SIN_BIAS + 0.25) * core::f32::consts::FRAC_1_PI
}

/// Dead zone of the diode model: below this the diode does not conduct.
const DIODE_KNEE: f32 = 0.667;

/// Combined scale of Warps' diode approximation.
///
/// Upstream writes it as `dead_zone += fabs(dead_zone)` — a branchless
/// `max(d, 0) * 2` — then squares, then scales by `0.0432476...`. Squaring the
/// doubled value is `4x`, so the whole thing is `4 * 0.0432476` applied to
/// `max(|x| - knee, 0)²`. Folded into one constant here because the
/// antiderivative needs the closed form anyway.
const DIODE_SCALE: f32 = 0.173_190_63;

/// Diode ring-modulator nonlinearity, after Julian Parker (DAFx-11) — the
/// model Warps uses for `ALGORITHM_ANALOG_RING_MODULATION`.
///
/// Zero inside the dead zone, quadratic outside, odd overall. Kept because it
/// is the one piece of Warps' character that is genuinely cheap: no table, no
/// transcendental, and a cubic antiderivative.
#[inline(always)]
fn diode(x: f32) -> f32 {
    let a = (if x < 0.0 { -x } else { x }) - DIODE_KNEE;
    if a <= 0.0 {
        0.0
    } else {
        let m = DIODE_SCALE * a * a;
        if x < 0.0 {
            -m
        } else {
            m
        }
    }
}

/// Antiderivative of [`diode`]. Even, because `diode` is odd.
#[inline(always)]
fn diode_int(x: f32) -> f32 {
    let a = (if x < 0.0 { -x } else { x }) - DIODE_KNEE;
    if a <= 0.0 {
        0.0
    } else {
        DIODE_SCALE * a * a * a * (1.0 / 3.0)
    }
}

/// The shaping algorithms, in the order the `algorithm` parameter sweeps
/// them. Adjacent entries are crossfaded, as Warps does with its own table.
///
/// # This does not line up with Warps, and that matters for A/B
///
/// Warps dispatches on `min(algorithm * 8, 5.999)` across **six** table
/// entries; this sweeps **three**. So the same `WARP.ALG` value selects
/// different things in the two stages — at 0.5, Warps is running
/// `XOR + COMPARATOR` while this is running the sine fold. Any comparison
/// made by setting one macro and swapping the stage underneath is therefore
/// comparing two *different effects*, not two implementations of one, and a
/// render A/B at a fixed macro value will mislead you in whichever direction
/// the algorithms happen to differ.
///
/// Matching them needs a per-stage macro value, or a common algorithm map
/// both stages are driven from. Until that exists, compare at `algorithm =
/// 0.0`, where both are at the crossfade end of their tables and the stages
/// are doing the nearest thing to the same job.
const ALGORITHM_COUNT: usize = 4;

/// A two-input waveshaping stage: ADAA saturation, ADAA folding, and ring
/// modulation, with the parameter surface of the Warps stage it replaces.
///
/// All three algorithms are evaluated every sample and the selected pair
/// blended, rather than evaluating only the pair in use. That costs more than
/// Warps' dispatch does, and it is still cheap: it keeps every ADAA state
/// advancing on a continuous input, so moving the algorithm knob cannot click
/// on a stale `x_prev`. Buying continuity with arithmetic is the right way
/// round at these magnitudes.
pub struct Shaper {
    bypass: bool,
    /// Blend position across [`ALGORITHM_COUNT`], pre-scaled.
    algorithm: f32,
    timbre: f32,
    /// Input gain into the saturators, derived from `drive` at control rate.
    drive_gain: f32,

    sat_carrier: Adaa1,
    sat_modulator: Adaa1,
    folder: Adaa1,
    /// The diode ring runs two nonlinearities, on the sum and the difference
    /// of the two inputs, so it needs an ADAA state for each.
    diode_sum: Adaa1,
    diode_diff: Adaa1,
    /// Band-limits the modulator so the ring-modulation product stays under
    /// Nyquist. This is the substitute for oversampling on the one algorithm
    /// ADAA cannot cover.
    mod_lp: OnePoleLp,
}

impl Shaper {
    /// `sample_rate` fixes the modulator band-limit at `fs/4`, which is the
    /// bound that keeps a product of two signals inside Nyquist.
    pub fn new(sample_rate: f32) -> Self {
        let mod_lp = OnePoleLp::new(cutoff_coeff(sample_rate * 0.25, sample_rate));
        Self {
            bypass: false,
            algorithm: 0.0,
            timbre: 0.5,
            drive_gain: 1.0,
            sat_carrier: Adaa1::default(),
            sat_modulator: Adaa1::default(),
            folder: Adaa1::default(),
            diode_sum: Adaa1::default(),
            diode_diff: Adaa1::default(),
            mod_lp,
        }
    }

    /// Bypass detent, matching `warps::Modulator::set_bypass`: the carrier
    /// passes through untouched and the aux tap carries the modulator.
    #[inline]
    pub fn set_bypass(&mut self, bypass: bool) {
        self.bypass = bypass;
    }

    /// Control-rate parameters, mirroring the Warps call this replaces.
    ///
    /// `drive` arrives already remapped by the strip onto `0.50..1.00` (see
    /// `warps_drive_from_macro`), so the usable span is the top half; it maps
    /// linearly onto an input gain of `1.0..`[`MAX_DRIVE_GAIN`]. Warps' own
    /// `carrier_shape` and `note` arguments have no analogue here: the strip
    /// pinned `Carrier::External` permanently, which left `note` unread.
    #[inline]
    pub fn set_parameters(&mut self, algorithm: f32, timbre: f32, drive: f32) {
        let a = algorithm.clamp(0.0, 1.0);
        // Scaled so the top of the knob lands *on* the last algorithm rather
        // than a hair below it, and clamped off the end so the integral part
        // can always index `i + 1`.
        self.algorithm = (a * (ALGORITHM_COUNT - 1) as f32).min(ALGORITHM_COUNT as f32 - 1.001);
        self.timbre = timbre.clamp(0.0, 1.0);
        let d = ((drive.clamp(0.0, 1.0) - 0.5) * 2.0).clamp(0.0, 1.0);
        self.drive_gain = 1.0 + d * (MAX_DRIVE_GAIN - 1.0);
    }

    /// Clear every filter and ADAA state. The strip calls this when a voice
    /// is reset, so a new note does not inherit the previous one's `x_prev`.
    pub fn reset(&mut self) {
        self.sat_carrier.reset();
        self.sat_modulator.reset();
        self.folder.reset();
        self.diode_sum.reset();
        self.diode_diff.reset();
        self.mod_lp.reset();
    }

    /// Shape `carrier` in place against `modulator`, writing the auxiliary
    /// tap to `aux_out`.
    ///
    /// Signature and semantics match `mi_dsp::warps::Warps::process_dual`, so
    /// the strip can hold both and pick one. The aux tap is the sum of the
    /// two saturated inputs, which is what Warps puts there.
    pub fn process_dual(&mut self, carrier: &mut [f32], modulator: &[f32], aux_out: &mut [f32]) {
        let n = carrier.len().min(modulator.len()).min(aux_out.len());

        if self.bypass {
            // Warps' bypass copies both channels through, so the aux tap
            // carries the modulator rather than going silent.
            aux_out[..n].copy_from_slice(&modulator[..n]);
            return;
        }

        let gain = self.drive_gain;
        let timbre = self.timbre;
        let fold_depth = 1.0 + timbre * MAX_FOLD_DEPTH;
        // Upstream's `4 + parameter * 24`.
        let diode_gain = 4.0 + timbre * 24.0;

        // Equal-power crossfade gains, control rate.
        //
        // Linear would be cheaper still and is wrong twice over. It dips ~3 dB
        // in the middle for two uncorrelated signals, which is audible as a
        // hole as the knob sweeps; and at `timbre = 0.5` it collapses to
        // `(c + m) / 2`, which is bit-for-bit the aux tap — making `WARP.OUT`
        // inert at the shipped default. Warps uses `lut_xfade_in`/`out` for
        // the same reason. `timbre` is per-chunk, so the two lookups cost
        // nothing per sample.
        let fade_m = fast::sin_turns(0.25 * timbre);
        let fade_c = fast::sin_turns(0.25 + 0.25 * timbre);

        let idx = self.algorithm as usize;
        let frac = self.algorithm - idx as f32;

        for i in 0..n {
            // Both inputs through their own ADAA saturator, which is where
            // the drive character comes from and what feeds the aux tap.
            let c = self
                .sat_carrier
                .tick(carrier[i] * gain, clip3, clip3_int);
            let m = self
                .sat_modulator
                .tick(modulator[i] * gain, clip3, clip3_int);

            aux_out[i] = (c + m) * 0.5;

            // 0: crossfade. Linear in the signals, so it generates nothing
            //    to alias and needs no antialiasing treatment at all.
            let xfade = c * fade_c + m * fade_m;

            // 1: fold. The violent one, and the reason ADAA is here.
            //
            // Trimmed to the saturator's own ceiling. `fold` is a sine, so it
            // leaves full scale whatever goes in, while `clip3` tops out at
            // 2/3 — without this the fold position is ~3.5 dB louder than the
            // crossfade either side of it and the algorithm knob is a volume
            // control. Measured against Warps at `WARP.DRV` 0.9 and
            // `WARP.MIX` 1.0, the untrimmed version clipped at full scale
            // where Warps peaked at 0.21.
            let folded = self.folder.tick(c * fold_depth, fold, fold_int) * (2.0 / 3.0);

            // 2: analog ring. Warps' diode model, through ADAA on both
            // halves. The gain into the diodes is what `timbre` drives here,
            // as it does upstream, and `soft_clip` catches the top the way
            // Warps' `SoftLimit` does — the same Padé tanh, division-free.
            let c2 = c * 2.0;
            let d = self.diode_sum.tick(m + c2, diode, diode_int)
                + self.diode_diff.tick(m - c2, diode, diode_int);
            let analog_ring = fast::soft_clip(d * diode_gain) * (2.0 / 3.0);

            // 3: digital ring. Band-limited modulator rather than
            // oversampling. 1.5 for the level-matching reason above: two
            // signals each bounded by 2/3 multiply to 4/9, and 1.5 puts that
            // back on 2/3.
            let m_bl = self.mod_lp.tick(m);
            let ring = c * m_bl * 1.5;

            let algos = [xfade, folded, analog_ring, ring];
            let a = algos[idx];
            let b = algos[(idx + 1).min(ALGORITHM_COUNT - 1)];
            carrier[i] = a + (b - a) * frac;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The antiderivatives have to actually be antiderivatives, or ADAA
    /// produces a plausible-looking signal that is not the shaper's output.
    /// Checked numerically rather than by inspection: `(F(x+h) - F(x-h)) /
    /// 2h` must converge on `f(x)`.
    #[test]
    fn antiderivatives_differentiate_back_to_their_nonlinearity() {
        let h = 1.0e-3;
        for k in -40..=40 {
            let x = k as f32 * 0.1;
            // Skip the knees, where the derivative is not continuous and a
            // central difference straddles two pieces.
            if (x.abs() - 1.0).abs() < 2.0 * h {
                continue;
            }
            let num = (clip3_int(x + h) - clip3_int(x - h)) / (2.0 * h);
            assert!(
                (num - clip3(x)).abs() < 1.0e-3,
                "clip3 at {x}: numeric {num} vs {}",
                clip3(x)
            );

            if (x.abs() - DIODE_KNEE).abs() > 2.0 * h {
                let num = (diode_int(x + h) - diode_int(x - h)) / (2.0 * h);
                assert!(
                    (num - diode(x)).abs() < 1.0e-3,
                    "diode at {x}: numeric {num} vs {}",
                    diode(x)
                );
            }

            let num = (fold_int(x + h) - fold_int(x - h)) / (2.0 * h);
            assert!(
                (num - fold(x)).abs() < 1.0e-2,
                "fold at {x}: numeric {num} vs {}",
                fold(x)
            );
        }
    }

    /// ADAA must agree with the plain nonlinearity on a slowly moving signal:
    /// as the step between samples goes to zero the difference quotient is
    /// the derivative of `F`, which is `f`. A stage that fails this is
    /// filtering, not antialiasing.
    #[test]
    fn adaa_converges_on_the_direct_nonlinearity_when_oversampled() {
        let mut st = Adaa1::default();
        let mut worst = 0.0f32;
        // One slow cycle: consecutive samples differ by ~6e-4, comfortably
        // above ADAA_EPS, so this exercises the quotient and not the
        // fallback.
        for i in 0..10_000 {
            let x = 2.0 * fast::sin_turns(i as f32 / 10_000.0);
            let y = st.tick(x, clip3, clip3_int);
            let d = (y - clip3(x)).abs();
            if d > worst {
                worst = d;
            }
        }
        assert!(worst < 1.0e-2, "ADAA diverged from clip3 by {worst}");
    }

    /// The `dx -> 0` fallback has to be continuous with the quotient around
    /// it, or every near-flat passage gets a click.
    #[test]
    fn the_epsilon_fallback_matches_the_quotient_either_side_of_it() {
        for &x0 in &[-1.5f32, -0.4, 0.0, 0.3, 1.2] {
            let mut below = Adaa1::default();
            let mut above = Adaa1::default();
            below.tick(x0, clip3, clip3_int);
            above.tick(x0, clip3, clip3_int);
            // One step just inside the fallback, one just outside.
            let y_in = below.tick(x0 + ADAA_EPS * 0.5, clip3, clip3_int);
            let y_out = above.tick(x0 + ADAA_EPS * 4.0, clip3, clip3_int);
            assert!(
                (y_in - y_out).abs() < 1.0e-3,
                "fallback discontinuity at {x0}: {y_in} vs {y_out}"
            );
        }
    }

    /// Bypass must be bit-transparent on the carrier. The Warps path is not —
    /// its bypass still round-trips through int16 — so this is a property the
    /// replacement gains, and one worth pinning.
    #[test]
    fn bypass_passes_the_carrier_through_untouched() {
        let mut s = Shaper::new(48_000.0);
        s.set_bypass(true);
        let mut carrier = [0.3f32, -0.7, 0.9, -0.15];
        let before = carrier;
        let modulator = [0.1f32, 0.2, 0.3, 0.4];
        let mut aux = [0.0f32; 4];
        s.process_dual(&mut carrier, &modulator, &mut aux);
        assert_eq!(carrier, before);
        assert_eq!(aux, modulator);
    }

    /// Nothing in here may produce a NaN or run away, whatever it is handed.
    #[test]
    fn extreme_inputs_stay_finite() {
        let mut s = Shaper::new(48_000.0);
        for &algo in &[0.0f32, 0.25, 0.5, 0.75, 1.0] {
            for &drive in &[0.0f32, 0.5, 0.75, 1.0] {
                s.reset();
                s.set_parameters(algo, 1.0, drive);
                let mut carrier = [12.0f32, -9.0, 0.0, 1.0e-30, -1.0e-30, 4.0, -4.0, 0.0];
                let modulator = [-7.0f32, 3.0, 0.0, 1.0, -1.0, 1.0e-20, 0.0, 5.0];
                let mut aux = [0.0f32; 8];
                s.process_dual(&mut carrier, &modulator, &mut aux);
                for (i, v) in carrier.iter().chain(aux.iter()).enumerate() {
                    assert!(
                        v.is_finite(),
                        "non-finite at {i} for algo {algo} drive {drive}: {v}"
                    );
                }
            }
        }
    }

    /// No algorithm may be louder than the saturator that feeds it, or the
    /// algorithm knob doubles as a volume control and an A/B against another
    /// stage compares loudness rather than character. This is the property
    /// the first render A/B caught: the untrimmed folder clipped at full
    /// scale where Warps peaked at 0.21.
    #[test]
    fn the_algorithms_are_level_matched_to_each_other() {
        let mut levels = [0.0f32; 4];
        for (i, algo) in [0.0f32, 1.0 / 3.0, 2.0 / 3.0, 1.0].iter().enumerate() {
            let mut s = Shaper::new(48_000.0);
            s.set_parameters(*algo, 1.0, 1.0);
            let mut carrier = [0.0f32; 512];
            let mut modulator = [0.0f32; 512];
            for k in 0..512 {
                carrier[k] = 0.8 * fast::sin_turns(k as f32 * 7.0 / 512.0);
                modulator[k] = 0.8 * fast::sin_turns(k as f32 * 3.0 / 512.0);
            }
            let mut aux = [0.0f32; 512];
            s.process_dual(&mut carrier, &modulator, &mut aux);
            levels[i] = carrier.iter().fold(0.0f32, |a, v| a.max(v.abs()));
        }
        let hi = levels.iter().cloned().fold(0.0f32, f32::max);
        let lo = levels.iter().cloned().fold(f32::MAX, f32::min);
        // The saturator ceiling is 2/3; equal-power crossfade may reach
        // `sqrt(2)` of it where the two inputs correlate, which is the normal
        // behaviour of an equal-power law and not a level bug. Full scale is
        // the real bound.
        assert!(hi <= 1.0, "an algorithm reaches full scale: {levels:?}");
        assert!(
            hi / lo < 2.5,
            "algorithms differ by more than 8 dB: {levels:?}"
        );
    }

    /// Drive has to do something monotonic, or the knob is decorative.
    #[test]
    fn drive_increases_saturation() {
        let mut quiet = Shaper::new(48_000.0);
        let mut loud = Shaper::new(48_000.0);
        // Algorithm 0 at timbre 0 is the saturated carrier alone.
        quiet.set_parameters(0.0, 0.0, 0.5);
        loud.set_parameters(0.0, 0.0, 1.0);
        let mut a = [0.0f32; 64];
        let mut b = [0.0f32; 64];
        for i in 0..64 {
            let v = 0.4 * fast::sin_turns(i as f32 / 64.0);
            a[i] = v;
            b[i] = v;
        }
        let modulator = [0.0f32; 64];
        let mut aux = [0.0f32; 64];
        quiet.process_dual(&mut a, &modulator, &mut aux);
        loud.process_dual(&mut b, &modulator, &mut aux);
        let rms = |s: &[f32]| (s.iter().map(|v| v * v).sum::<f32>() / s.len() as f32).sqrt();
        assert!(
            rms(&b) > rms(&a) * 1.5,
            "drive did not raise level: {} vs {}",
            rms(&a),
            rms(&b)
        );
    }
}
