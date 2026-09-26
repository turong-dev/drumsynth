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
}

impl Warps {
    /// Create and initialise Warps at the given sample rate.
    pub fn new(sample_rate: f32) -> Self {
        let mut stage = Self {
            storage: Storage::new(),
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
        #[allow(unsafe_code)]
        unsafe {
            sys::mi_warps_init(self.storage.as_ptr(), sample_rate);
        }
    }

    /// Set the algorithm (0..1), algorithm parameter (0..1) and drive (0..1),
    /// plus the carrier source and, for an internal carrier, its MIDI pitch.
    ///
    /// `note` is ignored when `carrier` is [`Carrier::External`].
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

    /// Process one block in place. `buf` is mono; it is duplicated to both
    /// Warps inputs and the left output is written back.
    ///
    /// `buf` may be any length up to [`MAX_BLOCK`]; the device engine uses
    /// this for 32-sample segments.
    pub fn process(&mut self, buf: &mut [f32]) {
        let n = buf.len();
        assert!(n <= MAX_BLOCK, "Warps block size cannot exceed {MAX_BLOCK}");
        let mut in_l = [0.0f32; MAX_BLOCK];
        let mut out_l = [0.0f32; MAX_BLOCK];
        let mut out_r = [0.0f32; MAX_BLOCK];
        in_l[..n].copy_from_slice(buf);
        #[allow(unsafe_code)]
        unsafe {
            sys::mi_warps_process(
                self.storage.as_ptr(),
                in_l.as_ptr(),
                in_l.as_ptr(),
                out_l.as_mut_ptr(),
                out_r.as_mut_ptr(),
                n,
            );
        }
        buf.copy_from_slice(&out_l[..n]);
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
}
