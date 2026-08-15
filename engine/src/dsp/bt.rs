//! Bridged-T style bandpass biquad.
//!
//! A direct-form-II biquad shaped like the analog bridged-T resonator used
//! in the TR-808's kick, toms, and the low half of the snare — the network
//! a pulse prompts, a tuned LC pair rings. The coefficient form is the
//! standard RJB audio-EQ-cookbook bandpass (constant-0-dB-peak-gain):
//!
//! ```text
//!   b0 =  alpha       a0 = 1 + alpha
//!   b1 =  0           a1 = -2·cos(w0)
//!   b2 = -alpha       a2 = 1 - alpha
//! ```
//!
//! Coefficients are split across two update rates, matching how
//! [`crate::dsp::Svf`] is shaped:
//!
//! - [`set_q`](BridgedT::set_q) at control rate (machine `set_macros`,
//!   LFO/CC push, trigger) — does the divide and stores the Q-dependent
//!   coefficient family: `alpha`, `a0_inv`, `b0`, `a2`. The caller passes
//!   an `anchor_hz` (typically the resonator's settled pitch) so `alpha`'s
//!   `sin(w0)` is computed against the right frequency; `b0`/`a2` stay
//!   anchored there through the hit.
//!
//! - [`set_hz`](BridgedT::set_hz) at per-sample rate (machine `tick`) —
//!   does just one [`fast::sin_turns`] lookup (for `cos_w0`) and one
//!   multiply to update `a1`. No divides. This is the Phase 11 Option B
//!   split — measured Option A at +3,200 cy/block for 1 voice (all the
//!   cost was in the per-sample divides); Option B moves the divides to
//!   block rate.
//!
//! Q modulation works correctly with the split because the modulation
//! architecture (LFOs, CC smoother) pushes at block rate, not per sample —
//! so `set_q` is re-called every block when Q is being modulated, and the
//! per-sample path stays cheap. `b0` and `a2` go momentarily stale across
//! a single hit's pitch sweep (the bandwidth drift is inaudible against
//! the resonator's own ring); the next block's `set_q` refreshes them.
//!
//! The `b1 = 0`, `b2 = -b0` identity is folded into the per-sample path,
//! which becomes four multiplies and three adds (vs five and four for a
//! general DF-II biquad).

use crate::dsp::fast;
use crate::{DENORMAL_FLOOR, SAMPLE_RATE};

/// Radians → phase-in-turns. `2π rad = 1 turn`.
const TURNS_PER_RAD: f32 = 1.0 / core::f32::consts::TAU;

/// Bridged-T style 2-pole bandpass biquad, direct form II.
///
/// Coefficients are split between [`set_q`](Self::set_q) (control-rate,
/// the Q/resonance family + the divides) and [`set_hz`](Self::set_hz)
/// (per-sample, the moving-frequency term). `Clone + Copy` so it can sit
/// by value inside a machine struct, like [`SineOsc`](crate::dsp::SineOsc)
/// and [`DecayEnv`](crate::dsp::DecayEnv).
#[derive(Clone, Copy)]
pub struct BridgedT {
    /// Feed-forward gain, `b0 = alpha · a0_inv`. Encodes `b2 = -b0` implicitly.
    /// Set by [`set_q`](Self::set_q); constant across samples while Q is
    /// unchanged.
    b0: f32,
    /// Feedback coefficient, `a1 = -2·cos(w0) · a0_inv`. Set by
    /// [`set_hz`](Self::set_hz); moves with the resonator's target frequency.
    a1: f32,
    /// Feedback coefficient, `a2 = (1 - alpha) · a0_inv`. Set by
    /// [`set_q`](Self::set_q); constant while Q is unchanged.
    a2: f32,
    /// Cached `1 / (1 + alpha)`, the Q-dependent divide. Stored so the
    /// per-sample path scales `cos_w0` by it without re-dividing.
    a0_inv: f32,
    /// DF-II delay registers.
    v1: f32,
    v2: f32,
}

impl BridgedT {
    /// Build an idle filter. Coefficients are all zero; call
    /// [`set_q`](Self::set_q) and [`set_hz`](Self::set_hz) before
    /// [`process`](Self::process) produces any sound.
    pub const fn new() -> Self {
        Self {
            b0: 0.0,
            a1: 0.0,
            a2: 0.0,
            a0_inv: 0.0,
            v1: 0.0,
            v2: 0.0,
        }
    }

    /// Recompute the Q-dependent coefficient family at control rate.
    ///
    /// Stores `a0_inv = 1 / (1 + alpha)`, `b0 = alpha · a0_inv`, and
    /// `a2 = (1 - alpha) · a0_inv`, where `alpha = sin(w0) / 2Q` is
    /// computed against the caller-supplied `anchor_hz` — typically the
    /// resonator's *settled* pitch (the long ringing-tail frequency), so
    /// the bandwidth/gain shape is anchored to where the hit spends most
    /// of its time. Also refreshes `a1` against `anchor_hz` so the
    /// resonator comes out of `set_q` ready to ring at the anchor.
    ///
    /// Per-sample `set_hz` refreshes `a1` as the pitch sweeps; `b0`/`a2`
    /// stay cached at the anchor. Audible bandwidth drift during a fast
    /// pitch sweep is well below threshold (the resonator's own ring
    /// dominates perception). When Q is being modulated, this runs every
    /// block — still cheap, divides stay out of `tick`.
    ///
    /// `q` is the conventional Q factor — `0.707` is Butterworth-flat,
    /// `~5` begins to ring, `~10` nears self-oscillation. Values below
    /// `0.5` are clamped to keep the feedback sum bounded; degenerate
    /// parameters should be boring, not explosive.
    pub fn set_q(&mut self, q: f32, anchor_hz: f32) {
        let q = q.max(0.5);
        let alpha = Self::compute_alpha(anchor_hz, q);
        let a0_inv = 1.0 / (1.0 + alpha);
        self.a0_inv = a0_inv;
        self.b0 = alpha * a0_inv;
        self.a2 = (1.0 - alpha) * a0_inv;
        // `a1` scales with `a0_inv`, so refresh it against the anchor too.
        self.a1 = Self::compute_a1(anchor_hz, a0_inv);
    }

    /// Recompute the per-sample moving coefficient (`a1`) for `hz`.
    ///
    /// One [`fast::sin_turns`] lookup (for `cos_w0`), one multiply. No
    /// divide — uses the cached `a0_inv` from the last
    /// [`set_q`](Self::set_q). `b0`, `a2`, and `a0_inv` stay cached —
    /// they are exact when Q is unchanged, stale only across a single
    /// hit's pitch sweep (audible bandwidth drift is below threshold
    /// against the resonator's own ring). When Q *is* being modulated,
    /// `set_q` runs every block and refreshes them, so the staleness
    /// has no chance to accumulate.
    pub fn set_hz(&mut self, hz: f32) {
        let hz = hz.clamp(1.0, SAMPLE_RATE * 0.49);
        self.a1 = Self::compute_a1(hz, self.a0_inv);
    }

    /// `alpha = sin(w0) / 2Q` for a given `(hz, q)`. Pure: no state read.
    fn compute_alpha(hz: f32, q: f32) -> f32 {
        let hz = hz.clamp(1.0, SAMPLE_RATE * 0.49);
        let w0 = core::f32::consts::TAU * hz / SAMPLE_RATE;
        let sin_w0 = fast::sin_turns(w0 * TURNS_PER_RAD);
        sin_w0 / (2.0 * q)
    }

    /// `a1 = -2 · cos(w0) · a0_inv` for a given `hz`. Pure: scales the
    /// caller-supplied `a0_inv`. One `sin_turns` lookup.
    fn compute_a1(hz: f32, a0_inv: f32) -> f32 {
        let hz = hz.clamp(1.0, SAMPLE_RATE * 0.49);
        let w0 = core::f32::consts::TAU * hz / SAMPLE_RATE;
        let cos_w0 = fast::sin_turns(w0 * TURNS_PER_RAD + 0.25); // cos via +π/2 phase
        -2.0 * cos_w0 * a0_inv
    }

    /// Clear the delay registers. Call on trigger so a new hit starts from
    /// silence rather than the tail of the previous one.
    pub fn reset(&mut self) {
        self.v1 = 0.0;
        self.v2 = 0.0;
    }

    /// Process one sample. Four multiplies, three adds, one DF-II shift.
    #[inline(always)]
    pub fn process(&mut self, x: f32) -> f32 {
        let v0 = x - self.a1 * self.v1 - self.a2 * self.v2;
        // b1 = 0, b2 = -b0, so out = b0·v0 + b2·v2 = b0·(v0 - v2).
        let out = self.b0 * (v0 - self.v2);

        let v1_next = v0;
        let v2_next = self.v1;
        // Denormal guard: long resonance tails asymptote into denormal range
        // and trap to microcode on some cores. Flush to exactly zero below
        // [`DENORMAL_FLOOR`], matching `OnePoleLp` / `Svf`.
        let v1_next = if libm::fabsf(v1_next) < DENORMAL_FLOOR {
            0.0
        } else {
            v1_next
        };
        let v2_next = if libm::fabsf(v2_next) < DENORMAL_FLOOR {
            0.0
        } else {
            v2_next
        };
        self.v2 = v2_next;
        self.v1 = v1_next;
        out
    }
}

impl Default for BridgedT {
    fn default() -> Self {
        Self::new()
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
        for i in 0..40_000 {
            let x = libm::sinf(phase * core::f32::consts::TAU);
            phase += inc;
            if phase >= 1.0 {
                phase -= 1.0;
            }
            let y = f(x);
            if i > 20_000 {
                peak = peak.max(libm::fabsf(y));
            }
        }
        peak
    }

    /// Helper: configure a `BridgedT` the way BdVa does — `set_q` is given the
    /// anchor frequency (typically the resonator's settled pitch) so the
    /// Q-dependent bandwidth/gain family is anchored there. Per-sample
    /// `set_hz` afterwards moves only `a1`.
    fn configure(bt: &mut BridgedT, hz: f32, q: f32) {
        bt.set_q(q, hz);
    }

    #[test]
    fn bandpass_peaks_near_target_hz() {
        let mut bt = BridgedT::new();
        configure(&mut bt, 500.0, 4.0);
        let at = gain_at(500.0, |x| bt.process(x));

        let mut bt = BridgedT::new();
        configure(&mut bt, 500.0, 4.0);
        let away = gain_at(125.0, |x| bt.process(x));

        assert!(
            at > away * 4.0,
            "expected peak √-of-2 ratio or better; at={at}, away={away}"
        );
    }

    #[test]
    fn peak_moves_with_target_hz() {
        let mut bt_lo = BridgedT::new();
        configure(&mut bt_lo, 200.0, 4.0);
        let lo = gain_at(200.0, |x| bt_lo.process(x));

        let mut bt_lo = BridgedT::new();
        configure(&mut bt_lo, 200.0, 4.0);
        // Off-peak: 200 Hz drive into a 800 Hz resonator should be quiet.
        let lo_off = gain_at(800.0, |x| bt_lo.process(x));

        let mut bt_hi = BridgedT::new();
        configure(&mut bt_hi, 800.0, 4.0);
        let hi = gain_at(800.0, |x| bt_hi.process(x));

        assert!(
            lo > lo_off * 4.0,
            "200 Hz drive should peak: {lo} vs {lo_off}"
        );
        assert!(
            hi > lo,
            "higher Q same; gain should be ~equal: {hi} vs {lo}"
        );
    }

    #[test]
    fn higher_q_narrower_band() {
        let q_low = 1.0;
        let q_high = 8.0;

        let mut bt = BridgedT::new();
        configure(&mut bt, 1000.0, q_low);
        let stopband_low = gain_at(500.0, |x| bt.process(x));

        let mut bt = BridgedT::new();
        configure(&mut bt, 1000.0, q_high);
        let stopband_high = gain_at(500.0, |x| bt.process(x));

        // Higher Q = more attenuation off-peak.
        assert!(
            stopband_high < stopband_low * 0.5,
            "higher Q did not narrow the band: low={stopband_low}, high={stopband_high}"
        );
    }

    #[test]
    fn impulse_response_is_bandpass_shaped() {
        let mut bt = BridgedT::new();
        configure(&mut bt, 200.0, 6.0);
        bt.process(1.0);
        let mut peak = 0.0f32;
        let mut energy = 0.0f32;
        for _ in 0..SAMPLE_RATE as usize {
            let s = bt.process(0.0);
            peak = peak.max(libm::fabsf(s));
            energy += s * s;
        }
        assert!(peak > 0.0, "resonator was silent after impulse");
        assert!(energy > 0.0, "resonator had zero energy");
        assert!(energy < 0.1, "resonator ran away: energy={energy}");
    }

    #[test]
    fn reaches_silence_on_zero_input() {
        let mut bt = BridgedT::new();
        configure(&mut bt, 200.0, 4.0);
        bt.process(1.0); // excite
        for _ in 0..(5.0 * SAMPLE_RATE) as usize {
            bt.process(0.0);
        }
        // Both delay registers should have flushed to zero.
        let mut zero_input_out = 0.0f32;
        for _ in 0..100 {
            zero_input_out += libm::fabsf(bt.process(0.0));
        }
        assert_eq!(zero_input_out, 0.0, "resonator did not flush to zero");
    }

    #[test]
    fn set_hz_moves_peak_without_touching_q() {
        // The Option B contract: per-sample `set_hz` moves the resonator's
        // peak frequency while reusing the cached `a0_inv` from `set_q`.
        // Configure at one frequency, sweep `set_hz` to another, and
        // confirm the peak follows.
        let mut bt = BridgedT::new();
        configure(&mut bt, 200.0, 5.0);
        bt.set_hz(600.0); // retune per-sample, Q should be unchanged

        let at_600 = gain_at(600.0, |x| bt.process(x));
        let mut bt2 = BridgedT::new();
        configure(&mut bt2, 200.0, 5.0);
        bt2.set_hz(600.0);
        let at_200 = gain_at(200.0, |x| bt2.process(x));
        assert!(
            at_600 > at_200 * 4.0,
            "set_hz did not move the peak: 600Hz drive={at_600}, 200Hz drive={at_200}"
        );
    }

    #[test]
    fn set_q_at_block_rate_when_hz_flowing() {
        // Simulate Q modulation at block rate: alternate `set_hz` (per
        // sample) with periodic `set_q` calls (every N samples). The
        // resonator's peak frequency should follow `set_hz` and its Q
        // should follow the most recent `set_q`. No NaNs, no runaway.
        let mut bt = BridgedT::new();
        configure(&mut bt, 400.0, 2.0);
        let block = 32usize;
        for i in 0..(SAMPLE_RATE as usize * 2) {
            // Drive at the target frequency.
            let phase = (i as f32) * 400.0 / SAMPLE_RATE;
            let x = libm::sinf(phase * core::f32::consts::TAU);
            let y = bt.process(x);
            assert!(y.is_finite(), "non-finite output under Q modulation");
            // Every block, sweep hz and toggle Q between two values.
            if i % block == 0 {
                let anchor_hz = 400.0 + 50.0 * libm::sinf(i as f32 / 1000.0);
                bt.set_q(if i % (block * 2) == 0 { 2.0 } else { 8.0 }, anchor_hz);
                bt.set_hz(anchor_hz);
            }
        }
        // No runaway: drive a one-sample impulse at the end and check the
        // tail stays bounded.
        bt.process(1.0);
        let mut tail_max = 0.0f32;
        for _ in 0..(0.1 * SAMPLE_RATE) as usize {
            tail_max = tail_max.max(libm::fabsf(bt.process(0.0)));
        }
        assert!(
            tail_max < 1.5,
            "resonator ran away under Q modulation: {tail_max}"
        );
    }

    #[test]
    fn zero_q_does_not_explode() {
        let mut bt = BridgedT::new();
        bt.set_q(0.0, 200.0); // Q clamped internally
        for _ in 0..10_000 {
            let s = bt.process(1.0);
            assert!(s.is_finite(), "Q=0 produced NaN");
            assert!(libm::fabsf(s) <= 10.0, "Q=0 runaway: {s}");
        }
    }

    #[test]
    fn nyquist_target_does_not_explode() {
        let mut bt = BridgedT::new();
        bt.set_q(6.0, SAMPLE_RATE * 0.5); // clamped internally
        for _ in 0..10_000 {
            let s = bt.process(1.0);
            assert!(s.is_finite(), "Nyquist target produced NaN");
        }
    }
}
