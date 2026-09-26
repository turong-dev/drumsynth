//! Block-rate wrapper for the Mutable Instruments Clouds texture synthesizer.
//!
//! Clouds lives on the shared send FX bus. It needs two externally supplied
//! buffers: a large buffer (~118 KB) and a small buffer (~65 KB).

use core::ffi::c_void;
use core::mem::MaybeUninit;

use crate::sys;

#[repr(align(8))]
struct Storage(MaybeUninit<[u8; sys::MI_CLOUDS_STORAGE_SIZE]>);

impl Storage {
    const fn new() -> Self {
        Self(MaybeUninit::uninit())
    }

    fn as_ptr(&mut self) -> *mut c_void {
        self.0.as_mut_ptr().cast()
    }
}

/// Suggested large buffer size for Clouds, taken from the upstream test code.
pub const LARGE_BUFFER_SIZE: usize = 118784;
/// Suggested small buffer size for Clouds, taken from the upstream test code.
pub const SMALL_BUFFER_SIZE: usize = 65536;

/// Mutable Instruments Clouds texture synthesizer.
pub struct Clouds {
    storage: Storage,
}

impl Clouds {
    /// Create and initialise Clouds with the required external buffers.
    pub fn new(large_buffer: &mut [u8], small_buffer: &mut [u8]) -> Self {
        let mut stage = Self {
            storage: Storage::new(),
        };
        #[allow(unsafe_code)]
        unsafe {
            sys::mi_clouds_init(
                stage.storage.as_ptr(),
                large_buffer.as_mut_ptr().cast(),
                large_buffer.len(),
                small_buffer.as_mut_ptr().cast(),
                small_buffer.len(),
            );
        }
        stage
    }

    /// Prepare after a parameter change that affects buffer layout.
    pub fn prepare(&mut self) {
        #[allow(unsafe_code)]
        unsafe {
            sys::mi_clouds_prepare(self.storage.as_ptr());
        }
    }

    /// Set Clouds parameters.
    #[allow(clippy::too_many_arguments)]
    pub fn set_parameters(
        &mut self,
        position: f32,
        size: f32,
        pitch: f32,
        density: f32,
        texture: f32,
        dry_wet: f32,
        stereo_spread: f32,
        feedback: f32,
        reverb: f32,
        freeze: bool,
        trigger: bool,
        gate: bool,
    ) {
        #[allow(unsafe_code)]
        unsafe {
            sys::mi_clouds_set_parameters(
                self.storage.as_ptr(),
                position,
                size,
                pitch,
                density,
                texture,
                dry_wet,
                stereo_spread,
                feedback,
                reverb,
                freeze as i32,
                trigger as i32,
                gate as i32,
            );
        }
    }

    /// Process one block. Input and output are mono; the same signal is sent
    /// to both channels and the left output is returned.
    pub fn process(&mut self, buf: &mut [f32]) {
        let n = buf.len();
        assert!(n <= 32, "Clouds block size cannot exceed 32");
        let mut in_l = [0.0f32; 32];
        let mut out_l = [0.0f32; 32];
        let mut out_r = [0.0f32; 32];
        in_l[..n].copy_from_slice(buf);
        #[allow(unsafe_code)]
        unsafe {
            sys::mi_clouds_process(
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn clouds_processes_without_nan() {
        let mut large = [0u8; LARGE_BUFFER_SIZE];
        let mut small = [0u8; SMALL_BUFFER_SIZE];
        let mut clouds = Clouds::new(&mut large, &mut small);
        clouds.set_parameters(
            0.5, 0.5, 0.0, 0.5, 0.5, 0.5, 0.5, 0.0, 0.0, false, true, false,
        );
        clouds.prepare();
        let mut buf = [0.5f32; 32];
        clouds.process(&mut buf);
        for &s in &buf {
            assert!(s.is_finite(), "Clouds produced non-finite sample");
        }
    }
}
