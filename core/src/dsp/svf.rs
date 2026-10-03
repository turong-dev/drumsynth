//! State-variable filter, Chamberlin form.
//!
//! Used by the per-track strip. Unlike the one-poles in [`super::filter`],
//! this gives a resonant multimode response (LP / BP / HP / Notch) from one
//! pair of integrators, with cutoff and resonance both controllable at
//! setup/CC rate.
//!
//! # Topology
//!
//! The classic Chamberlin digital SVF: damping feeds back the bandpass,
//! the highpass node drives two trapezoidal integrators in series. The
//! integrator coefficient `c1 = 2·sin(π·fc/fs)` and damping `k = 1/Q` are
//! precomputed in [`recalc`](Self::recalc); the per-sample path is two
//! multiplies and an add — no `sin` or `expf` in the hot path.
//!
//! # Stability
//!
//! The Chamberlin form only holds its pole inside the unit circle while
//! `c1² + 2·c1·k < 4`. That is *not* the same as staying below Nyquist: with
//! the default strip resonance (`Q = 0.695`, `k = 1.44`) that boundary is
//! 8.2 kHz and the ceiling actually enforced — the boundary less
//! [`STABILITY_MARGIN`] — is **6.4 kHz**, against a cutoff macro at 1.0 that
//! asks for 20 kHz. Past the boundary the
//! integrators pump, the output diverges to `inf` and then `NaN`, and it
//! arrives at the master clipper as a non-finite sample rather than as
//! obviously wrong audio.
//!
//! So both coefficient setters clamp to the Q-dependent stability ceiling
//! rather than to a fixed fraction of the sample rate. The bound is solved
//! once per [`recalc`](Svf::recalc) and cached as a coefficient, so the
//! per-sample [`set_cutoff`](Svf::set_cutoff) applies it with a single `min`
//! and stays free of transcendentals;
//! [`stability_ceiling_hz`] is the same bound expressed in Hz, for callers and
//! tests that want to reason about it as a frequency.
//!
//! The cost is that the top of the `RIP.CUT` / `STRIP.CUT` range stops raising
//! the cutoff once the ceiling is reached — at default resonance everything
//! above ~6.4 kHz is one setting. Lowering `Q` buys a higher ceiling, so the
//! resonance knob and the cutoff knob interact the way they do in the analog
//! circuit this models.

use crate::DENORMAL_FLOOR;

/// How far inside the stability boundary the cutoff is held.
///
/// The boundary is `c1² + 2·c1·k = 4`, but clamping to *exactly* it does not
/// work: `c1` is computed through `sinf`/`sin_turns` and then multiplied and
/// accumulated in f32, so the realised coefficient lands a fraction of an ulp
/// either side of the target. Sitting on the boundary therefore self-oscillates
/// — measured, not theorised: a kick that is silent above 4 kHz at the source
/// came out of the filter with 48% of its energy above 4 kHz, which is a
/// marginally-stable state-variable filter screaming rather than a signal.
///
/// Solving for 3.0 instead of 4.0 buys roughly 20% of headroom in `c1` (about
/// 2.2 stops at the default resonance) and stops it dead. The cost is a lower
/// maximum cutoff, which is the correct trade for a filter that must never
/// generate content of its own.
const STABILITY_MARGIN: f32 = 3.0;

/// Highest cutoff the Chamberlin integrators stay stable at, in Hz, for a
/// given Q and sample rate.
///
/// Solves `c1² + 2·c1·k = STABILITY_MARGIN` for `c1`, then inverts
/// `c1 = 2·sin(π·fc/fs)`.
///
/// Returns `sample_rate * 0.49` when the requested Q is low enough that the
/// bandwidth limit binds first — which it always is, since `c1` cannot exceed
/// 2 and so `fc` cannot exceed `fs/2`.
pub fn stability_ceiling_hz(q: f32, sample_rate: f32) -> f32 {
    // Same damping clamp as `recalc`: `k` is `1/Q`, floored so that a Q below
    // 0.5 cannot push the feedback past 2.
    let k = (1.0 / q.max(0.5)).min(2.0);
    // Larger root of c1² + 2·k·c1 − M = 0.
    let c1_max = -k + libm::sqrtf(k * k + STABILITY_MARGIN);
    // c1 = 2·sin(π·fc/fs)  ⇒  fc = asin(c1/2)·fs/π
    let ratio = (c1_max * 0.5).min(1.0);
    libm::asinf(ratio) * sample_rate / core::f32::consts::PI
}

/// Filter mode.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum SvfMode {
    /// Lowpass.
    Lp,
    /// Bandpass.
    Bp,
    /// Highpass.
    Hp,
    /// Notch (LP + HP summed).
    Notch,
    /// Bypass — copy through unchanged.
    Off,
}

/// A Chamberlin state-variable filter.
///
/// State is the two integrator outputs `y_l` (lowpass), `y_b` (bandpass).
/// Coefficients `c1 = 2·sin(π·fc/fs)` (integration rate) and `k = 1/Q`
/// (damping) are precomputed and never touched in the per-sample path.
/// Cutoff and resonance are taken as user-facing values
/// ([`recalc`](Self::recalc)) so callers can map from macros once at
/// control rate.
#[derive(Clone, Copy)]
pub struct Svf {
    y_l: f32,
    y_b: f32,
    c1: f32,
    k: f32,
    /// Largest `c1` the integrators stay stable at for the current `k`, i.e.
    /// the stability ceiling expressed as a coefficient rather than as a
    /// frequency. Cached because it depends only on `k`, which is setup rate.
    ///
    /// Clamping here rather than in Hz is what keeps
    /// [`set_cutoff`](Self::set_cutoff) free of transcendentals: inverting the
    /// bound into Hz needs `asinf`, comparing against it in the coefficient
    /// domain needs one `min`.
    c1_max: f32,
    mode: SvfMode,
}

impl Svf {
    /// Build a bypass filter at the given mode; coefficient is recomputed
    /// by the caller via [`recalc`](Self::recalc).
    pub const fn new(mode: SvfMode) -> Self {
        Self {
            y_l: 0.0,
            y_b: 0.0,
            c1: 0.0,
            k: 0.0,
            // The bound at `k = 0`: `sqrt(STABILITY_MARGIN)`. A `const fn`
            // cannot call `sqrtf`, and this is the value `recalc` would
            // compute before it has been told a Q, so a `set_cutoff` that
            // lands before the first `recalc` is clamped rather than loose.
            c1_max: 1.732_050_8,
            mode,
        }
    }

    /// Recompute coefficients from cutoff, Q and sample rate. Setup rate.
    ///
    /// `q` is the conventional Q factor — `0.707` is the Butterworth
    /// (maximally flat) point, `5.0` begins to ring, `20.0` nears
    /// self-oscillation. Values below `0.5` are clamped to keep the
    /// feedback stable.
    pub fn recalc(&mut self, cutoff_hz: f32, q: f32, sample_rate: f32) {
        self.k = (1.0 / q.max(0.5)).min(2.0);
        // The Q-dependent stability ceiling, as a coefficient. Solved here
        // and cached so the per-sample path can reuse it — see `c1_max` and
        // the module docs. Clamping to Nyquist alone is what let a wide-open
        // cutoff diverge to NaN at the default resonance.
        self.c1_max = -self.k + libm::sqrtf(self.k * self.k + STABILITY_MARGIN);
        // Bandwidth limit first, so `sin` stays on its monotonic quarter and
        // a cutoff past Nyquist cannot fold back to a *lower* coefficient.
        let f = cutoff_hz.clamp(1.0, sample_rate * 0.49);
        // 2·sin(π·fc/fs) is the Chamberlin integration coefficient.
        let c1 = 2.0 * libm::sinf(core::f32::consts::PI * f / sample_rate);
        self.c1 = c1.min(self.c1_max);
    }

    /// Refresh only the cutoff-dependent coefficient (`c1`), for a
    /// cutoff that moves every sample. Sample rate.
    ///
    /// `k` (damping from Q) is untouched — Q is setup-rate, so a caller
    /// that swept a cutoff per sample against a static Q (e.g. the SweepFX
    /// machine's LFO) pays one [`crate::dsp::fast::sin_turns`] lookup
    /// instead of the full [`recalc`](Self::recalc) (`libm::sinf`, ~1,200
    /// cycles on the M7 — the same split `BridgedT::set_hz` makes for the
    /// bridged-T). See the per-sample comment in `recalc`.
    ///
    /// The stability ceiling holds for a per-sample sweep too, but it is
    /// applied as a `min` against the cached [`c1_max`](Self::c1_max) rather
    /// than by converting the bound back into Hz: the inverse needs `asinf`,
    /// and this runs on every sample of every track. A caller that changes Q
    /// must call [`recalc`](Self::recalc) to pick up the new ceiling — `k` and
    /// the bound derived from it are both setup rate.
    #[inline(always)]
    pub fn set_cutoff(&mut self, cutoff_hz: f32, sample_rate: f32) {
        // Nyquist first: past `fs/2` the sine folds back down, and a `min`
        // against the ceiling would then wave a *lower* coefficient through
        // as if it were in range.
        let f = cutoff_hz.clamp(1.0, sample_rate * 0.49);
        // sin(π·fc/fs) = sin_turns(fc / (2·fs)) — the table takes turns.
        let c1 = 2.0 * crate::dsp::fast::sin_turns(f * 0.5 / sample_rate);
        self.c1 = c1.min(self.c1_max);
    }

    /// Choose the output mode without touching coefficients.
    #[inline]
    pub fn set_mode(&mut self, mode: SvfMode) {
        self.mode = mode;
    }

    /// Current filter mode.
    #[inline]
    pub fn mode(&self) -> SvfMode {
        self.mode
    }

    /// Clear the integrator state.
    #[inline]
    pub fn reset(&mut self) {
        self.y_l = 0.0;
        self.y_b = 0.0;
    }

    /// Advance one sample and return the selected output.
    ///
    /// Highpass drives the integrator pair: bandpass integrates the
    /// highpass, lowpass integrates the bandpass. Damping feeds the
    /// bandpass back (subtracted from the input). Two multiplies and two
    /// adds. Flushes the integrators to zero below [`DENORMAL_FLOOR`] so a
    /// ringing tail cannot trap to microcode via denormals.
    ///
    /// `Off` is a true bypass — the integrators are not advanced, so a
    /// default strip (filter mode `Off`) pays none of the per-sample filter
    /// cost. State is cleared on entering `Off` so a later switch back on
    /// starts from silence rather than a stale ring.
    #[inline(always)]
    pub fn tick(&mut self, x: f32) -> f32 {
        if self.mode == SvfMode::Off {
            return x;
        }

        let y_h = x - self.y_l - self.k * self.y_b;
        self.y_b += self.c1 * y_h;
        self.y_l += self.c1 * self.y_b;

        if libm::fabsf(self.y_b) < DENORMAL_FLOOR {
            self.y_b = 0.0;
        }
        if libm::fabsf(self.y_l) < DENORMAL_FLOOR {
            self.y_l = 0.0;
        }

        match self.mode {
            SvfMode::Lp => self.y_l,
            SvfMode::Bp => self.y_b,
            SvfMode::Hp => y_h,
            SvfMode::Notch => self.y_l + y_h,
            SvfMode::Off => x,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::SAMPLE_RATE;

    /// Steady-state gain at a given frequency injected through `f`.
    fn gain_at(hz: f32, mut f: impl FnMut(f32) -> f32) -> f32 {
        let inc = hz / SAMPLE_RATE;
        let mut phase = 0.0f32;
        let mut peak = 0.0f32;
        for i in 0..20_000 {
            let x = libm::sinf(phase * core::f32::consts::TAU);
            phase += inc;
            if phase >= 1.0 {
                phase -= 1.0;
            }
            let y = f(x);
            if i > 10_000 {
                peak = peak.max(libm::fabsf(y));
            }
        }
        peak
    }

    #[test]
    fn lowpass_passes_low_blocks_high() {
        let mut f = Svf::new(SvfMode::Lp);
        f.recalc(1000.0, 0.707, SAMPLE_RATE);
        let low = gain_at(100.0, |x| f.tick(x));
        let mut f = Svf::new(SvfMode::Lp);
        f.recalc(1000.0, 0.707, SAMPLE_RATE);
        let high = gain_at(10_000.0, |x| f.tick(x));
        assert!(low > 0.9, "low attenuated: {low}");
        assert!(high < 0.25, "high passed: {high}");
    }

    #[test]
    fn highpass_does_the_opposite() {
        let mut f = Svf::new(SvfMode::Hp);
        f.recalc(1000.0, 0.707, SAMPLE_RATE);
        let low = gain_at(100.0, |x| f.tick(x));
        let mut f = Svf::new(SvfMode::Hp);
        f.recalc(1000.0, 0.707, SAMPLE_RATE);
        let high = gain_at(10_000.0, |x| f.tick(x));
        assert!(low < 0.25, "lp leaked: {low}");
        assert!(high > 0.9, "hp attenuated: {high}");
    }

    #[test]
    fn bandpass_peaks_near_cutoff() {
        let mut f = Svf::new(SvfMode::Bp);
        f.recalc(1000.0, 5.0, SAMPLE_RATE);
        let at = gain_at(1000.0, |x| f.tick(x));
        let mut f = Svf::new(SvfMode::Bp);
        f.recalc(1000.0, 5.0, SAMPLE_RATE);
        let away = gain_at(100.0, |x| f.tick(x));
        assert!(at > away * 4.0, "bp not peaked enough: {at} vs {away}");
    }

    #[test]
    fn off_bypasses_unchanged() {
        let mut f = Svf::new(SvfMode::Off);
        f.recalc(1000.0, 0.707, SAMPLE_RATE);
        for i in -50..=50 {
            let x = i as f32 * 0.1;
            approx::assert_abs_diff_eq!(f.tick(x), x, epsilon = 1e-7);
        }
    }

    #[test]
    fn bounded_for_sustained_drive() {
        let mut f = Svf::new(SvfMode::Lp);
        f.recalc(2000.0, 8.0, SAMPLE_RATE);
        for _ in 0..100_000 {
            let s = f.tick(1.0);
            assert!(s.is_finite(), "SVF blew up");
            assert!(s.abs() < 10.0, "SVF runaway: {s}");
        }
    }

    /// The regression this clamp exists for. At the default strip resonance
    /// (`Q = 0.695`) the stability ceiling is ~6.4 kHz, and a cutoff macro at
    /// 1.0 is 20 kHz — so `recalc` used to push the integrators outside the
    /// unit circle and the filter diverged to `NaN`. It reached the master
    /// clipper as a non-finite sample, which is a much harder failure to
    /// trace back to a filter than loud wrong audio would be.
    #[test]
    fn wide_open_cutoff_stays_bounded_at_default_resonance() {
        for mode in [SvfMode::Lp, SvfMode::Bp, SvfMode::Hp, SvfMode::Notch] {
            let mut f = Svf::new(mode);
            // 0.695 is what `STRIP_RESO_INFO`'s default 0.01 maps to.
            f.recalc(20_000.0, 0.695, SAMPLE_RATE);
            for _ in 0..20_000 {
                let s = f.tick(1.0);
                assert!(s.is_finite(), "{mode:?} produced {s} at 20 kHz");
            }
        }
    }

    /// Same ceiling via the per-sample path. `set_cutoff` has to clamp against
    /// the *cached* `k`, or a swept cutoff (mi-drum's Ripples, SweepFX's LFO)
    /// diverges even though `recalc` is safe.
    #[test]
    fn per_sample_cutoff_sweep_stays_bounded() {
        let mut f = Svf::new(SvfMode::Lp);
        f.recalc(1000.0, 0.695, SAMPLE_RATE);
        for i in 0..20_000 {
            // Sweep up to and past the ceiling and back down.
            let t = (i % 4000) as f32 / 4000.0;
            let hz = 20_000.0 * t;
            f.set_cutoff(hz, SAMPLE_RATE);
            let s = f.tick(1.0);
            assert!(s.is_finite(), "sweep diverged at {hz} Hz: {s}");
        }
    }

    /// The ceiling has to be where the maths says, not merely "high enough" —
    /// a clamp that is too tight would quietly cap the filter's usable range.
    #[test]
    fn ceiling_matches_the_analytic_bound() {
        // Q = 0.695, k = 1/0.695: c1² + 2·c1·k = STABILITY_MARGIN at the ceiling.
        let q = 0.695f32;
        let hz = stability_ceiling_hz(q, SAMPLE_RATE);
        let k = 1.0 / q;
        let c1 = 2.0 * libm::sinf(core::f32::consts::PI * hz / SAMPLE_RATE);
        let s = c1 * c1 + 2.0 * c1 * k;
        approx::assert_abs_diff_eq!(s, STABILITY_MARGIN, epsilon = 1e-3);

        // The true boundary — where the pole leaves the unit circle — is still
        // above the ceiling, which is what makes the clamp load-bearing rather
        // than decorative. Without the margin these two are the same number.
        let c1_true = -k + libm::sqrtf(k * k + 4.0);
        let boundary_hz = libm::asinf(c1_true * 0.5) * SAMPLE_RATE / core::f32::consts::PI;
        assert!(
            hz < boundary_hz - 100.0,
            "ceiling {hz} Hz is not meaningfully inside the boundary {boundary_hz} Hz"
        );
    }

    /// The ceiling must sit *inside* the boundary, not on it.
    ///
    /// Clamping to exactly `c1² + 2·c1·k = 4` looks correct and is not:
    /// `c1` is computed through `sinf` and then multiplied and accumulated in
    /// f32, so the realised coefficient lands a fraction of an ulp either side
    /// of the target, and a filter sitting on the boundary self-oscillates.
    /// Measured through the engine: a kick that is silent above 4 kHz at the
    /// source came out of the filter with 48% of its energy above 4 kHz — a
    /// marginally-stable SVF screaming, which is exactly the "gritty" this
    /// margin exists to remove.
    #[test]
    fn ceiling_is_inside_the_boundary_not_on_it() {
        for q in [0.5f32, 0.695, 1.0, 2.0, 4.0] {
            let k = (1.0 / q.max(0.5)).min(2.0);
            let hz = stability_ceiling_hz(q, SAMPLE_RATE);
            let c1 = 2.0 * libm::sinf(core::f32::consts::PI * hz / SAMPLE_RATE);
            let realised = c1 * c1 + 2.0 * c1 * k;
            assert!(
                realised < 4.0,
                "Q={q}: ceiling sits on or outside the boundary ({realised})"
            );
            // And with real headroom, not a rounding error's worth.
            assert!(
                realised <= 3.05,
                "Q={q}: only {realised} — not enough margin to survive f32 rounding"
            );
        }
    }

    /// The regression this margin exists for, measured the way it was found:
    /// a low-frequency-only input through a wide-open filter must not come out
    /// with energy where there was none.
    #[test]
    fn wide_open_filter_does_not_invent_high_frequencies() {
        for q in [0.5f32, 0.695, 1.0, 2.0] {
            let mut f = Svf::new(SvfMode::Lp);
            f.recalc(20_000.0, q, SAMPLE_RATE);
            // Low sine only — nothing above 1 kHz goes in.
            let mut last = 0.0f32;
            let mut crossings_at_1k = 0u32;
            for i in 0..8 * SAMPLE_RATE as usize {
                let t = i as f32 * core::f32::consts::PI * 2.0 * 200.0 / SAMPLE_RATE;
                let s = libm::sinf(t) * 0.5;
                let y = f.tick(s);
                assert!(y.is_finite(), "Q={q}: diverged to {y}");
                // Count zero crossings: a self-oscillating filter adds a high
                // partial, which shows up as far more crossings than a 200 Hz
                // sine can produce in the same span.
                if (last < 0.0) != (y < 0.0) {
                    crossings_at_1k += 1;
                }
                last = y;
            }
            // 200 Hz over 8 s = 3200 crossings, give or take a little slew.
            assert!(
                crossings_at_1k < 3400,
                "Q={q}: filter invented {crossings_at_1k} crossings from a 200 Hz sine \
                 (expected ~3200) — it is self-oscillating"
            );
        }
    }

    /// Lowering Q must buy a higher ceiling, or the resonance and cutoff
    /// knobs would be stuck fighting each other.
    #[test]
    fn lower_q_raises_the_ceiling() {
        let tight = stability_ceiling_hz(0.5, SAMPLE_RATE);
        let butterworth = stability_ceiling_hz(0.707, SAMPLE_RATE);
        let resonant = stability_ceiling_hz(4.0, SAMPLE_RATE);
        assert!(tight < butterworth, "Q=0.5 should have the lowest ceiling");
        assert!(
            resonant > butterworth,
            "high Q should tolerate a higher cutoff than Butterworth"
        );
        // Every ceiling is under Nyquist.
        for q in [0.5f32, 0.707, 1.0, 4.0, 20.0] {
            assert!(stability_ceiling_hz(q, SAMPLE_RATE) <= SAMPLE_RATE * 0.5);
        }
    }

    /// The per-sample path clamps against a *cached* bound, so the cache has
    /// to be refreshed when Q changes. A `recalc` that updated `k` and left
    /// `c1_max` stale would keep sweeping against the old ceiling, which is
    /// the failure this pins — and it is invisible in `recalc`'s own output,
    /// because `recalc` computes the bound it then uses.
    #[test]
    fn a_q_change_moves_the_ceiling_the_swept_cutoff_clamps_to() {
        // Q = 0.5 has the lowest ceiling, Q = 4.0 a higher one.
        let saturated_c1 = |q: f32| {
            let mut f = Svf::new(SvfMode::Lp);
            f.recalc(1_000.0, q, SAMPLE_RATE);
            // Well past any ceiling, so the clamp is what decides `c1`.
            f.set_cutoff(20_000.0, SAMPLE_RATE);
            f.c1
        };
        assert!(
            saturated_c1(4.0) > saturated_c1(0.5),
            "the swept cutoff is clamping against a stale ceiling"
        );

        // And the clamped coefficient is the bound itself, not an approach to
        // it: the whole point of clamping in the coefficient domain is that
        // the realised `c1` lands exactly on `c1_max`.
        for q in [0.5f32, 0.707, 4.0] {
            let mut f = Svf::new(SvfMode::Lp);
            f.recalc(1_000.0, q, SAMPLE_RATE);
            let bound = f.c1_max;
            f.set_cutoff(20_000.0, SAMPLE_RATE);
            approx::assert_abs_diff_eq!(f.c1, bound, epsilon = 1e-6);
            // Which is to say: exactly on the margin that was solved for.
            let realised = f.c1 * f.c1 + 2.0 * f.c1 * f.k;
            approx::assert_abs_diff_eq!(realised, STABILITY_MARGIN, epsilon = 1e-4);
        }
    }

    /// The clamp must not cap a cutoff that is already legal.
    #[test]
    fn cutoff_below_the_ceiling_is_passed_through() {
        for hz in [80.0f32, 500.0, 2_000.0, 5_000.0] {
            let mut a = Svf::new(SvfMode::Bp);
            let mut b = Svf::new(SvfMode::Bp);
            a.recalc(hz, 4.0, SAMPLE_RATE);
            // Reference: the old formula, for a cutoff known to be in range.
            let clamped = hz.clamp(1.0, stability_ceiling_hz(4.0, SAMPLE_RATE));
            b.recalc(clamped, 4.0, SAMPLE_RATE);
            approx::assert_abs_diff_eq!(a.c1, b.c1, epsilon = 1e-6);
        }
    }

    /// `set_cutoff` must reproduce `recalc`'s integrator coefficient (the
    /// only term the per-sample path changes) closely enough that the two
    /// filters cannot be told apart on a sustained input.
    #[test]
    fn set_cutoff_matches_recalc_coefficient() {
        for hz in [80.0f32, 500.0, 3_000.0, 8_000.0, 12_000.0] {
            let mut a = Svf::new(SvfMode::Bp);
            let mut b = Svf::new(SvfMode::Bp);
            a.recalc(hz, 4.0, SAMPLE_RATE);
            b.recalc(hz, 4.0, SAMPLE_RATE);
            b.set_cutoff(hz, SAMPLE_RATE);
            assert!(
                (a.c1 - b.c1).abs() < 1e-5,
                "c1 mismatch at {hz} Hz: {} vs {}",
                a.c1,
                b.c1
            );
        }
    }

    #[test]
    fn set_cutoff_close_to_recalc_across_range() {
        let mut a = Svf::new(SvfMode::Bp);
        let mut b = Svf::new(SvfMode::Bp);
        let mut max_err = 0.0f32;
        for hz in (1..24_000).step_by(97) {
            a.recalc(hz as f32, 3.0, SAMPLE_RATE);
            b.recalc(hz as f32, 3.0, SAMPLE_RATE);
            b.set_cutoff(hz as f32, SAMPLE_RATE);
            max_err = max_err.max((a.c1 - b.c1).abs());
        }
        assert!(max_err < 1e-5, "max c1 error: {max_err}");
    }
}
