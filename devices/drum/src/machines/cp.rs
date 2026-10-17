//! CP: clap — multi-burst noise envelope layered with an FM body.
//!
//! A clap is not one hit; it's three or four re-triggered noise bursts
//! crammed into the first ~25ms (the "crunch"), then a longer tail that
//! blends into the body. The Syntakt CP VINTAGE Balances noise with an
//! FM "body"; we do the same — a short FM tone under the noise gives
//! weight to the crunch without a separate body machine.
//!
//! # The burst envelope
//!
//! Rather than running four separate envelopes, a single envelope reads its
//! amplitude from a precomputed burst table — a sequence of `(hold_samples,
//! gap_samples)` pairs whose durations sum to the ~25ms crunch region, after
//! which the envelope falls through to an exponential decay tail. One PRNG,
//! one filter, one table walk. The classic clap shape for the cost of a hat.
//!
//! # Params
//!
//! What each control does and the range it spans. NRPN addresses are in
//! the generated cheat-sheet (`cargo run -p render -- cheatsheet`) and are
//! not repeated here: a second copy drifts, as the `CC 20 + slot` column
//! that used to be in this table did.
//!
//! | name  | range        | notes                                            |
//! |-------|--------------|--------------------------------------------------|
//! | TUNE  | 150..400 Hz  | body pitch                                       |
//! | RATIO | 1.0..1.5     | body osc ratio                                   |
//! | MACH  | 0..1         | machine selector (quantised over MachineId::ALL) |
//! | HPF   | 800..4000 Hz | noise highpass colour                            |
//! | LPF   | 4..12 kHz    | noise lowpass top                                |
//! | LEVEL | 0..1         | per-machine output level                         |
//! | BDEC  | 50..250 ms   | body decay                                       |
//! | NDEC  | 100..600 ms  | noise (burst + tail) decay                       |
//! | BAL   | 0..1         | noise↔body balance                               |
//!
//! All other slots are RESV (default 0.0) and ignored.

use crate::dsp::filter::cutoff_coeff;
use crate::dsp::{decay_coeff, fast, DecayEnv, Noise, OnePoleHp, OnePoleLp, SineOsc};
use crate::machines::{
    NUM_PARAMS, SLOT_FILT_0, SLOT_FILT_1, SLOT_LEVEL, SLOT_MACH_0, SLOT_MACH_1, SLOT_MACH_5,
    SLOT_MACH_6, SLOT_MACH_7,
};
use crate::SAMPLE_RATE;

/// Number of initial bursts in the crunch region. Three is the classic 808
/// clap; four reads slightly fuller.
const BURSTS: usize = 3;

/// Burst pattern: (on_samples, off_samples) for each burst. The last burst's
/// "off" tail connects into the decay, so it's effectively longer. Values
/// tuned for ~25ms total crunch at 48kHz.
const BURST_ON: u16 = 110; // ~2.3ms ON per burst
const BURST_OFF: u16 = 280; // ~5.8ms gap between bursts

/// CP machine.
pub struct Cp {
    noise: Noise,
    noise_env: BurstEnv,
    body: SineOsc,
    body_env: DecayEnv,
    hp: OnePoleHp,
    lp: OnePoleLp,
    body_hz: f32,
    body_ratio: f32,
    /// Semitone multiplier applied to every frequency. Set by [`retune`],
    /// re-applied by `set_params` so a later param recompute keeps the note.
    freq_scale: f32,
    body_gain: f32,
    noise_gain: f32,
    level: f32,
}

impl Cp {
    /// Build the machine with the given param values applied.
    pub fn new(params: &[f32; NUM_PARAMS]) -> Self {
        let mut s = Self {
            noise: Noise::new(0x4D3A_91C7),
            noise_env: BurstEnv::new(),
            body: SineOsc::new(),
            body_env: DecayEnv::new(0.0),
            hp: OnePoleHp::new(0.0),
            lp: OnePoleLp::new(0.0),
            body_hz: 0.0,
            body_ratio: 0.0,
            freq_scale: 1.0,
            body_gain: 0.0,
            noise_gain: 0.0,
            level: 0.0,
        };
        s.set_params(params);
        s
    }

    /// Recompute coefficients from params. Setup rate.
    pub fn set_params(&mut self, params: &[f32; NUM_PARAMS]) {
        let body_hz = 150.0 + 250.0 * params[SLOT_MACH_0]; // TUNE 150..400 Hz
        let body_ratio = 1.0 + 0.5 * params[SLOT_MACH_1]; // RATIO 1.0..1.5
        let body_decay_s = 0.05 + 0.2 * params[SLOT_MACH_5]; // BDEC 50..250 ms
        let noise_decay_s = 0.1 + 0.5 * params[SLOT_MACH_6]; // NDEC 100..600 ms
        let hp_hz = 800.0 + 3200.0 * params[SLOT_FILT_0]; // HPF 800..4000 Hz
        let lp_hz = 4000.0 + 8000.0 * params[SLOT_FILT_1]; // LPF 4..12 kHz
        let bal = params[SLOT_MACH_7].clamp(0.0, 1.0); // BAL noise↔body
        let level = params[SLOT_LEVEL]; // LEVEL 0..1

        self.body_hz = body_hz * self.freq_scale;
        self.body_ratio = body_ratio;
        self.body.set_freq(self.body_hz * body_ratio);
        self.body_env
            .set_coeff(decay_coeff(body_decay_s, SAMPLE_RATE));
        self.noise_env
            .set_tail(decay_coeff(noise_decay_s, SAMPLE_RATE));
        self.hp.set_coeff(cutoff_coeff(hp_hz, SAMPLE_RATE));
        self.lp.set_coeff(cutoff_coeff(lp_hz, SAMPLE_RATE));

        self.body_gain = 1.0 - bal;
        self.noise_gain = bal;
        self.level = level;
    }

    /// Transpose by `semis` semitones relative to the param pitch.
    ///
    /// Scales the FM body; the noise crunch is pitchless and untouched.
    /// Absolute, not incremental.
    pub fn retune(&mut self, semis: f32) {
        let new_scale = fast::semitone_ratio(semis);
        let ratio = new_scale / self.freq_scale;
        self.freq_scale = new_scale;
        self.body_hz *= ratio;
        self.body.set_freq(self.body_hz * self.body_ratio);
    }

    /// Hit it.
    pub fn trigger(&mut self, velocity: f32) {
        self.noise_env.trigger(velocity);
        self.body_env.trigger(velocity);
        self.body.reset_phase();
    }

    /// Silence.
    pub fn reset(&mut self) {
        self.noise_env.reset();
        self.body_env.reset();
        self.hp.reset();
        self.lp.reset();
    }

    /// Still sounding?
    pub fn is_active(&self) -> bool {
        self.noise_env.is_active() || self.body_env.is_active()
    }

    /// One sample.
    #[inline(always)]
    pub fn tick(&mut self) -> f32 {
        let noise_amp = self.noise_env.tick();
        let body_amp = self.body_env.tick();

        if noise_amp == 0.0 && body_amp == 0.0 {
            return 0.0;
        }

        let rattle = if noise_amp != 0.0 {
            self.lp.tick(self.hp.tick(self.noise.tick())) * noise_amp
        } else {
            0.0
        };
        let body = if body_amp != 0.0 {
            self.body.tick() * body_amp
        } else {
            0.0
        };

        let mixed = rattle * self.noise_gain + body * self.body_gain;
        fast::soft_clip(mixed) * self.level
    }
}

/// A burst-then-decay envelope for the clap crunch.
///
/// Walks `BURSTS` on/off segments (the crunch), then falls through to an
/// exponential decay tail. One-shot, velocity-scaled, flushes to zero.
struct BurstEnv {
    /// Per-sample amplitude multiplier during the tail.
    tail_coeff: f32,
    /// Current value.
    value: f32,
    /// What phase of the envelope we're in.
    stage: Stage,
    /// Sample counter inside the current burst segment.
    seg_left: u16,
    /// Which burst (0..BURSTS) we're in, or BURSTS once we've moved to tail.
    burst_idx: usize,
    /// Triggered peak.
    peak: f32,
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Stage {
    Idle,
    BurstOn,
    BurstOff,
    Tail,
}

impl BurstEnv {
    const fn new() -> Self {
        Self {
            tail_coeff: 0.0,
            value: 0.0,
            stage: Stage::Idle,
            seg_left: 0,
            burst_idx: 0,
            peak: 1.0,
        }
    }

    fn set_tail(&mut self, coeff: f32) {
        self.tail_coeff = coeff;
    }

    fn trigger(&mut self, velocity: f32) {
        self.peak = velocity.clamp(0.0, 1.0);
        self.value = self.peak;
        self.stage = Stage::BurstOn;
        self.seg_left = BURST_ON;
        self.burst_idx = 0;
    }

    fn reset(&mut self) {
        self.stage = Stage::Idle;
        self.value = 0.0;
    }

    fn is_active(&self) -> bool {
        self.stage != Stage::Idle
    }

    /// Advance one sample and return the level *before* the step.
    #[inline(always)]
    fn tick(&mut self) -> f32 {
        let out = self.value;
        match self.stage {
            Stage::Idle => {}
            Stage::BurstOn => {
                self.seg_left = self.seg_left.saturating_sub(1);
                if self.seg_left == 0 {
                    // Last burst goes straight to the tail (no gap).
                    if self.burst_idx + 1 >= BURSTS {
                        self.stage = Stage::Tail;
                    } else {
                        self.stage = Stage::BurstOff;
                        self.seg_left = BURST_OFF;
                        self.value = 0.0;
                    }
                }
            }
            Stage::BurstOff => {
                self.seg_left = self.seg_left.saturating_sub(1);
                if self.seg_left == 0 {
                    self.burst_idx += 1;
                    self.stage = Stage::BurstOn;
                    self.seg_left = BURST_ON;
                    self.value = self.peak;
                }
            }
            Stage::Tail => {
                self.value *= self.tail_coeff;
                if self.value < crate::DENORMAL_FLOOR {
                    self.value = 0.0;
                    self.stage = Stage::Idle;
                }
            }
        }
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::machines::MachineId;

    fn peak_over(s: &mut Cp, n: usize) -> f32 {
        let mut peak = 0.0f32;
        for _ in 0..n {
            peak = peak.max(libm::fabsf(s.tick()));
        }
        peak
    }

    #[test]
    fn silent_until_struck() {
        let id = MachineId::Cp;
        let params = id.default_params();
        let mut s = Cp::new(&params);
        assert_eq!(peak_over(&mut s, 1000), 0.0);
    }

    #[test]
    fn produces_burst_then_tail() {
        let id = MachineId::Cp;
        let params = id.default_params();
        let mut s = Cp::new(&params);
        s.trigger(1.0);

        // Count "loud then quiet" transitions in the first 30ms — should see
        // several (one per burst in the crunch).
        let window = (0.03 * SAMPLE_RATE) as usize;
        let mut loud_samples = 0;
        let mut quiet_samples = 0;
        let mut transitions = 0;
        let mut was_loud = false;
        for _ in 0..window {
            let v = libm::fabsf(s.tick());
            let loud = v > 0.05;
            if loud {
                loud_samples += 1;
            } else {
                quiet_samples += 1;
            }
            if loud != was_loud {
                transitions += 1;
                was_loud = loud;
            }
        }
        // Both loud and quiet regions exist (it's bursty, not continuous).
        assert!(loud_samples > 50, "no loud bursts: {loud_samples}");
        assert!(
            quiet_samples > 50,
            "no gaps between bursts: {quiet_samples}"
        );
        // Multiple loud/quiet transitions confirm the burst pattern.
        assert!(
            transitions >= 3,
            "expected >=3 burst transitions: {transitions}"
        );
    }

    #[test]
    fn decays_to_silence() {
        let id = MachineId::Cp;
        let params = id.default_params();
        let mut s = Cp::new(&params);
        s.trigger(1.0);
        for _ in 0..(5.0 * SAMPLE_RATE) as usize {
            s.tick();
        }
        assert!(!s.is_active());
    }

    #[test]
    fn burst_env_shape() {
        let mut e = BurstEnv::new();
        e.set_tail(decay_coeff(0.2, SAMPLE_RATE));
        e.trigger(1.0);
        let mut loud = 0;
        let mut quiet = 0;
        for _ in 0..(0.05 * SAMPLE_RATE) as usize {
            let v = e.tick();
            if v > 0.5 {
                loud += 1;
            } else if v == 0.0 {
                quiet += 1;
            }
        }
        assert!(
            loud > 0 && quiet > 0,
            "burst env didn't gate: loud={loud} quiet={quiet}"
        );
    }
}
