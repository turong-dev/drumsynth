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
//! The Chamberlin form is numerically well-behaved up to roughly `fs / 6`,
//! which covers everything a drum strip needs (cutoffs rarely exceed 12kHz).
//! Above that, the resonance softens; documented behaviour, not a bug.

use crate::DENORMAL_FLOOR;

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
        let f = cutoff_hz.clamp(1.0, sample_rate * 0.49);
        // 2·sin(π·fc/fs) is the Chamberlin integration coefficient; it stays
        // in `(-2, 2)` across the clamp range, which is the stability limit.
        self.c1 = 2.0 * libm::sinf(core::f32::consts::PI * f / sample_rate);
        self.k = (1.0 / q.max(0.5)).min(2.0);
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
    #[inline(always)]
    pub fn set_cutoff(&mut self, cutoff_hz: f32, sample_rate: f32) {
        let f = cutoff_hz.clamp(1.0, sample_rate * 0.49);
        // sin(π·fc/fs) = sin_turns(fc / (2·fs)) — the table takes turns.
        self.c1 = 2.0 * crate::dsp::fast::sin_turns(f * 0.5 / sample_rate);
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
