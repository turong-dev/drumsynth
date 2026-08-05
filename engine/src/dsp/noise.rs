//! Noise.

/// White noise from a xorshift32 PRNG.
///
/// Three shifts, three XORs, one multiply. No division, no table, no
/// `rand` crate — which matters, because most of the RNG ecosystem assumes
/// `std` or drags in `getrandom`.
///
/// Deterministic by design. Given the same seed you get the same sequence on
/// your laptop and on the Teensy, which means a WAV rendered on the host is
/// bit-comparable against one captured from hardware. That is a genuinely
/// useful property when you are trying to work out whether a difference you
/// are hearing is the algorithm or the analogue stage.
#[derive(Clone, Copy)]
pub struct Noise {
    state: u32,
}

impl Noise {
    /// Create with a specific seed.
    ///
    /// Zero is remapped, since xorshift is absorbing at zero and would output
    /// silence forever.
    #[inline]
    pub const fn new(seed: u32) -> Self {
        Self {
            state: if seed == 0 { 0x2545_F491 } else { seed },
        }
    }

    /// Next sample, in `-1.0..1.0`.
    #[inline(always)]
    pub fn tick(&mut self) -> f32 {
        let mut x = self.state;
        x ^= x << 13;
        x ^= x >> 17;
        x ^= x << 5;
        self.state = x;

        // Take the top 24 bits into a float. 24 bits is the f32 mantissa, so
        // this maps evenly with no rounding artefacts, and the scale factor is
        // a power of two so the multiply is exact.
        ((x >> 8) as f32) * (2.0 / 16_777_216.0) - 1.0
    }
}

impl Default for Noise {
    fn default() -> Self {
        Self::new(0x1234_5678)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn stays_in_range() {
        let mut n = Noise::default();
        for _ in 0..1_000_000 {
            let s = n.tick();
            assert!((-1.0..1.0).contains(&s), "out of range: {s}");
        }
    }

    #[test]
    fn roughly_zero_mean() {
        let mut n = Noise::default();
        let count = 1_000_000;
        let mut sum = 0.0f64;
        for _ in 0..count {
            sum += n.tick() as f64;
        }
        let mean = sum / count as f64;
        assert!(mean.abs() < 0.01, "mean drifted to {mean}");
    }

    #[test]
    fn deterministic_across_runs() {
        let mut a = Noise::new(42);
        let mut b = Noise::new(42);
        for _ in 0..1000 {
            assert_eq!(a.tick(), b.tick());
        }
    }

    #[test]
    fn zero_seed_still_produces_output() {
        let mut n = Noise::new(0);
        assert_ne!(n.tick(), n.tick());
    }
}
