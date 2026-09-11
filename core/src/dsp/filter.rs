//! Filters.
//!
//! One-pole only. For percussion these are usually enough, and they cost one
//! multiply-add per sample against the four-plus of a biquad. Reach for
//! something steeper when you can demonstrate you need it.

use crate::DENORMAL_FLOOR;

/// Compute the one-pole coefficient for a given cutoff.
///
/// Setup-time only — calls `expf`. The result is the feedback coefficient for
/// both [`OnePoleLp`] and [`OnePoleHp`].
#[inline]
pub fn cutoff_coeff(hz: f32, sample_rate: f32) -> f32 {
    let f = hz.clamp(1.0, sample_rate * 0.49);
    libm::expf(-core::f32::consts::TAU * f / sample_rate)
}

/// One-pole lowpass.
#[derive(Clone, Copy)]
pub struct OnePoleLp {
    z: f32,
    coeff: f32,
}

impl OnePoleLp {
    /// Create with the given feedback coefficient.
    #[inline]
    pub const fn new(coeff: f32) -> Self {
        Self { z: 0.0, coeff }
    }

    /// Update the cutoff coefficient.
    #[inline]
    pub fn set_coeff(&mut self, coeff: f32) {
        self.coeff = coeff;
    }

    /// Clear the filter state.
    #[inline]
    pub fn reset(&mut self) {
        self.z = 0.0;
    }

    /// Process one sample.
    #[inline(always)]
    pub fn tick(&mut self, x: f32) -> f32 {
        self.z = x + self.coeff * (self.z - x);
        if libm::fabsf(self.z) < DENORMAL_FLOOR {
            self.z = 0.0;
        }
        self.z
    }
}

/// One-pole highpass, implemented as input minus its lowpass.
#[derive(Clone, Copy)]
pub struct OnePoleHp {
    lp: OnePoleLp,
}

impl OnePoleHp {
    /// Create with the given feedback coefficient.
    #[inline]
    pub const fn new(coeff: f32) -> Self {
        Self {
            lp: OnePoleLp::new(coeff),
        }
    }

    /// Update the cutoff coefficient.
    #[inline]
    pub fn set_coeff(&mut self, coeff: f32) {
        self.lp.set_coeff(coeff);
    }

    /// Clear the filter state.
    #[inline]
    pub fn reset(&mut self) {
        self.lp.reset();
    }

    /// Process one sample.
    #[inline(always)]
    pub fn tick(&mut self, x: f32) -> f32 {
        x - self.lp.tick(x)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::SAMPLE_RATE;

    /// Measure steady-state gain at a frequency by driving a sine through.
    fn gain_at(hz: f32, mut f: impl FnMut(f32) -> f32) -> f32 {
        let inc = hz / SAMPLE_RATE;
        let mut phase = 0.0f32;
        let mut peak = 0.0f32;

        // Settle, then measure.
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
        let c = cutoff_coeff(1000.0, SAMPLE_RATE);
        let mut lp = OnePoleLp::new(c);
        let low = gain_at(100.0, |x| lp.tick(x));

        let mut lp = OnePoleLp::new(c);
        let high = gain_at(10_000.0, |x| lp.tick(x));

        assert!(low > 0.9, "low frequency attenuated: {low}");
        assert!(high < 0.2, "high frequency passed: {high}");
    }

    #[test]
    fn highpass_does_the_opposite() {
        let c = cutoff_coeff(1000.0, SAMPLE_RATE);
        let mut hp = OnePoleHp::new(c);
        let low = gain_at(100.0, |x| hp.tick(x));

        let mut hp = OnePoleHp::new(c);
        let high = gain_at(10_000.0, |x| hp.tick(x));

        assert!(low < 0.2, "low frequency passed: {low}");
        assert!(high > 0.9, "high frequency attenuated: {high}");
    }
}
