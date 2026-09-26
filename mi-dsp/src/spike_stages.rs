//! Block-rate processing stages from the Mutable Instruments source.
//!
//! Phase 14 replaces each stage of the mi-drum chain with an MI alternative.
//! These are the wrappers for the first three, chosen to bracket the cost
//! range rather than to be a complete catalog: the low-pass gate (the stage
//! that gives Plaits its character), overdrive (about as cheap as a stage
//! gets), and the modal resonator (about as expensive).
//!
//! Same FFI rule as [`crate::plaits`]: one call per stage per block, never per
//! sample. Each wrapper owns aligned storage that the C++ object is
//! placement-new'd into, so nothing allocates and the struct can be moved
//! after construction.

use core::ffi::c_void;
use core::mem::MaybeUninit;

use crate::sys;

macro_rules! stage_storage {
    ($name:ident, $size:expr) => {
        #[repr(align(8))]
        struct $name(MaybeUninit<[u8; $size]>);

        impl $name {
            const fn new() -> Self {
                Self(MaybeUninit::uninit())
            }

            fn as_ptr(&mut self) -> *mut c_void {
                self.0.as_mut_ptr().cast()
            }
        }
    };
}

stage_storage!(LpgStorage, 64);
stage_storage!(OverdriveStorage, 16);
stage_storage!(ResonatorStorage, 2048);

/// Buchla-style low-pass gate: a vactrol-modelled envelope driving a combined
/// VCA and low-pass filter.
///
/// This is the pair Plaits uses internally on its own voices, and the reason a
/// Plaits patch decays the way it does rather than the way a linear VCA would.
/// Exposing it as a *stage* is the thing Plaits itself cannot do: there, the
/// LPG is welded to the engine. Here any machine can be put through it.
pub struct Lpg {
    storage: LpgStorage,
}

impl Lpg {
    /// Create and initialise a low-pass gate.
    pub fn new() -> Self {
        let mut stage = Self {
            storage: LpgStorage::new(),
        };
        // SAFETY: storage is correctly sized and aligned for the C++ object;
        // the C side placement-news into it. Sizes are static_assert'd in the
        // shim, so a vendored change that outgrows them fails the build.
        #[allow(unsafe_code)]
        unsafe {
            sys::mi_lpg_init(stage.storage.as_ptr());
        }
        stage
    }

    /// Arm the envelope's attack ramp. Call on note-on.
    pub fn trigger(&mut self) {
        #[allow(unsafe_code)]
        unsafe {
            sys::mi_lpg_trigger(self.storage.as_ptr());
        }
    }

    /// Advance the envelope one block and apply the gate to `buf` in place.
    ///
    /// `attack` is the vactrol ramp rate, `short_decay` and `decay_tail` the
    /// two halves of its asymmetric decay, `hf` the high-frequency bleed that
    /// keeps the gate from sounding like a plain low-pass.
    pub fn process(
        &mut self,
        attack: f32,
        short_decay: f32,
        decay_tail: f32,
        hf: f32,
        buf: &mut [f32],
    ) {
        #[allow(unsafe_code)]
        unsafe {
            sys::mi_lpg_process(
                self.storage.as_ptr(),
                attack,
                short_decay,
                decay_tail,
                hf,
                buf.as_mut_ptr(),
                buf.len(),
            );
        }
    }
}

impl Default for Lpg {
    fn default() -> Self {
        Self::new()
    }
}

/// Soft-clip overdrive with output-ceiling compensation.
///
/// The drive parameter raises pre-gain and lowers post-gain together, which
/// keeps the *ceiling* roughly constant as saturation increases — it does not
/// keep perceived level constant for a given input. Measured, 16 blocks in:
///
/// ```text
///   input   d=0.0   d=0.2   d=0.4   d=0.6   d=0.8   d=1.0
///    0.50   0.000   0.199   0.419   0.618   0.999   1.000
///    1.00   0.000   0.398   0.829   1.074   1.000   1.000
/// ```
///
/// Two things to design around, both of which caught a first attempt at
/// specifying this as a stage:
///
/// 1. **`drive = 0.0` is silence, not dry.** A stage selector whose "off"
///    position means "unchanged" cannot simply pass 0 through to this — it has
///    to bypass the stage entirely, or map its own 0 to a unity-ish drive.
/// 2. It can exceed unity in the middle of the range (1.074 at `d = 0.6` on a
///    full-scale input), so it is not a limiter and does not replace the
///    engine's master clip.
///
/// This is still the right stage for a gain-compensated "DIRT" macro — the
/// compensation is what stops a drive sweep from being a volume sweep at the
/// top of the range — but the mapping has to be built, not assumed.
pub struct Overdrive {
    storage: OverdriveStorage,
}

impl Overdrive {
    /// Create and initialise an overdrive.
    pub fn new() -> Self {
        let mut stage = Self {
            storage: OverdriveStorage::new(),
        };
        // SAFETY: see `Lpg::new`.
        #[allow(unsafe_code)]
        unsafe {
            sys::mi_overdrive_init(stage.storage.as_ptr());
        }
        stage
    }

    /// Apply overdrive to `buf` in place. `drive` is 0..1.
    pub fn process(&mut self, drive: f32, buf: &mut [f32]) {
        #[allow(unsafe_code)]
        unsafe {
            sys::mi_overdrive_process(self.storage.as_ptr(), drive, buf.as_mut_ptr(), buf.len());
        }
    }
}

impl Default for Overdrive {
    fn default() -> Self {
        Self::new()
    }
}

/// A 24-mode modal resonator bank — the Rings/Elements body model.
///
/// As a stage this stops being a filter in the usual sense: the input excites
/// a set of tuned modes, so what comes out is the *object's* pitch and decay
/// rather than the input's, coloured by what hit it. A noise burst becomes a
/// struck bar; a click becomes a bell.
pub struct Resonator {
    storage: ResonatorStorage,
}

impl Resonator {
    /// Create and initialise a resonator.
    ///
    /// `position` is where the object is struck (0..1), which sets which modes
    /// are excited. `resolution` caps how many modes run — the cost is close
    /// to linear in it, so it is the dial to turn when the budget says no.
    pub fn new(position: f32, resolution: i32) -> Self {
        let mut stage = Self {
            storage: ResonatorStorage::new(),
        };
        // SAFETY: see `Lpg::new`.
        #[allow(unsafe_code)]
        unsafe {
            sys::mi_resonator_init(stage.storage.as_ptr(), position, resolution);
        }
        stage
    }

    /// Excite the resonator with `input`, writing the result to `output`.
    ///
    /// Not in place: the resonator is not a filter applied to a signal so much
    /// as an object driven by one, and the C++ signature reflects that.
    pub fn process(
        &mut self,
        f0: f32,
        structure: f32,
        brightness: f32,
        damping: f32,
        input: &[f32],
        output: &mut [f32],
    ) {
        let n = input.len().min(output.len());
        #[allow(unsafe_code)]
        unsafe {
            sys::mi_resonator_process(
                self.storage.as_ptr(),
                f0,
                structure,
                brightness,
                damping,
                input.as_ptr(),
                output.as_mut_ptr(),
                n,
            );
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn lpg_gates_a_constant_toward_silence() {
        let mut lpg = Lpg::new();
        let mut open = [1.0f32; 24];
        lpg.trigger();
        lpg.process(0.1, 0.5, 0.5, 0.1, &mut open);
        let opened: f32 = open.iter().map(|s| s.abs()).sum();

        // Without a retrigger the vactrol decays; after many blocks the same
        // input should come through markedly quieter.
        let mut closed = [1.0f32; 24];
        for _ in 0..200 {
            closed = [1.0f32; 24];
            lpg.process(0.1, 0.5, 0.5, 0.1, &mut closed);
        }
        let shut: f32 = closed.iter().map(|s| s.abs()).sum();
        assert!(shut < opened, "gate did not close: {shut} vs {opened}");
    }

    /// Pins the two facts a stage selector has to design around: zero drive
    /// mutes rather than passing dry, and the ceiling stops rising near the
    /// top of the range rather than climbing with drive.
    #[test]
    fn overdrive_mutes_at_zero_and_ceilings_out() {
        fn peak_at(drive: f32, input: f32) -> f32 {
            let mut od = Overdrive::new();
            let mut buf = [input; 24];
            // Several blocks so the parameter interpolators settle.
            for _ in 0..16 {
                buf = [input; 24];
                od.process(drive, &mut buf);
            }
            buf.iter().fold(0.0f32, |m, s| m.max(s.abs()))
        }

        assert_eq!(
            peak_at(0.0, 0.5),
            0.0,
            "zero drive should mute, not pass dry"
        );

        // Monotonic in drive over the lower range...
        assert!(peak_at(0.2, 0.5) < peak_at(0.6, 0.5));
        // ...and compensated at the top: a full-scale input lands at unity for
        // both 0.8 and 1.0 rather than climbing further.
        let hot_high = peak_at(0.8, 1.0);
        let hot_max = peak_at(1.0, 1.0);
        assert!((hot_high - hot_max).abs() < 0.05, "{hot_high} vs {hot_max}");
        assert!(hot_max <= 1.01);
    }

    #[test]
    fn resonator_rings_after_an_impulse() {
        let mut res = Resonator::new(0.3, 24);
        let mut input = [0.0f32; 24];
        input[0] = 1.0;
        let mut output = [0.0f32; 24];
        res.process(0.01, 0.3, 0.5, 0.3, &input, &mut output);

        // Keep exciting with silence; a resonator should still be sounding.
        let silence = [0.0f32; 24];
        let mut tail = [0.0f32; 24];
        res.process(0.01, 0.3, 0.5, 0.3, &silence, &mut tail);
        let energy: f32 = tail.iter().map(|s| s.abs()).sum();
        assert!(energy > 0.0, "resonator did not ring");
        assert!(energy.is_finite());
    }
}
