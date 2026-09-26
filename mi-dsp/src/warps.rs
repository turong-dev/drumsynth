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
const MAX_BLOCK: usize = 32;

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
        #[allow(unsafe_code)]
        unsafe {
            sys::mi_warps_init(stage.storage.as_ptr(), sample_rate);
        }
        stage
    }

    /// Set the algorithm (0..1), algorithm parameter (0..1) and drive (0..1).
    pub fn set_parameters(&mut self, algorithm: f32, parameter: f32, drive: f32) {
        #[allow(unsafe_code)]
        unsafe {
            sys::mi_warps_set_parameters(self.storage.as_ptr(), algorithm, parameter, drive);
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
    use std::println;

    #[test]
    fn warps_creates() {
        let _warps = Warps::new(48000.0);
    }

    #[test]
    fn warps_sets_parameters() {
        let mut warps = Warps::new(48000.0);
        warps.set_parameters(0.3, 0.5, 0.5);
    }

    #[test]
    fn warps_processes_without_nan() {
        let mut warps = Warps::new(48000.0);
        warps.set_parameters(0.0, 0.0, 0.0);
        for size in [4usize, 5] {
            println!("processing size {}", size);
            let mut buf = [0.5f32; 32];
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
}
