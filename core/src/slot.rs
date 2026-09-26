//! Generic voice-slot surface.
//!
//! A [`Slot`] is the per-track synthesis model: the thing that makes sound
//! when triggered. [`Track`](crate::track::Track) is generic over it so the
//! same strip, mixing, modulation and CC-macro framework can host drum
//! machines, FM voices, or any other no-alloc voice on different devices.

use crate::macros::MacroInfo;
use core::fmt::Debug;

/// Identifier for a slot that can be selected per track.
///
/// `N` is the device's macro count so the identifier can vend a default macro
/// array of the right size.
pub trait SlotId<const N: usize>: Copy + Eq + Debug {
    /// Stable catalogue index, `0..count()`.
    fn index(self) -> usize;

    /// Look up an identifier from its catalogue index, if in range.
    fn from_index(i: usize) -> Option<Self>;

    /// Number of identifiers in the device's catalogue.
    fn count() -> usize;

    /// Factory macro values for this identifier.
    fn default_macros(self) -> [f32; N];
}

/// Display and control metadata for a slot identifier.
///
/// This is the narrow surface the grid UI needs from a device: macro names,
/// abbreviations, defaults, and a human-readable label per machine. It extends
/// [`SlotId`] so a device can vend both identity and metadata from the same
/// type.
pub trait DeviceModel<const N: usize>: SlotId<N> {
    /// Per-macro metadata for the machine identified by `self`.
    fn macro_info(self) -> [MacroInfo; N];

    /// Human-readable label for the machine identified by `self`.
    fn label(self) -> &'static str;
}

/// A single voice slot that [`Track`](crate::track::Track) can host.
///
/// `N` is the number of normalised macros the slot understands. This is a
/// const generic rather than an associated const because array sizes must be
/// known at compile time.
pub trait Slot<const N: usize>: Sized {
    /// Device-specific identifier for this slot.
    type Id: SlotId<N>;

    /// Build a slot from its identifier and initial macros.
    fn new(id: Self::Id, macros: &[f32; N]) -> Self;

    /// Build a slot directly in the memory pointed to by `ptr`.
    ///
    /// The default implementation simply calls [`Self::new`] and writes the
    /// result. Slots that own self-referential state (e.g. C++ objects with
    /// internal pointers) must override this to avoid dangling pointers after
    /// a stack-to-heap move.
    ///
    /// # Safety
    ///
    /// `ptr` must be valid for writes and properly aligned for `Self`.
    #[allow(unsafe_code)]
    unsafe fn new_in_place(id: Self::Id, macros: &[f32; N], ptr: *mut Self) {
        ptr.write(Self::new(id, macros));
    }

    /// Which identifier this slot was built from.
    fn id(&self) -> Self::Id;

    /// Recompute coefficients from macros. Setup/control rate.
    fn set_macros(&mut self, macros: &[f32; N]);

    /// Begin a hit at `velocity` (0..=1.0).
    fn trigger(&mut self, velocity: f32);

    /// Transpose the voice by `semis` semitones relative to its macro pitch.
    ///
    /// Devices without a pitch concept may no-op. Absolute, not incremental.
    fn retune(&mut self, semis: f32);

    /// Force to silence.
    fn reset(&mut self);

    /// Still producing output?
    fn is_active(&self) -> bool;

    /// One sample of output, pre-strip.
    fn tick(&mut self) -> f32;

    /// Render one contiguous segment of samples into `out`.
    ///
    /// The default implementation calls [`tick`](Self::tick) for each sample,
    /// which is correct for any per-sample voice. Block-rate devices can
    /// override this to render the segment in one call and keep the strip
    /// processing block-aligned.
    ///
    /// `out` is a mono buffer; pan, level and sends are applied by the
    /// containing [`Track`](crate::track::Track).
    fn process_segment(&mut self, out: &mut [f32]) {
        for s in out.iter_mut() {
            *s = self.tick();
        }
    }

    /// Apply a device-specific audio strip to a collected source segment.
    ///
    /// The engine collects raw source samples into `Track::source_segment` via
    /// [`tick`](Self::tick), then calls this hook so devices with block-rate
    /// strip modules (e.g. mi-drum's Warps → Ripples) can process the segment
    /// before pan, level and sends are applied. The default implementation is
    /// a no-op.
    fn process_audio_strip(&mut self, _buf: &mut [f32]) {}
}
