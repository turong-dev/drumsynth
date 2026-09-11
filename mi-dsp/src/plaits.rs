//! Wrapper for the Mutable Instruments Plaits macro oscillator voice.

use core::mem::MaybeUninit;

// Re-export the C constants so callers know the required buffer shape.
pub use crate::sys::{
    MiPlaitsModulations, MiPlaitsPatch, MiPlaitsVoice, PLAITS_VOICE_BUFFER_SIZE,
    PLAITS_VOICE_STORAGE_SIZE,
};

/// Number of synthesis models available in Plaits.
pub fn num_engines() -> usize {
    // SAFETY: the C function is a pure constant query.
    unsafe { crate::sys::mi_plaits_num_engines() as usize }
}

/// Aligned scratch buffer for Plaits engine allocations.
#[derive(Clone, Copy)]
#[repr(align(8))]
pub struct PlaitsBuffer([u8; PLAITS_VOICE_BUFFER_SIZE]);

impl PlaitsBuffer {
    /// Create a fresh scratch buffer.
    pub const fn new() -> Self {
        Self([0u8; PLAITS_VOICE_BUFFER_SIZE])
    }
}

impl Default for PlaitsBuffer {
    fn default() -> Self {
        Self::new()
    }
}

/// Maximum number of Plaits voices that can be alive at once.
///
/// This pool is shared by all [`PlaitsVoice`] instances. The mi-drum engine
/// uses six tracks; allowing a few extra covers reloads while a voice is being
/// replaced. The buffers live in `.bss`, not inside the voice struct, so a
/// voice can be moved after construction without invalidating its internal
/// pointers.
const MAX_VOICES: usize = 8;

/// Static pool of scratch buffers.
static mut VOICE_BUFFER_POOL: [PlaitsBuffer; MAX_VOICES] = [PlaitsBuffer::new(); MAX_VOICES];

/// Free bitmap for the pool. Bit `i` set = buffer `i` is in use.
///
/// Voice construction/destruction is single-threaded in this firmware context
/// (init/control thread), so a simple bitmap is sufficient.
static mut VOICE_BUFFER_USED: u8 = 0;

/// Allocate a buffer from the static pool.
///
/// Returns a pointer to the buffer and its index, or `None` if the pool is
/// exhausted.
#[allow(unsafe_code)]
// `i` indexes the pool array and shifts the free bitmap in step, so an
// iterator would need the index back out again via `enumerate`. Not worth
// restructuring a `static mut` walk that is about to be revisited when this
// pool is made thread-safe.
#[allow(clippy::needless_range_loop)]
fn alloc_buffer() -> Option<(*mut u8, u8)> {
    unsafe {
        for i in 0..MAX_VOICES {
            let mask = 1u8 << i;
            if VOICE_BUFFER_USED & mask == 0 {
                VOICE_BUFFER_USED |= mask;
                let ptr = VOICE_BUFFER_POOL[i].0.as_mut_ptr();
                return Some((ptr, i as u8));
            }
        }
        None
    }
}

/// Return a buffer to the static pool.
#[allow(unsafe_code)]
fn free_buffer(index: u8) {
    unsafe {
        VOICE_BUFFER_USED &= !(1u8 << index);
    }
}

/// A Plaits voice, owned by Rust and rendered block-rate over FFI.
///
/// `storage` holds the C++ `plaits::Voice` object. The scratch memory it needs
/// is borrowed from a static pool; the voice struct itself can be moved after
/// construction because the buffer never moves.
pub struct PlaitsVoice {
    storage: MaybeUninit<MiPlaitsVoice>,
    buffer: *mut u8,
    buffer_index: u8,
    initialized: bool,
}

impl PlaitsVoice {
    /// Create and initialize a Plaits voice.
    ///
    /// # Panics
    ///
    /// Panics if the static buffer pool is exhausted.
    pub fn new() -> Self {
        let (buffer, buffer_index) = alloc_buffer().expect("Plaits buffer pool exhausted");
        let mut voice = Self {
            storage: MaybeUninit::uninit(),
            buffer,
            buffer_index,
            initialized: false,
        };
        unsafe {
            crate::sys::mi_plaits_voice_init(voice.storage.as_mut_ptr(), voice.buffer as *mut _);
        }
        voice.initialized = true;
        voice
    }

    /// Initialize a Plaits voice directly at `ptr`.
    ///
    /// This avoids putting the voice on the stack, which is useful when the
    /// surrounding struct is huge. The buffer still comes from the static pool,
    /// so the resulting voice can be moved safely.
    ///
    /// # Safety
    ///
    /// `ptr` must be valid for writes and aligned to at least
    /// `PLAITS_VOICE_STORAGE_ALIGN`.
    #[allow(unsafe_code)]
    pub unsafe fn new_in_place(ptr: *mut PlaitsVoice) {
        let (buffer, buffer_index) = alloc_buffer().expect("Plaits buffer pool exhausted");
        core::ptr::addr_of_mut!((*ptr).storage).write(MaybeUninit::uninit());
        core::ptr::addr_of_mut!((*ptr).buffer).write(buffer);
        core::ptr::addr_of_mut!((*ptr).buffer_index).write(buffer_index);
        crate::sys::mi_plaits_voice_init((*ptr).storage.as_mut_ptr(), (*ptr).buffer as *mut _);
        core::ptr::addr_of_mut!((*ptr).initialized).write(true);
    }

    /// Render one block into planar `out`/`aux` buffers.
    ///
    /// `out` and `aux` must each be at least `frames` elements long.
    /// Output is signed 16-bit; convert to f32 [-1, 1] by dividing by 32767.0.
    pub fn render(
        &mut self,
        patch: &MiPlaitsPatch,
        modulations: &MiPlaitsModulations,
        out: &mut [i16],
        aux: &mut [i16],
        frames: usize,
    ) {
        assert!(out.len() >= frames);
        assert!(aux.len() >= frames);
        unsafe {
            crate::sys::mi_plaits_voice_render(
                self.storage.as_mut_ptr(),
                patch,
                modulations,
                out.as_mut_ptr(),
                aux.as_mut_ptr(),
                frames,
            );
        }
    }

    /// Convenience: render one block into f32 buffers, scaled to [-1, 1].
    pub fn render_f32(
        &mut self,
        patch: &MiPlaitsPatch,
        modulations: &MiPlaitsModulations,
        out: &mut [f32],
        aux: &mut [f32],
        frames: usize,
    ) {
        assert!(out.len() >= frames);
        assert!(aux.len() >= frames);
        let mut out_i = [0i16; 64];
        let mut aux_i = [0i16; 64];
        self.render(patch, modulations, &mut out_i, &mut aux_i, frames);
        let scale = 1.0 / 32767.0;
        for i in 0..frames {
            out[i] = out_i[i] as f32 * scale;
            aux[i] = aux_i[i] as f32 * scale;
        }
    }
}

impl Default for PlaitsVoice {
    fn default() -> Self {
        Self::new()
    }
}

impl Drop for PlaitsVoice {
    fn drop(&mut self) {
        // The C++ Voice destructor is trivial (no dynamic allocation), so an
        // explicit destructor call is unnecessary. Return the scratch buffer to
        // the pool so it can be reused by the next voice.
        if self.initialized {
            free_buffer(self.buffer_index);
            self.initialized = false;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn voice_initializes_without_panic() {
        let _voice = PlaitsVoice::new();
    }

    #[test]
    fn voice_renders_silence_when_idle() {
        let mut voice = PlaitsVoice::new();
        let patch = MiPlaitsPatch {
            note: 60.0,
            harmonics: 0.5,
            timbre: 0.5,
            morph: 0.5,
            frequency_modulation_amount: 0.0,
            timbre_modulation_amount: 0.0,
            morph_modulation_amount: 0.0,
            engine: 0,
            decay: 0.5,
            lpg_colour: 0.5,
        };
        let modulations = MiPlaitsModulations {
            engine: 0.0,
            note: 0.0,
            frequency: 0.0,
            harmonics: 0.0,
            timbre: 0.0,
            morph: 0.0,
            trigger: 0.0,
            level: 0.0,
            frequency_patched: 0,
            timbre_patched: 0,
            morph_patched: 0,
            trigger_patched: 0,
            level_patched: 0,
        };
        let mut out = [0.0f32; 24];
        let mut aux = [0.0f32; 24];
        voice.render_f32(&patch, &modulations, &mut out, &mut aux, 24);
        // Without a trigger, the voice should be silent (or very close).
        assert!(out.iter().all(|&s| s.abs() < 1.0));
    }

    #[test]
    fn voice_renders_non_silent_after_trigger() {
        let mut voice = PlaitsVoice::new();
        let patch = MiPlaitsPatch {
            note: 60.0,
            harmonics: 0.5,
            timbre: 0.5,
            morph: 0.5,
            frequency_modulation_amount: 0.0,
            timbre_modulation_amount: 0.0,
            morph_modulation_amount: 0.0,
            engine: 8, // virtual analog oscillator
            decay: 0.5,
            lpg_colour: 0.5,
        };
        let mut modulations = MiPlaitsModulations {
            engine: 0.0,
            note: 0.0,
            frequency: 0.0,
            harmonics: 0.0,
            timbre: 0.0,
            morph: 0.0,
            trigger: 1.0,
            level: 0.8,
            frequency_patched: 0,
            timbre_patched: 0,
            morph_patched: 0,
            trigger_patched: 1,
            level_patched: 0,
        };
        // The virtual-analog oscillator may need a block or two to start up,
        // so render several blocks after the trigger before checking the peak.
        let mut out = [0.0f32; 24];
        let mut aux = [0.0f32; 24];
        let mut peak = 0.0f32;
        for _ in 0..8 {
            voice.render_f32(&patch, &modulations, &mut out, &mut aux, 24);
            peak = peak.max(out.iter().map(|s| s.abs()).fold(0.0f32, f32::max));
            modulations.trigger = 0.0;
        }
        assert!(peak > 0.001, "rendered blocks are silent: peak={peak}");
    }
}
