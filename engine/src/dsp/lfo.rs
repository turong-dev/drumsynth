//! Low-frequency oscillators.
//!
//! Block-rate, not sample-rate: one `tick_block` advances the phase by one
//! processing block (32 samples at 48kHz = 667µs). That gives a 1.5kHz
//! control rate — more than enough for LFOs in the 0.1..20 Hz range that
//! drum tracks want, and it keeps the per-sample audio path untouched by
//! modulation work.
//!
//! Each LFO owns a single [`ModDest`] destination and a bipolar depth, the
//! same surface the Syntakt gives its two per-track LFOs (DEST + DEP).

use crate::BLOCK;
use crate::SAMPLE_RATE;

/// LFO rate range.
///
/// A single rate knob is easier to dial in if its travel is split into two
/// ranges: slow for sweeps and fades, fast for tremolo/FX. The normalized
/// `0..1` rate is mapped logarithmically across the chosen range.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum LfoRateMode {
    /// Slow range: 0.1 Hz to 10 Hz.
    Slow,
    /// Fast range: 1 Hz to 100 Hz.
    Fast,
}

impl LfoRateMode {
    /// Range bounds `(min_hz, max_hz)` for this mode.
    #[inline]
    const fn range(self) -> (f32, f32) {
        match self {
            Self::Slow => (0.1, 10.0),
            Self::Fast => (1.0, 100.0),
        }
    }
}

/// LFO waveform.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum LfoWave {
    /// Bipolar triangle, -1..1.
    Triangle,
    /// Bipolar sine, -1..1.
    Sine,
    /// Bipolar square, -1..1.
    Square,
    /// Bipolar sawtooth (rising), -1..1.
    Saw,
    /// Bipolar ramp (falling saw), -1..1.
    Ramp,
    /// Unipolar exponential decay curve, 0..1, repeats.
    Exp,
}

/// LFO trigger behaviour.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum LfoMode {
    /// Free-running from power-on; ignores triggers.
    Free,
    /// Phase resets to `start_phase` on trigger, then free-runs.
    Trig,
    /// Latches the value at trigger time and holds it.
    Hold,
    /// One cycle then stops at zero.
    OneShot,
}

/// What a modulator can address.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum ModDest {
    /// Machine macro knob `0..7`.
    Macro(usize),
    /// Strip filter cutoff.
    FilterCutoff,
    /// Strip filter resonance.
    FilterReso,
    /// Strip drive amount.
    Drive,
    /// Strip pan.
    Pan,
    /// Strip level.
    Level,
    /// Strip amp-decay time.
    AmpDecay,
    /// Strip send level to the delay bus.
    SendDelay,
    /// Strip send level to the reverb bus.
    SendReverb,
    /// No destination — LFO runs but contributes nothing.
    None,
}

impl ModDest {
    /// Quantise a normalised `0..1` macro value onto a [`ModDest`].
    ///
    /// Sixteen steps, evenly spaced: the eight macro knobs, then the strip
    /// destinations, then [`ModDest::None`] as a final "off" position. This is
    /// the mapping the LFO-destination macro slots use.
    pub fn from_macro(v: f32) -> Self {
        const DESTS: [ModDest; 16] = [
            ModDest::Macro(0),
            ModDest::Macro(1),
            ModDest::Macro(2),
            ModDest::Macro(3),
            ModDest::Macro(4),
            ModDest::Macro(5),
            ModDest::Macro(6),
            ModDest::Macro(7),
            ModDest::FilterCutoff,
            ModDest::FilterReso,
            ModDest::Drive,
            ModDest::Pan,
            ModDest::Level,
            ModDest::AmpDecay,
            ModDest::SendDelay,
            ModDest::SendReverb,
        ];
        let i = (v.clamp(0.0, 1.0) * (DESTS.len() - 1) as f32 + 0.5) as usize;
        DESTS[i.min(DESTS.len() - 1)]
    }
}

/// One LFO with a single destination and bipolar depth.
#[derive(Clone, Copy)]
pub struct Lfo {
    // Config (setup rate)
    speed_hz: f32,
    wave: LfoWave,
    mode: LfoMode,
    depth: f32,
    dest: ModDest,
    start_phase: f32,
    rate_mode: LfoRateMode,
    norm_rate: f32,
    // State (advanced per block)
    phase: f32,
    value: f32,
    active: bool,
    triggered: bool,
}

impl Lfo {
    /// Idle LFO, free-running sine at 1Hz, depth 0, destination None.
    pub const fn new() -> Self {
        Self {
            speed_hz: 1.0,
            wave: LfoWave::Sine,
            mode: LfoMode::Free,
            depth: 0.0,
            dest: ModDest::None,
            start_phase: 0.0,
            rate_mode: LfoRateMode::Slow,
            norm_rate: 0.5,
            phase: 0.0,
            value: 0.0,
            active: true,
            triggered: false,
        }
    }

    /// Configure the LFO. Setup rate.
    pub fn set_params(
        &mut self,
        speed_hz: f32,
        wave: LfoWave,
        mode: LfoMode,
        depth: f32,
        dest: ModDest,
        start_phase: f32,
    ) {
        self.speed_hz = speed_hz;
        self.wave = wave;
        self.mode = mode;
        self.depth = depth;
        self.dest = dest;
        self.start_phase = start_phase.clamp(0.0, 1.0);
    }

    /// Current destination.
    pub fn dest(&self) -> ModDest {
        self.dest
    }

    /// Current waveform.
    pub fn wave(&self) -> LfoWave {
        self.wave
    }

    /// Current trigger mode.
    pub fn mode(&self) -> LfoMode {
        self.mode
    }

    /// Current start phase, `0..1`.
    pub fn start_phase(&self) -> f32 {
        self.start_phase
    }

    /// Current rate range.
    pub fn rate_mode(&self) -> LfoRateMode {
        self.rate_mode
    }

    /// Current normalized rate `0..1` (used when the LFO was configured via
    /// [`set_rate`](Self::set_rate)).
    pub fn norm_rate(&self) -> f32 {
        self.norm_rate
    }

    /// Current absolute speed in Hz.
    pub fn speed_hz(&self) -> f32 {
        self.speed_hz
    }

    /// Configure the LFO from a normalized rate knob plus a slow/fast range.
    ///
    /// `rate` is `0..1`; it is mapped logarithmically across the selected
    /// range so the knob feels evenly spaced. This is the user-facing entry
    /// point; [`set_params`](Self::set_params) remains available for direct
    /// Hz values.
    pub fn set_rate(&mut self, rate: f32, mode: LfoRateMode) {
        self.norm_rate = rate.clamp(0.0, 1.0);
        self.rate_mode = mode;
        self.speed_hz = Self::map_norm_rate(self.norm_rate, mode);
    }

    fn map_norm_rate(rate: f32, mode: LfoRateMode) -> f32 {
        let (min, max) = mode.range();
        min * libm::powf(max / min, rate)
    }

    /// Is this LFO contributing (active and depth ≠ 0 and dest ≠ None)?
    pub fn is_contributing(&self) -> bool {
        self.active && self.depth != 0.0 && self.dest != ModDest::None
    }

    /// Latch the trigger event. Free-run LFOs ignore this; Trig/OneShot/Hold
    /// modes use it to reset phase or latch value.
    pub fn trigger(&mut self) {
        match self.mode {
            LfoMode::Free => {}
            LfoMode::Trig | LfoMode::OneShot => {
                self.phase = self.start_phase;
                self.active = true;
                self.triggered = true;
            }
            LfoMode::Hold => {
                // Snap the phase forward one step and hold that value.
                self.tick_block();
                self.active = false;
            }
        }
    }

    /// Reset to idle.
    pub fn reset(&mut self) {
        self.phase = self.start_phase;
        self.value = 0.0;
        self.active = self.mode == LfoMode::Free;
        self.triggered = false;
    }

    /// Advance one block's worth of phase and recompute `value`.
    pub fn tick_block(&mut self) {
        if !self.active {
            return;
        }

        let p = self.phase;

        self.value = match self.wave {
            LfoWave::Triangle => {
                // Triangle: 0..0.5 rises -1..1, 0.5..1 falls 1..-1.
                let t = if p < 0.5 { p * 2.0 } else { 2.0 - p * 2.0 };
                t * 2.0 - 1.0
            }
            LfoWave::Sine => crate::dsp::fast::sin_turns(p) * 2.0,
            LfoWave::Square => {
                if p < 0.5 {
                    1.0
                } else {
                    -1.0
                }
            }
            LfoWave::Saw => p * 2.0 - 1.0,
            LfoWave::Ramp => 1.0 - p * 2.0,
            LfoWave::Exp => {
                // Exponential decay shape, repeating. Uses the same exp2_approx
                // as the oscillator pitch sweeps but on a unipolar curve.
                let t = 1.0 - p;
                crate::dsp::fast::exp2_approx(-4.0 * t * t)
            }
        };

        // Advance phase.
        let inc = self.speed_hz.abs() * BLOCK as f32 / SAMPLE_RATE;
        let phase_dir = if self.speed_hz < 0.0 { -1.0 } else { 1.0 };
        self.phase += inc * phase_dir;
        if self.phase >= 1.0 {
            self.phase -= 1.0;
            if self.mode == LfoMode::OneShot && self.triggered {
                self.active = false;
                self.value = 0.0;
            }
        }
        if self.phase < 0.0 {
            self.phase += 1.0;
            if self.mode == LfoMode::OneShot && self.triggered {
                self.active = false;
                self.value = 0.0;
            }
        }
    }

    /// Current output value, range depends on waveform (see [`LfoWave`]).
    #[inline(always)]
    pub fn value(&self) -> f32 {
        self.value
    }

    /// Bipolar depth, -1..1.
    #[inline(always)]
    pub fn depth(&self) -> f32 {
        self.depth
    }
}

impl Default for Lfo {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sine_lfo_completes_a_cycle() {
        let mut lfo = Lfo::new();
        lfo.set_params(100.0, LfoWave::Sine, LfoMode::Free, 1.0, ModDest::None, 0.0);
        let blocks_per_cycle = (SAMPLE_RATE / BLOCK as f32 / 100.0) as usize;
        let mut peak = 0.0f32;
        for _ in 0..blocks_per_cycle {
            lfo.tick_block();
            peak = peak.max(lfo.value().abs());
        }
        assert!(peak > 0.9, "sine LFO never reached near-unity: {peak}");
    }

    #[test]
    fn square_lfo_alternates() {
        let mut lfo = Lfo::new();
        lfo.set_params(1.0, LfoWave::Square, LfoMode::Free, 1.0, ModDest::None, 0.0);
        lfo.tick_block();
        let first = lfo.value();
        // Advance past the half-way point.
        let half_cycle = (SAMPLE_RATE as f32 / BLOCK as f32 / 2.0) as usize + 1;
        for _ in 0..half_cycle {
            lfo.tick_block();
        }
        let second = lfo.value();
        assert!(
            first > 0.0 && second < 0.0,
            "square didn't flip: {first} then {second}"
        );
    }

    #[test]
    fn triangle_is_bounded() {
        let mut lfo = Lfo::new();
        lfo.set_params(
            1.0,
            LfoWave::Triangle,
            LfoMode::Free,
            1.0,
            ModDest::None,
            0.0,
        );
        for _ in 0..1000 {
            lfo.tick_block();
            assert!(
                lfo.value().abs() <= 1.01,
                "triangle escaped: {}",
                lfo.value()
            );
        }
    }

    #[test]
    fn trig_mode_resets_phase() {
        let mut lfo = Lfo::new();
        lfo.set_params(1.0, LfoWave::Sine, LfoMode::Trig, 1.0, ModDest::None, 0.0);
        // Run a bit.
        for _ in 0..100 {
            lfo.tick_block();
        }
        let mid_value = lfo.value();
        // Trigger should reset phase to 0 (start_phase=0), restarting.
        lfo.trigger();
        lfo.tick_block();
        let after = lfo.value();
        assert!(
            after.abs() < mid_value.abs() || mid_value.abs() < 0.01,
            "trig didn't reset: mid={mid_value} after={after}"
        );
    }

    #[test]
    fn one_shot_stops_after_one_cycle() {
        let mut lfo = Lfo::new();
        // 1500 blocks at 48kHz/32-sample blocks = 1 second; 100Hz → 100 cycles.
        lfo.set_params(
            100.0,
            LfoWave::Sine,
            LfoMode::OneShot,
            1.0,
            ModDest::None,
            0.0,
        );
        lfo.trigger();
        let blocks_per_cycle = (SAMPLE_RATE as f32 / BLOCK as f32 / 100.0) as usize + 1;
        for _ in 0..blocks_per_cycle {
            lfo.tick_block();
        }
        assert!(!lfo.active, "one-shot didn't stop after one cycle");
    }

    #[test]
    fn free_mode_ignores_trigger() {
        let mut lfo = Lfo::new();
        lfo.set_params(1.0, LfoWave::Sine, LfoMode::Free, 1.0, ModDest::None, 0.0);
        for _ in 0..100 {
            lfo.tick_block();
        }
        let before = lfo.phase;
        lfo.trigger();
        assert_eq!(lfo.phase, before, "free mode shouldn't reset on trigger");
    }

    #[test]
    fn exp_wave_is_unipolar() {
        let mut lfo = Lfo::new();
        lfo.set_params(1.0, LfoWave::Exp, LfoMode::Free, 1.0, ModDest::None, 0.0);
        for _ in 0..1000 {
            lfo.tick_block();
            assert!(lfo.value() >= 0.0, "exp went negative: {}", lfo.value());
            assert!(lfo.value() <= 1.01, "exp exceeded unity: {}", lfo.value());
        }
    }

    #[test]
    fn is_contributing_requires_active_depth_and_dest() {
        let mut lfo = Lfo::new();
        lfo.set_params(
            1.0,
            LfoWave::Sine,
            LfoMode::Free,
            0.0,
            ModDest::Macro(0),
            0.0,
        );
        assert!(!lfo.is_contributing(), "depth 0 should not contribute");
        lfo.set_params(1.0, LfoWave::Sine, LfoMode::Free, 0.5, ModDest::None, 0.0);
        assert!(!lfo.is_contributing(), "dest None should not contribute");
        lfo.set_params(
            1.0,
            LfoWave::Sine,
            LfoMode::Free,
            0.5,
            ModDest::Macro(0),
            0.0,
        );
        assert!(lfo.is_contributing(), "should contribute with depth + dest");
    }

    #[test]
    fn set_rate_maps_to_expected_range() {
        let mut lfo = Lfo::new();

        lfo.set_rate(0.0, LfoRateMode::Slow);
        approx::assert_abs_diff_eq!(lfo.speed_hz(), 0.1, epsilon = 1e-6);

        lfo.set_rate(1.0, LfoRateMode::Slow);
        approx::assert_abs_diff_eq!(lfo.speed_hz(), 10.0, epsilon = 1e-6);

        lfo.set_rate(0.0, LfoRateMode::Fast);
        approx::assert_abs_diff_eq!(lfo.speed_hz(), 1.0, epsilon = 1e-6);

        lfo.set_rate(1.0, LfoRateMode::Fast);
        approx::assert_abs_diff_eq!(lfo.speed_hz(), 100.0, epsilon = 1e-6);
    }

    #[test]
    fn set_rate_is_logarithmic() {
        let mut lfo = Lfo::new();
        lfo.set_rate(0.5, LfoRateMode::Slow);
        let mid = lfo.speed_hz();
        // Geometric midpoint of 0.1 and 10 is 1.0.
        approx::assert_abs_diff_eq!(mid, 1.0, epsilon = 1e-6);
    }

    #[test]
    fn set_rate_clamps_and_cycles_faster_in_fast_mode() {
        let mut slow = Lfo::new();
        slow.set_rate(1.0, LfoRateMode::Slow);
        let slow_hz = slow.speed_hz();

        let mut fast = Lfo::new();
        fast.set_rate(1.0, LfoRateMode::Fast);
        let fast_hz = fast.speed_hz();

        assert!(fast_hz > slow_hz, "fast mode max should exceed slow mode max");

        // Same normalized rate in the two modes should produce different
        // absolute frequencies.
        let mut slow = Lfo::new();
        slow.set_rate(0.5, LfoRateMode::Slow);
        let mut fast = Lfo::new();
        fast.set_rate(0.5, LfoRateMode::Fast);
        assert!(fast.speed_hz() > slow.speed_hz() * 5.0, "fast 0.5 should be much faster than slow 0.5");
    }

    #[test]
    fn set_rate_overrides_previous_params() {
        let mut lfo = Lfo::new();
        lfo.set_params(0.3, LfoWave::Sine, LfoMode::Free, 0.5, ModDest::FilterCutoff, 0.0);
        lfo.set_rate(0.75, LfoRateMode::Slow);
        assert_eq!(lfo.rate_mode(), LfoRateMode::Slow);
        assert!(lfo.speed_hz() > 0.3, "set_rate should raise speed from 0.3");
    }
}
