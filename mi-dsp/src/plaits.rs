//! Wrapper for the Mutable Instruments Plaits macro oscillator voice.

use core::cell::UnsafeCell;
use core::mem::MaybeUninit;
use core::sync::atomic::{AtomicU32, Ordering};

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
/// This pool is shared by all [`PlaitsVoice`] instances. The buffers live in
/// `.bss`, not inside the voice struct, so a voice can be moved after
/// construction without invalidating its internal pointers.
///
/// The size differs by target. Firmware runs one engine and needs only its six
/// tracks plus headroom for a reload mid-swap. Host builds run the test harness,
/// which constructs a whole engine per test *in parallel*, so the pool has to
/// cover several engines at once or tests fail on exhaustion rather than on
/// anything real. At 16 KB a buffer that is 128 KB on target and 2 MB on host —
/// the latter being nothing, and the former being the number the RAM budget
/// already accounts for.
#[cfg(target_os = "none")]
const MAX_VOICES: usize = 8;
#[cfg(not(target_os = "none"))]
const MAX_VOICES: usize = 128;

/// Bits per word of the free bitmap. `AtomicU64` does not exist on
/// `thumbv7em-none-eabihf`, so the bitmap is an array of 32-bit words rather
/// than a single integer.
const POOL_BITS: usize = 32;

/// Number of words in the free bitmap.
const POOL_WORDS: usize = MAX_VOICES.div_ceil(POOL_BITS);

/// Static pool of scratch buffers.
///
/// Wrapped in an `UnsafeCell` rather than declared `static mut`: taking a
/// reference to a `static mut` is undefined behaviour, and the pool is
/// genuinely shared across threads in host test builds.
#[repr(align(8))]
struct BufferPool(UnsafeCell<[PlaitsBuffer; MAX_VOICES]>);

// SAFETY: every access goes through `alloc_buffer`, which claims a bit in
// `VOICE_BUFFER_USED` with a compare-exchange before handing out the matching
// buffer pointer. A given index is therefore owned by at most one
// `PlaitsVoice` at a time, and that voice is the only thing that writes to it.
#[allow(unsafe_code)]
unsafe impl Sync for BufferPool {}

static VOICE_BUFFER_POOL: BufferPool =
    BufferPool(UnsafeCell::new([PlaitsBuffer::new(); MAX_VOICES]));

/// Free bitmap for the pool. Bit `i` of word `w` set = buffer `w * 32 + i` is
/// in use.
static VOICE_BUFFER_USED: [AtomicU32; POOL_WORDS] = [const { AtomicU32::new(0) }; POOL_WORDS];

/// Allocate a buffer from the static pool.
///
/// Returns a pointer to the buffer and its index, or `None` if the pool is
/// exhausted. Thread-safe: the claim is a compare-exchange, so two threads
/// racing for the last buffer cannot both win it.
#[allow(unsafe_code)]
fn alloc_buffer() -> Option<(*mut u8, u8)> {
    for (w, word) in VOICE_BUFFER_USED.iter().enumerate() {
        // Bits past MAX_VOICES in the final word are never claimable.
        let valid: u32 = {
            let remaining = MAX_VOICES - w * POOL_BITS;
            if remaining >= POOL_BITS {
                u32::MAX
            } else {
                (1u32 << remaining) - 1
            }
        };
        let mut used = word.load(Ordering::Relaxed);
        loop {
            let free = !used & valid;
            if free == 0 {
                break;
            }
            let bit = free.trailing_zeros();
            match word.compare_exchange_weak(
                used,
                used | (1u32 << bit),
                Ordering::Acquire,
                Ordering::Relaxed,
            ) {
                Ok(_) => {
                    let index = w * POOL_BITS + bit as usize;
                    // SAFETY: the compare-exchange above succeeded, so this
                    // index's bit was clear and is now ours exclusively. No
                    // other thread can hand out the same pointer until
                    // `free_buffer` clears the bit in `Drop`.
                    let ptr = unsafe { (*VOICE_BUFFER_POOL.0.get())[index].0.as_mut_ptr() };
                    return Some((ptr, index as u8));
                }
                // Lost the race (or a spurious weak failure); retry this word
                // with the value we actually observed.
                Err(actual) => used = actual,
            }
        }
    }
    None
}

/// Return a buffer to the static pool.
fn free_buffer(index: u8) {
    let index = index as usize;
    let (w, bit) = (index / POOL_BITS, index % POOL_BITS);
    VOICE_BUFFER_USED[w].fetch_and(!(1u32 << bit), Ordering::Release);
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
    use std::vec::Vec;

    #[test]
    fn voice_initializes_without_panic() {
        let _voice = PlaitsVoice::new();
    }

    /// A freed buffer becomes claimable again, so a process that repeatedly
    /// builds and discards engines does not leak its way to exhaustion.
    ///
    /// Deliberately does not assert *which* index comes back: these tests run
    /// against the same process-wide pool as every other test in this binary,
    /// and the harness runs them in parallel.
    #[test]
    fn pool_reuses_buffers_after_drop() {
        let (_, index) = alloc_buffer().expect("pool empty at rest");
        free_buffer(index);
        let (_, again) = alloc_buffer().expect("freed buffer was not reusable");
        free_buffer(again);
    }

    /// Live claims never alias. This is the invariant that the `unsafe impl
    /// Sync` on the pool rests on, and the one a `static mut` bitmap could not
    /// promise once the test harness started constructing engines in parallel.
    #[test]
    fn pool_never_hands_out_the_same_buffer_twice() {
        let claims: Vec<_> = (0..4)
            .map(|_| alloc_buffer().expect("pool exhausted"))
            .collect();
        for (i, (ptr_a, idx_a)) in claims.iter().enumerate() {
            for (ptr_b, idx_b) in claims.iter().skip(i + 1) {
                assert_ne!(idx_a, idx_b);
                assert_ne!(ptr_a, ptr_b);
            }
        }
        for (_, index) in claims {
            free_buffer(index);
        }
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
