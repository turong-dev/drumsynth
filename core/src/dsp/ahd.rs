//! Attack-Hold-Decay envelope.
//!
//! Complements [`crate::dsp::DecayEnv`] (one-shot exponential). This one
//! has a linear attack stage and an explicit hold, which is what the
//! per-track *amp* envelope needs — a snare that wants a 2ms attack lip, a
//! tonal voice that wants to hold a long-decaying tail, neither of which a
//! bare decay can describe.
//!
//! The decay stage shares [`DecayEnv`]'s `-60dB-inside-N-seconds`
//! semantics. The attack stage is linear rather than exponential: percussive
//! attacks run a handful of samples, where a curve you cannot see is a
//! curve you cannot hear, and linear is one multiply per sample.
//!
//! Used by [`crate::Track`] for its amp envelope. The same struct serves
//! as the filter envelope, with the caller scaling its output by a bipolar
//! depth and adding it to the cutoff.
//!
//! # Holds that answer to a gate
//!
//! [`HoldMode::Timed`] is the original behaviour: the hold times out on its
//! own and the envelope falls into decay whether or not a key came up. That
//! is right for a one-shot gesture, and it is what every machine in the drum
//! catalogue did while there was no note-off path.
//!
//! [`HoldMode::Gated`] makes the hold wait for [`AhdEnv::release`] instead,
//! which is what turns the same envelope into a sustained voice: note-on
//! opens it, the level sits at peak for as long as the key is down, note-off
//! drops it into decay. A gated hold is not bounded by `hold_s`; a caller
//! that wants a backstop against a note-off that never arrives opts in with
//! [`AhdEnv::set_max_hold_s`]. That opt-in matters because `is_active` is the
//! engine's per-track cost gate — a voice left ringing keeps its track out of
//! the idle early-out for as long as it rings.

use crate::dsp::decay_coeff;
use crate::DENORMAL_FLOOR;
use crate::SAMPLE_RATE;

/// What ends an [`AhdEnv`]'s hold stage.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Default)]
pub enum HoldMode {
    /// The hold times out after `hold_s` and decay begins on its own. The
    /// pre-note-off behaviour: one trigger produces one fixed-length gesture.
    #[default]
    Timed,
    /// The hold lasts until [`AhdEnv::release`] is called, or until the
    /// watchdog set by [`AhdEnv::set_max_hold_s`] expires. `hold_s` is
    /// ignored in this mode — a gated note is bounded by the player holding
    /// the key, not by a preset gesture length.
    Gated,
}

/// Where the envelope is in its segments.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Stage {
    /// Not sounding; output is zero.
    Idle,
    /// Linear rise from zero to `peak` over `attack_samples`.
    Attack,
    /// Held at `peak` until the mode's terminating condition arrives.
    Hold,
    /// Exponential fall from `peak` toward zero.
    Decay,
}

/// Attack-Hold-Decay envelope, velocity-scaled.
///
/// Decay toward roughly -60dB after `decay_s`. The decay coefficient is
/// derived from `decay_s` exactly as in [`DecayEnv`]; the per-sample
/// multiply therefore behaves the same way and flushes to exact zero
/// below [`DENORMAL_FLOOR`].
#[derive(Clone, Copy)]
pub struct AhdEnv {
    stage: Stage,
    value: f32,
    peak: f32,
    attack_inc: f32,
    /// Length of the timed hold in samples, from `hold_s`. Only consulted
    /// under [`HoldMode::Timed`].
    hold_samples: usize,
    /// Watchdog length in samples, from [`Self::set_max_hold_s`]. Only
    /// consulted under [`HoldMode::Gated`], where 0 means no watchdog.
    max_hold_samples: usize,
    /// Countdown armed from whichever of the two applies, in samples.
    hold_left: usize,
    decay_coeff: f32,
    /// What ends the hold stage.
    hold_mode: HoldMode,
}

impl AhdEnv {
    /// Idle envelope with zero-length segments; configure via
    /// [`set_params`](Self::set_params).
    pub const fn new() -> Self {
        Self {
            stage: Stage::Idle,
            value: 0.0,
            peak: 1.0,
            attack_inc: 0.0,
            hold_samples: 0,
            max_hold_samples: 0,
            hold_left: 0,
            decay_coeff: 0.0,
            hold_mode: HoldMode::Timed,
        }
    }

    /// Choose what ends the hold stage. Setup rate.
    ///
    /// Takes effect on the next [`trigger`](Self::trigger); it does not
    /// retroactively re-arm a hold that is already running.
    pub fn set_hold_mode(&mut self, mode: HoldMode) {
        self.hold_mode = mode;
    }

    /// Current hold mode.
    pub fn hold_mode(&self) -> HoldMode {
        self.hold_mode
    }

    /// Watchdog length in seconds for a [`HoldMode::Gated`] hold.
    ///
    /// A gated hold waits for [`release`](Self::release), so a note-off that
    /// never arrives — a stuck key, a dropped cable, a drum-grid trigger that
    /// has no off state — would otherwise ring the voice forever and hold
    /// its track out of the engine's `is_active` cost gate indefinitely.
    /// Setting a non-zero value bounds that. Zero, the default, means no
    /// watchdog: the note lasts exactly as long as the key.
    ///
    /// Ignored under [`HoldMode::Timed`], where `hold_s` already bounds the
    /// gesture. Setup rate.
    pub fn set_max_hold_s(&mut self, max_hold_s: f32) {
        self.max_hold_samples = if max_hold_s > 0.0 {
            (max_hold_s * SAMPLE_RATE) as usize
        } else {
            0
        };
    }

    /// The watchdog length, in seconds. Zero means no watchdog.
    ///
    /// A note longer than this silently becomes a fixed-length gesture, so
    /// this is worth checking when choosing the value.
    pub fn max_hold_s(&self) -> f32 {
        self.max_hold_samples as f32 * crate::INV_SAMPLE_RATE
    }

    /// Configure timings in seconds (setup rate). `attack_s` and
    /// `hold_s` are linear; `decay_s` is the time-to-(-60dB) exponential
    /// half-life. Any non-positive time collapses to a skipped stage.
    ///
    /// `hold_s` is the hold under [`HoldMode::Timed`] and is ignored under
    /// [`HoldMode::Gated`], where [`set_max_hold_s`](Self::set_max_hold_s)
    /// governs instead.
    pub fn set_params(&mut self, attack_s: f32, hold_s: f32, decay_s: f32) {
        self.attack_inc = if attack_s > 0.0 {
            1.0 / (attack_s * SAMPLE_RATE)
        } else {
            // Zero attack: jump straight to peak — segment degenerates to a
            // single sample step.
            0.0
        };
        self.hold_samples = if hold_s > 0.0 {
            (hold_s * SAMPLE_RATE) as usize
        } else {
            0
        };
        self.decay_coeff = decay_coeff(decay_s, SAMPLE_RATE);
    }

    /// Direct decay-coefficient override, for callers that already have one
    /// (e.g. a machine reusing the same decay time as an internal envelope).
    pub fn set_decay_coeff(&mut self, coeff: f32) {
        self.decay_coeff = coeff;
    }

    /// Arm the hold stage for the current mode, or skip straight to decay.
    ///
    /// A timed hold needs a non-zero `hold_s` to exist at all. A gated hold
    /// always exists — `release` or the watchdog is what ends it — so a
    /// missing watchdog is "hold forever", not "no hold".
    fn enter_hold_or_decay(&mut self) -> Stage {
        match self.hold_mode {
            HoldMode::Timed if self.hold_samples == 0 => Stage::Decay,
            HoldMode::Timed => {
                self.hold_left = self.hold_samples;
                Stage::Hold
            }
            HoldMode::Gated => {
                // A gated hold with no watchdog must hold forever, but the
                // shared `hold_left == 0` exit below needs a countdown that
                // is not already finished. `usize::MAX` is the sentinel:
                // `saturating_sub` walks it down 2^64 samples, which at
                // 48 kHz is longer than the device has been switched on.
                self.hold_left = if self.max_hold_samples == 0 {
                    usize::MAX
                } else {
                    self.max_hold_samples
                };
                Stage::Hold
            }
        }
    }

    /// Begin a hit at `velocity` (0.0..=1.0). Scales the *envelope's peak*,
    /// so a softer hit is shorter and quieter rather than only quieter.
    pub fn trigger(&mut self, velocity: f32) {
        let v = velocity.clamp(0.0, 1.0);
        self.peak = v;
        if self.attack_inc > 0.0 {
            self.value = 0.0;
            self.stage = Stage::Attack;
        } else {
            // Degenerate attack: start holding at full velocity.
            self.value = v;
            self.stage = self.enter_hold_or_decay();
        }
    }

    /// Force to silence immediately.
    #[inline]
    pub fn reset(&mut self) {
        self.stage = Stage::Idle;
        self.value = 0.0;
    }

    /// Close the gate: give up any remaining hold and enter the decay stage
    /// from wherever the level currently is.
    ///
    /// This is what a note-off does. It is the counterpart to
    /// [`trigger`](Self::trigger) and is a no-op once the envelope is
    /// already decaying or idle, so a note-off for a note that already
    /// finished — or a second note-off for the same note — costs nothing and
    /// changes nothing.
    ///
    /// Releasing out of the attack stage decays from the *current* level
    /// rather than snapping to peak, so tapping and releasing a held key
    /// fades out from however far the attack got instead of jumping up to
    /// full velocity and decaying from there.
    ///
    /// Only meaningful with [`HoldMode::Gated`]; on a [`HoldMode::Timed`]
    /// envelope the hold is already bounded, so a release just ends it
    /// early.
    #[inline]
    pub fn release(&mut self) {
        if matches!(self.stage, Stage::Attack | Stage::Hold) {
            self.stage = Stage::Decay;
        }
    }

    /// True while the envelope is producing non-zero output.
    #[inline]
    pub fn is_active(&self) -> bool {
        self.stage != Stage::Idle
    }

    /// Current level without advancing.
    #[inline(always)]
    pub fn peek(&self) -> f32 {
        self.value
    }

    /// Advance one sample and return the level *before* the step, so a
    /// freshly triggered envelope yields its full peak on the first sample
    /// of hold/decay rather than vanishing instantly.
    #[inline(always)]
    pub fn tick(&mut self) -> f32 {
        let out = self.value;
        match self.stage {
            Stage::Idle => {}
            Stage::Attack => {
                self.value += self.attack_inc * self.peak;
                if self.value >= self.peak {
                    self.value = self.peak;
                    self.stage = self.enter_hold_or_decay();
                }
            }
            Stage::Hold => {
                // The hold itself never changes `value` — it sits at peak.
                // The only thing that ends a hold is its countdown, armed
                // from `hold_s` for a timed hold or from the watchdog for a
                // gated one (see `enter_hold_or_decay`).
                self.hold_left = self.hold_left.saturating_sub(1);
                if self.hold_left == 0 {
                    self.stage = Stage::Decay;
                }
            }
            Stage::Decay => {
                self.value *= self.decay_coeff;
                if self.value < DENORMAL_FLOOR {
                    self.value = 0.0;
                    self.stage = Stage::Idle;
                }
            }
        }
        out
    }
}

impl Default for AhdEnv {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn silent_until_triggered() {
        let mut e = AhdEnv::new();
        e.set_params(0.0, 0.0, 0.1);
        for _ in 0..1000 {
            assert_eq!(e.tick(), 0.0);
        }
        assert!(!e.is_active());
    }

    #[test]
    fn zero_attack_jumps_to_peak() {
        let mut e = AhdEnv::new();
        e.set_params(0.0, 0.0, 0.1);
        e.trigger(1.0);
        assert_eq!(e.tick(), 1.0, "first sample should be the peak");
        // Already in decay; should be slightly less next sample.
        assert!(e.tick() < 1.0);
    }

    #[test]
    fn attack_ramps_linearly() {
        let mut e = AhdEnv::new();
        e.set_params(0.01, 0.0, 0.1);
        e.trigger(1.0);
        let n = (0.01 * SAMPLE_RATE) as usize;
        // The tick contract is "return the level *before* the step", so the
        // very first sample is 0 and the segment completes on transition;
        // the (n+1)th sample is the held peak. Loop n samples to see the
        // ramp, then confirm a held peak sample follows.
        let mut last = -1.0;
        for _ in 0..n {
            let v = e.tick();
            assert!(v > last, "attack should rise: {v} <= {last}");
            last = v;
        }
        // End of attack window: we should have just reached the peak.
        assert!((last - 1.0).abs() < 0.01, "did not reach peak: {last}");
        // One more tick returns the held/decayed peak.
        let peak_sample = e.tick();
        approx::assert_abs_diff_eq!(peak_sample, 1.0, epsilon = 1e-6);
    }

    #[test]
    fn hold_sits_at_peak() {
        let mut e = AhdEnv::new();
        e.set_params(0.0, 0.02, 0.1);
        e.trigger(1.0);
        let n = (0.02 * SAMPLE_RATE) as usize;
        // First tick is the held peak; n total hold ticks each yield peak.
        let first = e.tick();
        approx::assert_abs_diff_eq!(first, 1.0, epsilon = 1e-6);
        for _ in 0..(n - 1) {
            approx::assert_abs_diff_eq!(e.tick(), 1.0, epsilon = 1e-6);
        }
        // The nth hold tick transitions stage->Decay and still returns 1.0.
        approx::assert_abs_diff_eq!(e.tick(), 1.0, epsilon = 1e-6);
        assert!(matches!(e.stage, Stage::Decay), "should be in decay");
        // Next tick should be < 1.0 (the level before the next decay step).
        let decayed = e.tick();
        assert!(decayed < 1.0, "did not decay: {decayed}");
    }

    #[test]
    fn decays_to_silence() {
        let mut e = AhdEnv::new();
        e.set_params(0.0, 0.0, 0.05);
        e.trigger(1.0);
        for _ in 0..(5.0 * SAMPLE_RATE) as usize {
            e.tick();
        }
        assert!(!e.is_active(), "should have flushed to zero");
        assert_eq!(e.peek(), 0.0);
    }

    #[test]
    fn velocity_scales_peak() {
        let mut a = AhdEnv::new();
        a.set_params(0.0, 0.0, 0.1);
        a.trigger(1.0);
        let loud = a.tick();

        let mut b = AhdEnv::new();
        b.set_params(0.0, 0.0, 0.1);
        b.trigger(0.25);
        let quiet = b.tick();
        assert!(loud > quiet, "velocity had no effect: {loud} vs {quiet}");
        approx::assert_abs_diff_eq!(quiet, 0.25, epsilon = 1e-6);
    }

    #[test]
    fn degenerate_params_are_safe() {
        let mut e = AhdEnv::new();
        e.set_params(0.0, 0.0, 0.0);
        e.trigger(1.0);
        for _ in 0..1024 {
            let s = e.tick();
            assert!(s.is_finite(), "non-finite: {s}");
        }
    }

    // ----- gate close -----

    /// The whole point of the gated mode: with no watchdog the hold does not
    /// end on its own, so a note lasts as long as the key is down and
    /// `hold_s` has no say in it.
    #[test]
    fn gated_hold_outlives_the_timed_hold() {
        let mut e = AhdEnv::new();
        e.set_hold_mode(HoldMode::Gated);
        e.set_params(0.001, 0.05, 0.01); // 50 ms timed hold, ignored here
        e.set_max_hold_s(0.0); // no watchdog
        e.trigger(1.0);

        // Well past both the attack and the 50 ms a timed hold would have
        // used: still holding.
        for _ in 0..(1.0 * SAMPLE_RATE) as usize {
            e.tick();
            assert!(e.is_active(), "gated hold ended without a release");
        }
        approx::assert_abs_diff_eq!(e.peek(), 1.0, epsilon = 1e-6);
    }

    /// A timed hold and a gated hold of identical parameters differ only in
    /// what ends them. This is the regression guard for the mode being
    /// consulted at all — a `tick` that ignored `hold_mode` would pass the
    /// test above and fail this one.
    #[test]
    fn timed_hold_still_times_out_unchanged() {
        let mut e = AhdEnv::new();
        e.set_hold_mode(HoldMode::Timed);
        e.set_params(0.0, 0.01, 0.01);
        e.set_max_hold_s(10.0); // must be ignored in timed mode
        e.trigger(1.0);
        for _ in 0..(0.1 * SAMPLE_RATE) as usize {
            e.tick();
        }
        assert!(!e.is_active(), "timed hold should have ended on its own");
    }

    #[test]
    fn release_drops_a_hold_into_decay() {
        let mut e = AhdEnv::new();
        e.set_hold_mode(HoldMode::Gated);
        e.set_params(0.0, 10.0, 0.05);
        e.trigger(1.0);
        assert_eq!(e.tick(), 1.0);

        e.release();
        assert!(e.is_active(), "release should decay, not cut");
        // `tick` returns the level *before* the step, so the sample right
        // after a release out of a hold is still the peak; the decay shows
        // up on the next one.
        assert_eq!(e.tick(), 1.0, "release should not jump the level");
        let after = e.tick();
        assert!(after < 1.0, "still at peak after release: {after}");

        for _ in 0..(5.0 * SAMPLE_RATE) as usize {
            e.tick();
        }
        assert!(!e.is_active(), "decay should have run out");
        assert_eq!(e.peek(), 0.0);
    }

    /// Tapping and releasing mid-attack must not jump up to full velocity.
    /// A release that snapped to peak would turn every staccato into a
    /// full-velocity stab.
    #[test]
    fn release_during_attack_decays_from_the_current_level() {
        let mut e = AhdEnv::new();
        e.set_hold_mode(HoldMode::Gated);
        e.set_params(0.1, 10.0, 0.05);
        e.trigger(1.0);

        // A tenth of the way into the attack ramp.
        let quarter = (0.01 * SAMPLE_RATE) as usize;
        let mut last = 0.0;
        for _ in 0..quarter {
            last = e.tick();
        }
        assert!(
            last > 0.0 && last < 0.5,
            "expected a partial attack: {last}"
        );

        e.release();
        // The release decays from wherever the ramp had reached. One tick
        // may still show the pre-step level; it must not exceed the peak.
        let mut peak_after = 0.0f32;
        for _ in 0..8 {
            peak_after = peak_after.max(e.tick());
        }
        assert!(
            peak_after <= last + 0.01,
            "release jumped the level up: {last} -> {peak_after}"
        );
        assert!(peak_after < 0.5, "release snapped to a full-level stab");
    }

    /// Note-offs are not always paired, and a stray one must be inert.
    #[test]
    fn release_is_a_noop_when_idle_or_decaying() {
        let mut e = AhdEnv::new();
        e.set_hold_mode(HoldMode::Gated);
        e.set_params(0.0, 1.0, 0.05);

        // Idle.
        e.release();
        assert!(!e.is_active());
        assert_eq!(e.tick(), 0.0);

        // Already decaying: a second release must not change the trajectory.
        e.trigger(1.0);
        e.release();
        let first = e.tick();
        e.release();
        e.release();
        let second = e.tick();
        assert!(
            second < first,
            "repeat release disturbed the decay: {first} -> {second}"
        );
    }

    /// The opt-in backstop. With no note-off at all, the watchdog still ends
    /// the note, so `is_active` — the engine's per-track cost gate — cannot be
    /// pinned open by a lost event.
    #[test]
    fn gated_watchdog_caps_a_lost_note_off() {
        let mut e = AhdEnv::new();
        e.set_hold_mode(HoldMode::Gated);
        e.set_params(0.0, 0.0, 0.01); // no timed hold either
        e.set_max_hold_s(0.05);
        e.trigger(1.0);

        let still_holding = (0.02 * SAMPLE_RATE) as usize;
        for _ in 0..still_holding {
            e.tick();
            assert!(e.is_active(), "ended before the watchdog");
        }
        for _ in 0..(1.0 * SAMPLE_RATE) as usize {
            e.tick();
        }
        assert!(!e.is_active(), "the watchdog did not fire");
    }

    /// The watchdog is re-armed per trigger, so a second note gets a full
    /// one rather than inheriting a spent countdown.
    #[test]
    fn watchdog_rearms_on_every_trigger() {
        let mut e = AhdEnv::new();
        e.set_hold_mode(HoldMode::Gated);
        e.set_params(0.0, 0.0, 0.01);
        e.set_max_hold_s(0.05);

        e.trigger(1.0);
        for _ in 0..(0.04 * SAMPLE_RATE) as usize {
            e.tick();
        }
        e.trigger(1.0); // retrigger before the watchdog expires
        for _ in 0..(0.04 * SAMPLE_RATE) as usize {
            e.tick();
            assert!(e.is_active(), "retrigger did not re-arm the watchdog");
        }
    }

    #[test]
    fn default_hold_mode_is_timed() {
        assert_eq!(AhdEnv::new().hold_mode(), HoldMode::Timed);
    }

    #[test]
    fn max_hold_round_trips() {
        let mut e = AhdEnv::new();
        assert_eq!(e.max_hold_s(), 0.0, "no watchdog by default");
        e.set_max_hold_s(2.5);
        assert!((e.max_hold_s() - 2.5).abs() < 1.0 / SAMPLE_RATE);
        e.set_max_hold_s(0.0);
        assert_eq!(e.max_hold_s(), 0.0);
    }
}
