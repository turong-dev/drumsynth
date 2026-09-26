//! Block-rate wrapper for the Mutable Instruments Stages segment generator.
//!
//! Stages is used as a modulation source: one instance configured as 6 LFOs,
//! another as 3 AD envelopes.

use core::ffi::c_void;
use core::mem::MaybeUninit;

use crate::sys;

#[repr(align(8))]
struct Storage(MaybeUninit<[u8; sys::MI_STAGES_STORAGE_SIZE]>);

impl Storage {
    const fn new() -> Self {
        Self(MaybeUninit::uninit())
    }

    fn as_ptr(&mut self) -> *mut c_void {
        self.0.as_mut_ptr().cast()
    }
}

/// Segment types for `Stages::configure_single`.
pub const SEGMENT_RAMP: i32 = sys::MI_STAGES_SEGMENT_RAMP as i32;
/// Step segment type.
pub const SEGMENT_STEP: i32 = sys::MI_STAGES_SEGMENT_STEP as i32;
/// Hold segment type.
pub const SEGMENT_HOLD: i32 = sys::MI_STAGES_SEGMENT_HOLD as i32;
/// Alt segment type (oscillator/LFO).
pub const SEGMENT_ALT: i32 = sys::MI_STAGES_SEGMENT_ALT as i32;

/// Mutable Instruments Stages segment generator.
pub struct Stages {
    storage: Storage,
}

impl Stages {
    /// Create and initialise a Stages segment generator.
    pub fn new() -> Self {
        let mut stage = Self {
            storage: Storage::new(),
        };
        #[allow(unsafe_code)]
        unsafe {
            sys::mi_stages_init(stage.storage.as_ptr());
        }
        stage
    }

    /// Configure as a single segment.
    ///
    /// `segment_type` is one of the `SEGMENT_*` constants. `loop` and
    /// `has_trigger` are booleans. `primary` and `secondary` are the segment
    /// parameters (meaning depends on segment type).
    pub fn configure_single(
        &mut self,
        segment_type: i32,
        loop_: bool,
        has_trigger: bool,
        primary: f32,
        secondary: f32,
    ) {
        #[allow(unsafe_code)]
        unsafe {
            sys::mi_stages_configure_single(
                self.storage.as_ptr(),
                segment_type,
                loop_ as i32,
                has_trigger as i32,
                primary,
                secondary,
            );
        }
    }

    /// Configure as a two-segment AD envelope.
    pub fn configure_ad(&mut self, attack: f32, decay: f32) {
        #[allow(unsafe_code)]
        unsafe {
            sys::mi_stages_configure_ad(self.storage.as_ptr(), attack, decay);
        }
    }

    /// Trigger the envelope. LFO modes ignore this.
    pub fn trigger(&mut self) {
        #[allow(unsafe_code)]
        unsafe {
            sys::mi_stages_trigger(self.storage.as_ptr());
        }
    }

    /// Process one block, writing the generated modulation into `out`.
    pub fn process(&mut self, gate_flags: &[u8], out: &mut [f32]) {
        let n = gate_flags.len().min(out.len());
        assert!(n <= 96, "Stages block size cannot exceed 96");
        #[allow(unsafe_code)]
        unsafe {
            sys::mi_stages_process(
                self.storage.as_ptr(),
                gate_flags.as_ptr(),
                out.as_mut_ptr(),
                n,
            );
        }
    }
}

impl Default for Stages {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn stages_lfo_processes_without_nan() {
        let mut stages = Stages::new();
        stages.configure_single(SEGMENT_RAMP, true, false, 0.5, 0.5);
        let mut out = [0.0f32; 32];
        let gate = [0u8; 32];
        stages.process(&gate, &mut out);
        for &s in &out {
            assert!(s.is_finite(), "Stages produced non-finite sample");
        }
    }

    #[test]
    fn stages_ad_processes_without_nan() {
        let mut stages = Stages::new();
        stages.configure_ad(0.1, 0.3);
        stages.trigger();
        let mut out = [0.0f32; 32];
        let mut gate = [0u8; 32];
        gate[0] = 1;
        stages.process(&gate, &mut out);
        for &s in &out {
            assert!(s.is_finite(), "Stages AD produced non-finite sample");
        }
    }
}
