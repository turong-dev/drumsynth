//! Sweep FX: white noise through a swept SVF, gated by a long
//! self-timed AHD envelope.
//!
//! The "filter sweep" riser — the most-used gesture in electronic FX.
//! White noise drives a state-variable filter whose cutoff is
//! modulated by an internal LFO; an [`AhdEnv`] owns the gesture
//! length. Distinct from every other machine in the catalogue, which
//! are one-shot percussive hits — see [`DubSiren`](super::dub_siren::DubSiren)
//! for the sustained-gesture rationale.
//!
//! # Topology
//!
//! ```text
//!   internal LFO (triangle) ──► cutoff (octaves, exp2)
//!   AhdEnv (gesture)         ──► amp
//!   Noise ──► Svf (LP/BP/HP) ──► out
//! ```
//!
//! This is the first machine to use [`Svf`] inside a machine — until
//! Phase 12, [`Svf`] was exclusively a per-track strip component.
//! Resonance lives at [`SLOT_FILT_1`] (slot 9), the family-wide
//! resonance slot — the same disposition as [`BdVa`](super::bd_va::BdVa)'s
//! Q. [`Svf`] is the resonant network; the FILTER bank's cutoff slot
//! ([`SLOT_FILT_0`], slot 8) is unused, since the cutoff is *swept*
//! rather than static.
//!
//! The internal LFO runs at sample rate for the same reason as the
//! dub siren: a block-rate LFO would step a 1 Hz cutoff sweep in 6
//! audible increments per cycle. Triangle shape, fixed — the sweep
//! wants a back-and-forth movement, which sine and saw do not give
//! as cleanly. The per-sample cutoff retune pays one
//! [`fast::sin_turns`] lookup (via [`Svf::set_cutoff`]), not a full
//! [`Svf::recalc`] — the Option B split the bench's `8+FX+SWFX` row
//! gated (83.4% on the full `recalc`, over the 70% ceiling).
//!
//! # Macros
//!
//! Canonical 4-bank layout (PITCH/FILTER/AMP/MOD), flat index `bank*8+slot`,
//! MIDI CC `20 + flat` on the track's channel:
//!
//! | idx | CC  | name    | range         | notes |
//! |-----|-----|---------|---------------|-------|
//! | 0   | 20  | RATE    | 0.1..5 Hz      | sweep LFO speed |
//! | 1   | 21  | DEPTH   | 0..4 oct       | cutoff sweep width (exp2 from START) |
//! | 2   | 22  | START   | 80..8000 Hz    | cutoff centre |
//! | 5   | 25  | MACH    | 0..1           | machine selector (quantised over MachineId::ALL) |
//! | 9   | 29  | RESO    | 0.5..12 Q      | SVF resonance (FILTER bank resonance slot) |
//! | 16  | 36  | LEVEL   | 0..1           | per-machine output level |
//! | 17  | 37  | PAN     | 0..1           | (track-routed; ignored here) |
//! | 18  | 38  | DEC     | 0.5..6 s       | AHD gesture length (5% atk / 85% hold / 10% dec) |
//! | 20  | 40  | MODE    | 0..1           | LP/BP/HP, quantised (default BP — the classic riser shape) |
//! | 22  | 42  | SEND.DLY| 0..1           | delay send (track-routed) |
//! | 23  | 43  | SEND.RVB| 0..1           | reverb send (track-routed) |
//! | 26  | 46  | OUT     | 0..1           | track routing (Master/Aux1/2/3) |
//!
//! All other slots are RESV (default 0.0) and ignored.

use crate::dsp::{fast, AhdEnv, Noise, Svf, SvfMode};
use crate::machines::{
    NUM_MACROS, SLOT_MACH_5, SLOT_LEVEL, SLOT_FILT_1, SLOT_MACH_7, SLOT_MACH_1, SLOT_MACH_2,
    SLOT_MACH_0,
};
use crate::SAMPLE_RATE;

/// SVF mode selected by the MODE macro.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum SweepMode {
    Lp,
    Bp,
    Hp,
}

impl SweepMode {
    /// Quantise a 0..1 macro value to one of three modes in equal
    /// thirds. The default macro (0.0) lands in the first band →
    /// [`Bp`], the classic riser shape.
    const fn from_macro(v: f32) -> Self {
        if v < 1.0 / 3.0 {
            Self::Bp
        } else if v < 2.0 / 3.0 {
            Self::Lp
        } else {
            Self::Hp
        }
    }

    const fn to_svf_mode(self) -> SvfMode {
        match self {
            Self::Lp => SvfMode::Lp,
            Self::Bp => SvfMode::Bp,
            Self::Hp => SvfMode::Hp,
        }
    }
}

/// Sweep FX machine.
pub struct SweepFx {
    filter: Svf,
    env: AhdEnv,
    noise: Noise,
    /// Cutoff centre frequency in Hz, post-retune (though `retune`
    /// no-ops for noise-only machines). The LFO sweeps around this.
    start_hz: f32,
    /// Sweep width in octaves. The LFO outputs -1..1; the per-sample
    /// cutoff is `start * 2^(lfo * depth)`.
    depth_oct: f32,
    /// LFO phase accumulator, in turns (`0.0..1.0` = one cycle).
    lfo_phase: f32,
    /// LFO phase increment per sample = `rate / SAMPLE_RATE`.
    lfo_inc: f32,
    /// SVF mode, set by the MODE macro.
    mode: SweepMode,
    /// SVF resonance (Q). Cached so `tick` never reads the macro.
    q: f32,
    /// Highest cutoff the SVF can take at this Q before the Chamberlin
    /// integrators go unstable (see `set_macros`). Cached per gesture —
    /// Q only changes at setup rate, so `tick` costs a single `min`.
    cutoff_max: f32,
    /// Per-machine output level, 0..1.
    level: f32,
}

impl SweepFx {
    /// Build the machine with the given macro values applied.
    pub fn new(macros: &[f32; NUM_MACROS]) -> Self {
        let mut m = Self {
            filter: Svf::new(SvfMode::Bp),
            env: AhdEnv::new(),
            noise: Noise::new(0x4F73_A19D),
            start_hz: 0.0,
            depth_oct: 0.0,
            lfo_phase: 0.0,
            lfo_inc: 0.0,
            mode: SweepMode::Bp,
            q: 0.707,
            cutoff_max: SAMPLE_RATE / 6.0,
            level: 0.0,
        };
        m.set_macros(macros);
        m
    }

    /// Recompute coefficients from macros. Setup rate.
    pub fn set_macros(&mut self, macros: &[f32; NUM_MACROS]) {
        let rate_hz = 0.1 + 4.9 * macros[SLOT_MACH_0]; // RATE 0.1..5 Hz
        let depth_oct = 4.0 * macros[SLOT_MACH_1]; // DEPTH 0..4 oct
        let start_hz = 80.0 + 7920.0 * macros[SLOT_MACH_2]; // START 80..8000 Hz
        let q = 0.5 + 11.5 * macros[SLOT_FILT_1]; // RESO 0.5..12 Q (FILTER resonance slot)
        let total_s = 0.5 + 5.5 * macros[SLOT_MACH_5]; // DEC 0.5..6 s
        let mode = SweepMode::from_macro(macros[SLOT_MACH_7]); // MODE LP/BP/HP
        let level = macros[SLOT_LEVEL]; // LEVEL 0..1

        self.start_hz = start_hz;
        self.depth_oct = depth_oct;
        self.lfo_inc = rate_hz * crate::INV_SAMPLE_RATE;
        self.mode = mode;
        self.q = q;
        self.level = level;

        // The Chamberlin SVF (`dsp/svf.rs`) is only stable where the pole
        // stays inside the unit circle: with `c1 = 2·sin(π·fc/fs)` and
        // `k = 1/Q`, that bound is `c1² + 2·c1·k < 4`, i.e.
        // `fc < fs/π · asin((√(k²+4) − k)/2)`. For RESO=0 (Q=0.5, k=2)
        // the ceiling is ≈ 6.5 kHz; for RESO=1 (Q=12, k=1/12) it is
        // ≈ 19.4 kHz. Above it the integrators pump to NaN, which the
        // earlier fixed 12 kHz clamp did not catch (the extreme-macros
        // test found it at RESO=0.1). Q is setup-rate, so the ceiling is
        // cached here and `tick` only pays a `min`.
        let k = (1.0 / q.max(0.5)).min(2.0);
        self.cutoff_max =
            SAMPLE_RATE / core::f32::consts::PI * libm::asinf((libm::sqrtf(k * k + 4.0) - k) * 0.5);

        // Seed the filter at the start cutoff so the first sample of
        // the gesture is not silent waiting for the integrators to
        // charge. The integrator state is zeroed; only the coefficients
        // are precomputed here. The per-sample path refreshes `c1` as
        // the LFO moves the cutoff.
        self.filter.set_mode(mode.to_svf_mode());
        self.filter.recalc(start_hz, q, SAMPLE_RATE);

        // AHD proportions: 5% attack, 85% hold, 10% decay. Same
        // disposition as DubSiren — the hold is the sustained body
        // of the sweep, the decay lets it close cleanly.
        let atk = 0.05 * total_s;
        let hold = 0.85 * total_s;
        let dec = 0.10 * total_s;
        self.env.set_params(atk, hold, dec);
    }

    /// Transpose by `semis` semitones relative to the macro pitch.
    ///
    /// Noise-only machine — no pitch to transpose. This is a no-op per
    /// the Hat Classic precedent (`machines/mod.rs:1019`). Implemented
    /// as a no-op rather than `unreachable!` so a kit-level transpose
    /// (e.g. renderer's per-step semitone lane) does not panic when
    /// the track happens to hold a noise machine.
    pub fn retune(&mut self, _semis: f32) {}

    /// Begin a gesture at `velocity` (0.0..=1.0). The AHD peak scales
    /// with velocity; the gesture length is fixed by [`set_macros`].
    pub fn trigger(&mut self, velocity: f32) {
        self.env.trigger(velocity);
        self.filter.reset();
        self.lfo_phase = 0.0;
    }

    /// Silence.
    pub fn reset(&mut self) {
        self.env.reset();
        self.filter.reset();
        self.lfo_phase = 0.0;
    }

    /// Still sounding?
    pub fn is_active(&self) -> bool {
        self.env.is_active()
    }

    /// Advance the internal triangle LFO by one sample and return its
    /// bipolar output in `-1.0..1.0`.
    #[inline(always)]
    fn tick_lfo(&mut self) -> f32 {
        let p = self.lfo_phase;
        // Phase is kept in [0, 1) by the wrap below, so no floor needed.
        let out = 1.0 - 4.0 * libm::fabsf(p - 0.5);
        self.lfo_phase += self.lfo_inc;
        if self.lfo_phase >= 1.0 {
            self.lfo_phase -= 1.0;
        }
        out
    }

    /// One sample.
    #[inline(always)]
    pub fn tick(&mut self) -> f32 {
        let amp = self.env.tick();
        if amp == 0.0 {
            return 0.0;
        }

        // Cutoff multiplier from LFO × depth-in-octaves, exp2 family.
        let lfo = self.tick_lfo();
        // Clamp the swept cutoff to the Q-dependent stability ceiling
        // computed in `set_macros` (a single `min`; the ceiling varies
        // from ≈6.5 kHz at RESO=0 to ≈19.4 kHz at RESO=1). The lower
        // bound of 80 Hz matches the START macro's floor. Above the
        // ceiling the Chamberlin integrators go unstable and pump to
        // NaN (see the extreme-macros test).
        let cutoff =
            (self.start_hz * fast::exp2_approx(lfo * self.depth_oct)).clamp(80.0, self.cutoff_max);

        // The per-sample path only refreshes `c1` (the cutoff-dependent
        // integrator coefficient) via the sine table — `k` (damping from
        // Q) is setup-rate and stays cached from `set_macros`. A full
        // `Svf::recalc` here would pay `libm::sinf` (~1,200 cycles on the
        // M7) every sample; `set_cutoff` is one `sin_turns` lookup (~10),
        // the same split `BridgedT::set_hz` makes for the bridged-T.
        // The bench's `8+FX+SWFX` row is what gates this choice.
        self.filter.set_cutoff(cutoff, SAMPLE_RATE);

        let n = self.noise.tick();
        // Saturate before the level multiplier. A high-Q SVF with a
        // cutoff sweep can ring far past unity when noise pumps the
        // resonance for seconds (the worst-case macros in the NaN
        // test) — `soft_clip` keeps the per-sample output finite
        // without changing the audible character at normal levels,
        // the same disposition as `CyMetallic::tick`.
        fast::soft_clip(self.filter.tick(n)) * amp * self.level
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::machines::MachineId;

    fn peak_over(s: &mut SweepFx, n: usize) -> f32 {
        let mut peak = 0.0f32;
        for _ in 0..n {
            peak = peak.max(libm::fabsf(s.tick()));
        }
        peak
    }

    #[test]
    fn silent_until_struck() {
        let id = MachineId::SweepFx;
        let macros = id.default_macros();
        let mut s = SweepFx::new(&macros);
        assert_eq!(peak_over(&mut s, 1000), 0.0);
        assert!(!s.is_active());
    }

    #[test]
    fn velocity_scales_output() {
        let id = MachineId::SweepFx;
        let macros = id.default_macros();
        let peak_at = |vel: f32| {
            let mut s = SweepFx::new(&macros);
            s.trigger(vel);
            peak_over(&mut s, 4800)
        };
        let quiet = peak_at(0.25);
        let loud = peak_at(1.0);
        assert!(loud > quiet, "velocity had no effect: {quiet} vs {loud}");
    }

    #[test]
    fn decays_to_silence() {
        let id = MachineId::SweepFx;
        let macros = id.default_macros();
        let mut s = SweepFx::new(&macros);
        s.trigger(1.0);
        for _ in 0..(7.0 * SAMPLE_RATE) as usize {
            s.tick();
        }
        assert!(!s.is_active(), "sweep did not stop");
    }

    #[test]
    fn sustained_gesture_stays_active_past_one_second() {
        // The gesture contract: a sweep is *sustained*, not a hit. At
        // default DEC (~1.9 s) the machine must still be active past
        // 1 s. This is the test that pins the budget argument — see
        // Phase 12's "sustained-gesture test" line in PLAN.md.
        let id = MachineId::SweepFx;
        let macros = id.default_macros();
        let mut s = SweepFx::new(&macros);
        s.trigger(1.0);
        for _ in 0..(1.0 * SAMPLE_RATE) as usize {
            s.tick();
        }
        assert!(s.is_active(), "sweep cut short of 1 s");
        for _ in 0..(6.0 * SAMPLE_RATE) as usize {
            s.tick();
        }
        assert!(!s.is_active(), "sweep ran past 7 s");
    }

    #[test]
    fn rate_macro_changes_cutoff_speed() {
        // A fast RATE LFO produces a sweep that crosses the
        // measurement window's centre frequency more often. We
        // measure the variance of the output peak over short
        // windows: a faster sweep has window-to-window peak
        // variation; a near-static sweep has near-constant peaks.
        let id = MachineId::SweepFx;
        let mut slow = id.default_macros();
        slow[SLOT_MACH_0] = 0.0; // RATE 0.1 Hz
        slow[SLOT_MACH_1] = 0.5; // 2 oct depth — audible sweep
        let mut fast_macros = id.default_macros();
        fast_macros[SLOT_MACH_0] = 1.0; // RATE 5 Hz
        fast_macros[SLOT_MACH_1] = 0.5;

        let window_peak = |macros: &[f32; NUM_MACROS]| {
            let mut s = SweepFx::new(macros);
            s.trigger(1.0);
            let win = 2400; // 50 ms windows
            let mut peaks = [0.0f32; 8];
            for p in &mut peaks {
                *p = peak_over(&mut s, win);
            }
            // Variation = max - min across windows. A fast sweep
            // wanders more in 50 ms than a near-static one.
            let mut lo = f32::INFINITY;
            let mut hi = -f32::INFINITY;
            for &p in &peaks {
                lo = lo.min(p);
                hi = hi.max(p);
            }
            hi - lo
        };

        let slow_var = window_peak(&slow);
        let fast_var = window_peak(&fast_macros);
        assert!(
            fast_var > slow_var,
            "RATE macro should change sweep speed: slow={slow_var}, fast={fast_var}"
        );
    }

    #[test]
    fn retune_is_a_noop() {
        // Noise-only machine — retune must not alter output. Same
        // shape as Hat Classic. A retune followed by trigger produces
        // bit-identical output to a plain trigger.
        let id = MachineId::SweepFx;
        let macros = id.default_macros();
        let mut a = SweepFx::new(&macros);
        let mut b = SweepFx::new(&macros);
        b.retune(7.0);
        a.trigger(1.0);
        b.trigger(1.0);
        for _ in 0..(2.0 * SAMPLE_RATE) as usize {
            assert_eq!(a.tick(), b.tick(), "retune altered noise-only output");
        }
    }

    #[test]
    fn extreme_macros_do_not_produce_nans() {
        let id = MachineId::SweepFx;
        let base = id.default_macros();
        for &v in &[0.0f32, 1.0f32] {
            let mut macros = base;
            for m in macros.iter_mut() {
                *m = v;
            }
            let mut s = SweepFx::new(&macros);
            s.trigger(1.0);
            for _ in 0..(3.0 * SAMPLE_RATE) as usize {
                let v = s.tick();
                assert!(v.is_finite(), "non-finite output: {v}");
            }
        }
    }

    /// The all-macros 0.0 / 1.0 extremes above both land at RESO 0 or 1,
    /// but the stability ceiling is *worst* in between: the Chamberlin
    /// SVF pole leaves the unit circle below 12 kHz for RESO ≈ 0.1
    /// (Q ≈ 1.25). Sweep RESO through its whole travel against a full
    /// START × DEPTH sweep — the combination that actually reaches the
    /// ceiling.
    #[test]
    fn every_reso_value_stays_stable_at_full_depth() {
        let id = MachineId::SweepFx;
        let base = id.default_macros();
        for &reso in &[0.0f32, 0.05, 0.1, 0.15, 0.2, 0.25, 0.3, 0.4, 0.5, 0.7, 1.0] {
            let mut macros = base;
            macros[SLOT_FILT_1] = reso;
            macros[SLOT_MACH_2] = 1.0; // START 8 kHz
            macros[SLOT_MACH_1] = 1.0; // DEPTH 4 oct
            macros[SLOT_MACH_7] = 1.0; // HP — the highest output node
            let mut s = SweepFx::new(&macros);
            s.trigger(1.0);
            for _ in 0..(3.0 * SAMPLE_RATE) as usize {
                let v = s.tick();
                assert!(v.is_finite(), "RESO={reso} non-finite output: {v}");
            }
        }
    }
}
