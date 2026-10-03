//! Block-rate wrapper for the Mutable Instruments Warps meta-modulator.
//!
//! Warps is used as a mono processor in the mi-drum strip: the voice feeds both
//! the carrier and modulator inputs, and the combined result continues down the
//! strip.

use core::ffi::c_void;
use core::mem::MaybeUninit;

use crate::sys;

/// Maximum block size supported by the vendored Warps `Modulator`.
///
/// The vendored source reduced `kMaxBlockSize` from 96 to 32 to save memory
/// on the Teensy; the wrapper must not pass larger blocks.
pub const MAX_BLOCK: usize = 32;

#[repr(align(16))]
struct Storage(MaybeUninit<[u8; sys::MI_WARPS_STORAGE_SIZE]>);

impl Storage {
    const fn new() -> Self {
        Self(MaybeUninit::uninit())
    }

    fn as_ptr(&mut self) -> *mut c_void {
        self.0.as_mut_ptr().cast()
    }
}

/// What generates Warps' carrier.
///
/// `External` is the cross-modulator behaviour: the mono input drives both
/// Warps inputs, so the signal modulates itself. The other variants replace the
/// carrier with one of Warps' internal oscillators, pitched at
/// [`Warps::set_parameters`]'s `note` and frequency-modulated by the input —
/// which turns the same block into a small FM voice rather than an effect.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Carrier {
    /// Input cross-modulates itself. Matches the Eurorack module's default.
    External,
    /// Internal sine carrier.
    Sine,
    /// Internal triangle carrier.
    Triangle,
    /// Internal sawtooth carrier.
    Saw,
    /// Internal pulse carrier.
    Pulse,
    /// Internal band-limited-noise carrier.
    NoiseLp,
}

impl Carrier {
    /// Wire value: 0 for external, 1..=5 selecting `OscillatorShape` 0..=4.
    const fn wire(self) -> i32 {
        match self {
            Self::External => 0,
            Self::Sine => 1,
            Self::Triangle => 2,
            Self::Saw => 3,
            Self::Pulse => 4,
            Self::NoiseLp => 5,
        }
    }

    /// Every carrier, for sweeps and tests.
    pub const ALL: [Carrier; 6] = [
        Self::External,
        Self::Sine,
        Self::Triangle,
        Self::Saw,
        Self::Pulse,
        Self::NoiseLp,
    ];
}

/// Mutable Instruments Warps meta-modulator.
pub struct Warps {
    storage: Storage,
    /// Sample rate the C++ object was last initialised at, so a move can be
    /// repaired without the caller having to remember it.
    sample_rate: f32,
    /// Address `storage` had when the C++ object was constructed in it.
    ///
    /// `warps::Modulator` holds pointers into its own buffers, so moving the
    /// Rust struct leaves them pointing at the old location and the next
    /// `Process` dereferences freed or reused stack. That is a segfault that
    /// depends on stack layout, which means it hides: it survived for months
    /// of tests and then surfaced twice in one afternoon, once from adding a
    /// call frame and once from constructing in a test rather than in the
    /// engine. Comparing the address on the way into `Process` turns a
    /// latent crash into a one-off re-init.
    init_addr: usize,
}

impl Warps {
    /// Create and initialise Warps at the given sample rate.
    pub fn new(sample_rate: f32) -> Self {
        let mut stage = Self {
            storage: Storage::new(),
            sample_rate,
            init_addr: 0,
        };
        stage.init(sample_rate);
        stage
    }

    /// Re-initialise Warps in its current memory location.
    ///
    /// Use this after the struct has been moved (for example, from a stack
    /// temporary into an engine array) to fix up any self-referential C++
    /// pointers. Safe to call on a freshly constructed instance.
    pub fn init(&mut self, sample_rate: f32) {
        self.sample_rate = sample_rate;
        self.init_addr = self.storage.as_ptr() as usize;
        #[allow(unsafe_code)]
        unsafe {
            sys::mi_warps_init(self.storage.as_ptr(), sample_rate);
        }
    }

    /// Re-initialise if the struct has moved since it was constructed.
    ///
    /// One pointer compare per chunk. See [`Self::init_addr`] for why it earns
    /// its place. Re-initialising discards Warps' internal state (SRC history,
    /// feedback), which is the right trade: the alternative at that point is
    /// reading through stale pointers.
    #[inline]
    fn repair_if_moved(&mut self) {
        if self.storage.as_ptr() as usize != self.init_addr {
            let sr = self.sample_rate;
            self.init(sr);
        }
    }

    /// Set the algorithm (0..1), algorithm parameter (0..1) and drive (0..1),
    /// plus the carrier source and, for an internal carrier, its MIDI pitch.
    ///
    /// `note` is ignored when `carrier` is [`Carrier::External`].
    ///
    /// **On `drive`:** 0.0 is *not* a clean setting. Warps'
    /// `SaturatingAmplifier` computes its pre-gain as `0.5·drive` blended
    /// towards `24·drive⁵`, and post-gain as `1/SoftClip(...)` of that, so the
    /// knob's travel is:
    ///
    /// | `drive` | pre-gain | net gain | what it sounds like |
    /// |---|---|---|---|
    /// | 0.00 | 0.00 | 0.00 | **silence** |
    /// | 0.25 | 0.12 | 0.51 | quiet, gentle |
    /// | 0.50 | 0.38 | 1.07 | unity, mild colour |
    /// | 0.62 | 1.00 | 1.18 | unity, cleanest saturation point |
    /// | 0.70 | 2.16 | 2.19 | 2× overdriven |
    /// | 0.80 | 5.18 | 5.18 | hard |
    /// | 1.00 | 24.0 | 24.0 | destroyed |
    ///
    /// The bottom half of the knob is nearly linear and the top half covers
    /// 48× of gain, which is where the "gets crazy past halfway" reputation
    /// comes from — it is a property of the `drive⁵` term, not of this
    /// wrapper. Use [`Self::set_bypass`] for a genuinely clean section.
    pub fn set_parameters(
        &mut self,
        algorithm: f32,
        parameter: f32,
        drive: f32,
        carrier: Carrier,
        note: f32,
    ) {
        #[allow(unsafe_code)]
        unsafe {
            sys::mi_warps_set_parameters(
                self.storage.as_ptr(),
                algorithm,
                parameter,
                drive,
                carrier.wire(),
                note,
            );
        }
    }

    /// Route input to output unchanged, ignoring every parameter.
    ///
    /// This is the clean end of the drive axis. `drive = 0.0` mutes the voice
    /// outright (see [`Self::set_parameters`]), so a device that wants a
    /// "no colour" setting has to bypass rather than turn the knob down.
    pub fn set_bypass(&mut self, bypass: bool) {
        #[allow(unsafe_code)]
        unsafe {
            sys::mi_warps_set_bypass(self.storage.as_ptr(), bypass as i32);
        }
    }

    /// Process one block in place. `buf` is mono; it is duplicated to both
    /// Warps inputs and the left output is written back.
    ///
    /// `buf` may be any length up to [`MAX_BLOCK`]; the device engine uses
    /// this for 32-sample segments.
    pub fn process(&mut self, buf: &mut [f32]) {
        let n = buf.len();
        let mut modulator = [0.0f32; MAX_BLOCK];
        assert!(n <= MAX_BLOCK, "Warps block size cannot exceed {MAX_BLOCK}");
        modulator[..n].copy_from_slice(buf);
        let mut aux = [0.0f32; MAX_BLOCK];
        self.process_dual(buf, &modulator[..n], &mut aux[..n]);
    }

    /// Process one chunk with a **separate modulator**, writing both outputs.
    ///
    /// This is what Warps is actually built for. The module has two inputs and
    /// cross-modulates one against the other; handing it the same signal twice
    /// (which [`process`](Self::process) does) is a degenerate case where
    /// several algorithms collapse — a comparator fed two identical inputs has
    /// nothing to compare, and `ALGORITHM_XFADE` reduces to a gain of
    /// `fade_in + fade_out`, making the timbre parameter a level control.
    ///
    /// - `carrier` is input 1 and receives Warps' **main** output.
    /// - `modulator` is input 2, the signal the carrier is modulated against.
    /// - `aux_out` receives Warps' **aux** output, which in the cross-modulation
    ///   path is the sum of the two *saturated inputs* rather than a second
    ///   cross-modulation result (`modulator.cc:209`-`:222`, `:276`). It is a
    ///   drive-only tap, and it is scaled by 16384 against main's 32768, so it
    ///   arrives at half the amplitude.
    ///
    /// With an internal carrier ([`Carrier::Sine`] and friends) the roles
    /// shift: `carrier` becomes the oscillator's phase-modulation input and the
    /// carrier itself is generated internally.
    pub fn process_dual(&mut self, carrier: &mut [f32], modulator: &[f32], aux_out: &mut [f32]) {
        let n = carrier.len().min(modulator.len()).min(aux_out.len());
        assert!(n <= MAX_BLOCK, "Warps block size cannot exceed {MAX_BLOCK}");
        self.repair_if_moved();
        let mut in_l = [0.0f32; MAX_BLOCK];
        let mut in_r = [0.0f32; MAX_BLOCK];
        let mut out_l = [0.0f32; MAX_BLOCK];
        let mut out_r = [0.0f32; MAX_BLOCK];
        in_l[..n].copy_from_slice(&carrier[..n]);
        in_r[..n].copy_from_slice(&modulator[..n]);
        #[allow(unsafe_code)]
        unsafe {
            sys::mi_warps_process(
                self.storage.as_ptr(),
                in_l.as_ptr(),
                in_r.as_ptr(),
                out_l.as_mut_ptr(),
                out_r.as_mut_ptr(),
                n,
            );
        }
        carrier[..n].copy_from_slice(&out_l[..n]);
        aux_out[..n].copy_from_slice(&out_r[..n]);
    }
}

/// Shape for [`WarpsOscillator`], matching Warps' `OscillatorShape`.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Default)]
pub enum OscShape {
    /// Sine. Its modulation input is phase modulation.
    #[default]
    Sine,
    /// Triangle. Modulation input is frequency modulation.
    Triangle,
    /// Sawtooth. Modulation input is frequency modulation.
    Saw,
    /// Pulse. Modulation input is frequency modulation.
    Pulse,
    /// Band-limited noise. Modulation input ducks it.
    NoiseLp,
}

impl OscShape {
    const fn wire(self) -> i32 {
        match self {
            Self::Sine => 0,
            Self::Triangle => 1,
            Self::Saw => 2,
            Self::Pulse => 3,
            Self::NoiseLp => 4,
        }
    }

    /// All five, in macro order.
    pub const ALL: [OscShape; 5] = [
        Self::Sine,
        Self::Triangle,
        Self::Saw,
        Self::Pulse,
        Self::NoiseLp,
    ];
}

#[repr(align(8))]
struct OscStorage(MaybeUninit<[u8; sys::MI_WARPS_OSC_STORAGE_SIZE]>);

impl OscStorage {
    const fn new() -> Self {
        Self(MaybeUninit::uninit())
    }

    fn as_ptr(&mut self) -> *mut c_void {
        self.0.as_mut_ptr().cast()
    }
}

/// Warps' oscillator on its own, outside the `Modulator`.
///
/// `Modulator` can generate one of these internally and use it as the
/// **carrier**, which demotes the voice to a modulation index — the reason a
/// catalogue of 28 machines all sound like one sawtooth through an internal
/// carrier. Pulled out, the same oscillator can feed Warps' **modulator**
/// input instead, so the voice stays the carrier and the cross-modulator has
/// two genuinely different signals to work with.
///
/// Unlike [`Warps`] this is safe to move: `warps::Oscillator` is scalars and
/// an `stmlib::Svf`, with no pointers into its own storage, and `Init` assigns
/// every field.
pub struct WarpsOscillator {
    storage: OscStorage,
}

impl WarpsOscillator {
    /// Create and initialise an oscillator at the given sample rate.
    pub fn new(sample_rate: f32) -> Self {
        let mut osc = Self {
            storage: OscStorage::new(),
        };
        #[allow(unsafe_code)]
        unsafe {
            sys::mi_warps_osc_init(osc.storage.as_ptr(), sample_rate);
        }
        osc
    }

    /// Render `out.len()` samples at `shape` and MIDI `note`.
    ///
    /// `modulation` is the oscillator's own modulation input and must be at
    /// least as long as `out`. It is phase modulation for the sine, frequency
    /// modulation for the polyblep shapes, and a ducking signal for the noise.
    /// Zeros give a clean tone on every shape — `Duck` passes the internal
    /// signal through untouched when the external one is silent.
    pub fn render(&mut self, shape: OscShape, note: f32, modulation: &[f32], out: &mut [f32]) {
        let n = out.len().min(modulation.len());
        #[allow(unsafe_code)]
        unsafe {
            sys::mi_warps_osc_render(
                self.storage.as_ptr(),
                shape.wire(),
                note,
                modulation.as_ptr(),
                out.as_mut_ptr(),
                n,
            );
        }
    }
}

impl Default for Warps {
    fn default() -> Self {
        Self::new(48000.0)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::vec::Vec;

    #[test]
    fn warps_creates() {
        let _warps = Warps::new(48000.0);
    }

    #[test]
    fn warps_sets_parameters() {
        let mut warps = Warps::new(48000.0);
        warps.set_parameters(0.3, 0.5, 0.5, Carrier::External, 48.0);
    }

    /// Odd and short block sizes must be safe. The engine's segments are
    /// bounded by timed-event offsets, so Warps sees partial blocks whenever a
    /// trigger lands mid-block.
    #[test]
    fn warps_processes_without_nan_at_any_size() {
        let mut warps = Warps::new(48000.0);
        warps.set_parameters(0.0, 0.0, 0.0, Carrier::External, 48.0);
        for size in [1usize, 4, 5, 31, MAX_BLOCK] {
            let mut buf = [0.5f32; MAX_BLOCK];
            warps.process(&mut buf[..size]);
            for &s in &buf[..size] {
                assert!(
                    s.is_finite(),
                    "Warps produced non-finite sample at size {}",
                    size
                );
            }
        }
    }

    /// Process a fixed mono burst through one carrier setting and return the
    /// output, for comparing carriers against each other.
    fn render_with(carrier: Carrier) -> Vec<f32> {
        let mut warps = Warps::new(48000.0);
        // `new` initialises the C++ object at a stack temporary and then
        // returns it by value, so the self-referential pointers inside
        // `warps::Modulator` point at the old address. Re-init at the final
        // one — the same fix-up the engine does lazily in its strip. Without
        // it this faults under `-O0`, where the move is a real copy.
        warps.init(48000.0);
        warps.set_parameters(0.25, 0.5, 0.7, carrier, 48.0);
        let mut out = Vec::new();
        for block in 0..8 {
            let mut buf = [0.0f32; 32];
            for (i, s) in buf.iter_mut().enumerate() {
                let t = (block * 32 + i) as f32;
                *s = (t * 0.031).sin() * 0.7;
            }
            warps.process(&mut buf);
            out.extend_from_slice(&buf);
        }
        out
    }

    /// Every carrier shape must produce finite, audible output. The internal
    /// oscillator shapes overwrite the carrier with their own signal, so a
    /// wrong `carrier_shape` wire value would read past
    /// `OscillatorShape` and show up here.
    #[test]
    fn every_carrier_is_audible_and_finite() {
        for carrier in Carrier::ALL {
            let out = render_with(carrier);
            let peak = out.iter().fold(0.0f32, |a, &s| a.max(s.abs()));
            assert!(peak > 0.01, "{carrier:?} was silent (peak {peak})");
            assert!(
                out.iter().all(|s| s.is_finite()),
                "{carrier:?} produced a non-finite sample"
            );
        }
    }

    /// The internal oscillator carriers are genuinely different sounds, not one
    /// implementation reached five ways.
    #[test]
    fn internal_carriers_differ_from_each_other() {
        let sine = render_with(Carrier::Sine);
        for other in [
            Carrier::Triangle,
            Carrier::Saw,
            Carrier::Pulse,
            Carrier::NoiseLp,
        ] {
            let out = render_with(other);
            let delta = sine
                .iter()
                .zip(out.iter())
                .map(|(a, b)| (a - b).abs())
                .fold(0.0f32, f32::max);
            assert!(delta > 1.0e-3, "{other:?} sounds identical to Sine");
        }
    }

    /// Render a longer burst at a fixed internal-carrier pitch.
    ///
    /// A plain function rather than a closure on purpose: returning the `Vec`
    /// from a closure that also contains an FFI call miscompiles at `-O0` on
    /// this toolchain and faults on the return (it passes in release, and the
    /// firmware is always release). Keeping it a function sidesteps the
    /// artifact and reads the same.
    fn render_at_pitch(note: f32, blocks: usize) -> Vec<f32> {
        let mut warps = Warps::new(48000.0);
        // See `render_with`: re-init at the final address.
        warps.init(48000.0);
        warps.set_parameters(0.25, 0.5, 0.7, Carrier::Sine, note);
        let mut out = Vec::new();
        for block in 0..blocks {
            let mut buf = [0.0f32; 32];
            for (i, s) in buf.iter_mut().enumerate() {
                *s = ((block * 32 + i) as f32 * 0.02).sin() * 0.6;
            }
            warps.process(&mut buf);
            out.extend_from_slice(&buf);
        }
        out
    }

    /// The internal carrier is pitched by `note`, so two pitches must differ.
    /// This is what lets the strip's Warps track the voice rather than sitting
    /// on one fixed pitch.
    #[test]
    fn internal_carrier_follows_its_pitch() {
        let low = render_at_pitch(36.0, 16);
        let high = render_at_pitch(72.0, 16);
        let delta = low
            .iter()
            .zip(high.iter())
            .map(|(a, b)| (a - b).abs())
            .fold(0.0f32, f32::max);
        assert!(delta > 1.0e-3, "carrier ignored its pitch");
    }

    // ---- bandwidth and aliasing ------------------------------------------------
    //
    // "Sample rate loss" is the symptom these guard against: a 48 kHz engine whose
    // top end has quietly gone missing, which is audible as a dull, band-limited
    // character long before anyone thinks to measure it.
    //
    // Warps runs its cross-modulator at 6x (`kOversampling`, a 48-tap sinc SRC), so
    // the harmonics its saturator generates have room to exist before folding back.
    // The test is the gain *at the input frequency* — harmonics and intermod
    // products are not what is under test, bandwidth is.

    /// `sin` via a Taylor series, reduced to ±π first so the series stays
    /// accurate. This crate is `no_std` with no `libm`, and a test helper should
    /// not pull in a dependency for one transcendental.
    fn sin_f(x: f32) -> f32 {
        let two_pi = core::f32::consts::PI * 2.0;
        let mut t = x % two_pi;
        if t > core::f32::consts::PI {
            t -= two_pi;
        }
        if t < -core::f32::consts::PI {
            t += two_pi;
        }
        let t2 = t * t;
        t * (1.0 - t2 / 6.0 * (1.0 - t2 / 20.0 * (1.0 - t2 / 42.0 * (1.0 - t2 / 72.0))))
    }

    fn cos_f(x: f32) -> f32 {
        sin_f(x + core::f32::consts::FRAC_PI_2)
    }

    /// Newton's method, no transcendental needed. `no_std` has no `sqrt` either.
    fn sqrt_f(x: f32) -> f32 {
        if x <= 0.0 {
            return 0.0;
        }
        let mut r = x.max(1.0);
        for _ in 0..8 {
            r = 0.5 * (r + x / r);
        }
        r
    }

    /// Amplitude of `x` at `hz` by Goertzel. `x` must be an exact whole number
    /// of cycles at that frequency or the reading is smeared.
    fn amplitude_at(x: &[f32], hz: f32, sample_rate: f32) -> f32 {
        let wr = 2.0 * cos_f(2.0 * core::f32::consts::PI * hz / sample_rate);
        let (mut s1, mut s2) = (0.0f32, 0.0f32);
        for &v in x {
            let s0 = v + wr * s1 - s2;
            s2 = s1;
            s1 = s0;
        }
        sqrt_f((s1 * s1 + s2 * s2 - wr * s1 * s2).max(0.0)) / x.len() as f32 * 2.0
    }

    /// Run a whole signal through Warps, a block at a time.
    ///
    /// [`Warps::process`] takes at most [`MAX_BLOCK`] samples — it is the
    /// engine's per-chunk call, not a file processor — while a spectral
    /// measurement needs thousands of samples to resolve a bin. Feeding it the
    /// whole stimulus trips the assert rather than returning a bad reading.
    fn process_all(warps: &mut Warps, x: &mut [f32]) {
        for chunk in x.chunks_mut(MAX_BLOCK) {
            warps.process(chunk);
        }
    }

    /// Gain at `hz` through Warps, as a ratio. `bypass` picks the clean path.
    fn warps_gain_at(bypass: bool, drive: f32, hz: f32) -> f32 {
        let sr = 48000.0f32;
        let mut warps = Warps::new(sr);
        // `new` initialises the C++ object at a stack temporary and then
        // returns it by value, so the self-referential pointers inside
        // `warps::Modulator` point at the old address. Re-init at the final
        // one — the same fix-up the engine does lazily in its strip. Without
        // it this faults under `-O0`, where the move is a real copy.
        warps.init(sr);
        warps.set_bypass(bypass);
        warps.set_parameters(
            0.0,
            0.5,
            if bypass { 0.0 } else { drive },
            Carrier::External,
            60.0,
        );
        // A whole number of cycles makes the stimulus exactly periodic.
        let n = 4800usize;
        let cycles = (hz * n as f32 / sr).round().max(1.0) as usize;
        let tone: std::vec::Vec<f32> = (0..n)
            .map(|i| {
                sin_f((i as f32) * 2.0 * core::f32::consts::PI * cycles as f32 / n as f32) * 0.3
            })
            .collect();
        // One pass of settling, then measure the second.
        let mut warm = tone.clone();
        process_all(&mut warps, &mut warm);
        let mut out = tone.clone();
        process_all(&mut warps, &mut out);
        let got = amplitude_at(&out, cycles as f32 * sr / n as f32, sr);
        let want = amplitude_at(&tone, cycles as f32 * sr / n as f32, sr);
        if want > 0.0 {
            got / want
        } else {
            0.0
        }
    }

    /// Bypass is flat to 20 kHz, and the active path holds the band it is
    /// actually responsible for.
    ///
    /// Measured gain at the input frequency, relative to the same
    /// configuration at 1 kHz:
    ///
    /// | | 1 kHz | 5 kHz | 10 kHz | 15 kHz | 20 kHz |
    /// |---|---|---|---|---|---|
    /// | bypass | 1.00 | 1.00 | 1.00 | 1.00 | 1.00 |
    /// | drive 0.2 | 1.00 | 0.96 | 1.16 | 0.18 | 0.00 |
    /// | drive 0.6 | 1.00 | 0.93 | 0.44 | 0.07 | 0.00 |
    /// | drive 1.0 | 1.00 | 0.80 | 0.14 | 0.02 | 0.00 |
    ///
    /// So the active path **is** band-limited: it is flat to ~5 kHz, starts
    /// falling by 10 kHz, and is gone by 20 kHz. That is Warps' own 6x SRC
    /// (`kOversampling`, a 48-tap sinc) doing what it was designed to do at
    /// the rate the hardware runs, not a defect introduced by this wrapper —
    /// but it means a track with Warps engaged is darker than the same track
    /// bypassed, by a lot, and that is a mix decision rather than a subtlety.
    ///
    /// The assertions therefore cover the two things that would be bugs:
    /// bypass must be transparent, and the active path must hold the band up
    /// to 5 kHz at every drive. The rolloff above that is pinned as a
    /// measurement, not asserted as correct.
    ///
    /// The ratios are taken against the same drive's own 1 kHz gain, because
    /// `drive` is a level control as well as a colour control — an absolute
    /// threshold would fail at low drive for having turned the volume down.
    #[test]
    fn warps_holds_its_band_and_bypass_is_flat() {
        for &hz in &[1_000.0f32, 5_000.0, 10_000.0, 15_000.0, 20_000.0] {
            let g = warps_gain_at(true, 0.0, hz);
            assert!(
                (g - 1.0).abs() < 0.02,
                "bypass is not transparent at {hz} Hz: gain {g:.3}"
            );
        }

        for drive in [0.2f32, 0.6, 1.0] {
            let reference = warps_gain_at(false, drive, 1_000.0);
            assert!(reference > 0.0, "drive={drive} is silent at 1 kHz");
            let rel = warps_gain_at(false, drive, 5_000.0) / reference;
            assert!(
                rel > 0.75,
                "drive={drive}: 5 kHz is {rel:.3} of 1 kHz — the band Warps is \
                 meant to pass is being lost, not just the top octave"
            );
        }
    }

    /// Moving the struct must not fault or silence it.
    ///
    /// `warps::Modulator` holds pointers into its own buffers, so a move
    /// leaves them stale. The engine used to work around this with a
    /// `warps_initialized` flag and a lazy `init` on the first strip call;
    /// the wrapper now repairs itself, and this is what says so. Without the
    /// repair this test is a segfault, not a failure.
    #[test]
    fn surviving_a_move_is_the_wrapper_s_job() {
        let mut warps = Warps::new(48000.0);
        warps.set_parameters(0.25, 0.5, 0.7, Carrier::External, 48.0);
        let mut buf = [0.0f32; 32];
        for (i, s) in buf.iter_mut().enumerate() {
            *s = sin_f(i as f32 * 0.2) * 0.5;
        }
        warps.process(&mut buf);

        // Move it: onto the heap, which is certainly a different address.
        let mut moved = std::boxed::Box::new(warps);
        let mut after = [0.0f32; 32];
        for (i, s) in after.iter_mut().enumerate() {
            *s = sin_f(i as f32 * 0.2) * 0.5;
        }
        moved.process(&mut after);

        assert!(
            after.iter().all(|s| s.is_finite()),
            "non-finite output after a move"
        );
        let peak = after.iter().fold(0.0f32, |a, &s| a.max(s.abs()));
        assert!(peak > 1.0e-4, "silent after a move (peak {peak})");
    }

    /// Every shape must produce a clean, audible tone from a silent
    /// modulation input, and the shapes must differ from each other.
    ///
    /// The silent-input case is what the strip relies on: as a modulator
    /// source the oscillator wants to be a defined tone, not something the
    /// voice is smearing. `Duck` makes that true for the noise shape too —
    /// given a silent external input it passes the internal signal through
    /// rather than gating it.
    ///
    /// Measured over a long window. A single 64-sample block at these
    /// frequencies is a fraction of one cycle, so a pulse caught in its low
    /// state reads as silence.
    ///
    /// The shapes are **not** level-matched: measured peaks are sine 1.00,
    /// triangle 1.97, saw 0.99, pulse 0.89, noise 0.21. That is Warps' own
    /// characteristic — `Modulator` compensates with a 0.5 gain on the
    /// internal-carrier path, which the strip mirrors.
    #[test]
    fn standalone_oscillator_runs_clean_on_every_shape() {
        let silence = [0.0f32; 64];
        let mut rendered: Vec<Vec<f32>> = Vec::new();
        for shape in OscShape::ALL {
            let mut osc = WarpsOscillator::new(48000.0);
            let mut out = [0.0f32; 64];
            let mut captured: Vec<f32> = Vec::new();
            let mut peak = 0.0f32;
            for block in 0..200 {
                osc.render(shape, 48.0, &silence, &mut out);
                assert!(
                    out.iter().all(|s| s.is_finite()),
                    "{shape:?} produced a non-finite sample"
                );
                if block >= 4 {
                    peak = out.iter().fold(peak, |a, &s| a.max(s.abs()));
                    captured.extend_from_slice(&out);
                }
            }
            assert!(peak > 0.05, "{shape:?} was silent (peak {peak})");
            rendered.push(captured);
        }
        for i in 1..rendered.len() {
            let delta = rendered[0]
                .iter()
                .zip(rendered[i].iter())
                .map(|(a, b)| (a - b).abs())
                .fold(0.0f32, f32::max);
            assert!(
                delta > 1.0e-3,
                "{:?} is indistinguishable from Sine",
                OscShape::ALL[i]
            );
        }
    }

    /// The oscillator is pitched by `note`, which is what lets the strip
    /// track the voice.
    ///
    /// The ratio is asserted rather than the absolute pitch, because Warps'
    /// `midi_to_increment` is not MIDI: measured, `note` 48 renders ~64 Hz
    /// and `note` 60 ~129 Hz, so the scale sits an octave below MIDI. The
    /// strip passes the voice's note through unchanged, which keeps the
    /// modulator an octave under the voice — the same offset the internal
    /// carrier has always had.
    #[test]
    fn standalone_oscillator_follows_its_note() {
        let silence = [0.0f32; 64];
        let crossings = |note: f32| {
            let mut osc = WarpsOscillator::new(48000.0);
            let mut out = [0.0f32; 64];
            let mut total = 0u32;
            let mut last = 0.0f32;
            for _ in 0..200 {
                osc.render(OscShape::Sine, note, &silence, &mut out);
                for &s in out.iter() {
                    if (last < 0.0) != (s < 0.0) {
                        total += 1;
                    }
                    last = s;
                }
            }
            total
        };
        let low = crossings(48.0);
        let high = crossings(60.0);
        assert!(low > 20, "no tone at note 48 ({low} crossings)");
        let ratio = high as f32 / low as f32;
        assert!(
            (1.8..2.2).contains(&ratio),
            "an octave should double the rate: {low} -> {high} (x{ratio:.2})"
        );
    }

    /// A tone near the top of the band must not reappear somewhere it should not.
    /// With a cross-modulator generating products, an under-oversampled path folds
    /// them back into the audible band; 6x oversampling plus the 48-tap sinc SRC is
    /// what prevents it.
    #[test]
    fn no_alias_image_of_a_high_tone() {
        let sr = 48000.0f32;
        let mut warps = Warps::new(sr);
        // See `warps_gain_at`: re-init at the final address.
        warps.init(sr);
        warps.set_parameters(0.0, 0.5, 1.0, Carrier::External, 60.0);
        let hz = 12_000.0f32;
        let n = 4800usize;
        let cycles = (hz * n as f32 / sr).round() as usize;
        let tone: std::vec::Vec<f32> = (0..n)
            .map(|i| {
                sin_f((i as f32) * 2.0 * core::f32::consts::PI * cycles as f32 / n as f32) * 0.5
            })
            .collect();
        let mut warm = tone.clone();
        process_all(&mut warps, &mut warm);
        let mut out = tone;
        process_all(&mut warps, &mut out);

        // The fundamental's own harmonics are legitimate. What must not be there
        // is energy in a band the stimulus never occupied, which is what folding
        // looks like from the outside.
        let fundamental = amplitude_at(&out, hz, sr);
        let stray = [18_000.0f32, 20_000.0, 22_000.0]
            .iter()
            .map(|&f| amplitude_at(&out, f, sr))
            .fold(0.0f32, f32::max);
        assert!(
            stray < fundamental * 0.05,
            "energy at 18-22 kHz ({stray:.5}) is close to the fundamental \
             ({fundamental:.5}) — something is folding back into the band"
        );
    }
}
