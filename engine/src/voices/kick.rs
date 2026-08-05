//! Kick drum: pitch-swept sine with drive.
//!
//! The classic analogue-style topology. A sine whose frequency drops rapidly
//! from a click-like starting pitch down to the fundamental, multiplied by an
//! amplitude envelope, then driven into saturation for weight.
//!
//! Almost all of the character lives in the relationship between the two
//! envelope times. A pitch decay much shorter than the amp decay gives a
//! tight, clicky attack over a sustained body; bring them closer together and
//! it turns into a tom.

use crate::dsp::{decay_coeff, fast, DecayEnv, SineOsc};
use crate::SAMPLE_RATE;

/// Kick parameters.
#[derive(Clone, Copy)]
#[cfg_attr(feature = "debug-params", derive(Debug))]
pub struct KickParams {
    /// Pitch at the moment of the transient, Hz.
    pub start_hz: f32,
    /// Pitch the sweep settles to, Hz.
    pub end_hz: f32,
    /// Time for the pitch sweep to complete, seconds. Short is clicky.
    pub pitch_decay_s: f32,
    /// Amplitude decay, seconds.
    pub decay_s: f32,
    /// Saturation amount. 1.0 is clean, higher drives harder.
    pub drive: f32,
    /// Output level, linear.
    pub level: f32,
}

impl Default for KickParams {
    fn default() -> Self {
        Self {
            start_hz: 220.0,
            end_hz: 48.0,
            pitch_decay_s: 0.035,
            decay_s: 0.42,
            drive: 1.8,
            level: 0.9,
        }
    }
}

/// Kick drum voice.
pub struct Kick {
    osc: SineOsc,
    amp_env: DecayEnv,
    pitch_env: DecayEnv,
    // Cached from params so `tick` never reads the param struct.
    start_hz: f32,
    end_hz: f32,
    sweep_range: f32,
    drive: f32,
    level: f32,
}

impl Kick {
    /// Build a kick from parameters.
    pub fn new(p: &KickParams) -> Self {
        let mut k = Self {
            osc: SineOsc::new(),
            amp_env: DecayEnv::new(0.0),
            pitch_env: DecayEnv::new(0.0),
            start_hz: p.start_hz,
            end_hz: p.end_hz,
            sweep_range: p.start_hz - p.end_hz,
            drive: p.drive,
            level: p.level,
        };
        k.set_params(p);
        k
    }

    /// Recompute coefficients. Setup-time, not real-time.
    pub fn set_params(&mut self, p: &KickParams) {
        self.amp_env
            .set_coeff(decay_coeff(p.decay_s, SAMPLE_RATE));
        self.pitch_env
            .set_coeff(decay_coeff(p.pitch_decay_s, SAMPLE_RATE));
        self.start_hz = p.start_hz;
        self.end_hz = p.end_hz;
        self.sweep_range = p.start_hz - p.end_hz;
        self.drive = p.drive;
        self.level = p.level;
    }

    /// Strike the drum.
    pub fn trigger(&mut self, velocity: f32) {
        self.amp_env.trigger(velocity);
        self.pitch_env.trigger(1.0);
        // Consistent phase on every hit, so every transient is identical.
        self.osc.reset_phase();
    }

    /// Silence.
    pub fn reset(&mut self) {
        self.amp_env.reset();
        self.pitch_env.reset();
        self.osc.reset_phase();
    }

    /// Still sounding?
    pub fn is_active(&self) -> bool {
        self.amp_env.is_active()
    }

    /// One sample.
    #[inline(always)]
    pub fn tick(&mut self) -> f32 {
        let amp = self.amp_env.tick();
        if amp == 0.0 {
            // Exact comparison is safe because DecayEnv flushes to true zero.
            return 0.0;
        }

        // Linear sweep on a decaying envelope gives an exponential-feeling
        // glide without a per-sample powf.
        let sweep = self.pitch_env.tick();
        self.osc.set_freq(self.end_hz + self.sweep_range * sweep);

        let raw = self.osc.tick() * amp;
        fast::soft_clip(raw * self.drive) * self.level
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn silent_until_struck() {
        let mut k = Kick::new(&KickParams::default());
        for _ in 0..1000 {
            assert_eq!(k.tick(), 0.0);
        }
    }

    #[test]
    fn velocity_scales_output() {
        let p = KickParams::default();

        let peak_at = |vel: f32| {
            let mut k = Kick::new(&p);
            k.trigger(vel);
            let mut peak = 0.0f32;
            for _ in 0..4800 {
                peak = peak.max(libm::fabsf(k.tick()));
            }
            peak
        };

        let quiet = peak_at(0.25);
        let loud = peak_at(1.0);
        assert!(loud > quiet, "velocity had no effect: {quiet} vs {loud}");
    }

    #[test]
    fn pitch_falls_over_time() {
        // Zero crossings in the first 10ms should outnumber those in a
        // 10ms window taken later, since the sweep is downward.
        let mut k = Kick::new(&KickParams::default());
        k.trigger(1.0);

        let window = (0.01 * SAMPLE_RATE) as usize;
        let count_crossings = |k: &mut Kick, n: usize| {
            let mut prev = k.tick();
            let mut c = 0;
            for _ in 1..n {
                let s = k.tick();
                if (prev < 0.0) != (s < 0.0) {
                    c += 1;
                }
                prev = s;
            }
            c
        };

        let early = count_crossings(&mut k, window);
        // Skip ahead.
        for _ in 0..window * 4 {
            k.tick();
        }
        let late = count_crossings(&mut k, window);

        assert!(early > late, "pitch did not fall: {early} then {late}");
    }
}
