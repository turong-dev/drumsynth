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
                    // Hand out a cleared buffer. The pool is static memory
                    // that is never scrubbed on free, so a recycled buffer
                    // still holds the previous voice's samples; anything the
                    // model reads before writing it (filter memory, delay
                    // lines, the LPG) would then carry another voice's audio
                    // into this one and make renders depend on allocation
                    // history rather than on the seed.
                    unsafe {
                        core::ptr::write_bytes(ptr, 0, PLAITS_VOICE_BUFFER_SIZE);
                    }
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
/// construction because the buffer never moves and the C++ object holds no
/// self-pointers — `EngineRegistry` stores offsets from itself, not cached
/// addresses of its own members. That is what makes this `Send`, and with it
/// `MiDrumEngine`, which the host's live device mode requires.
pub struct PlaitsVoice {
    storage: MaybeUninit<MiPlaitsVoice>,
    buffer: *mut u8,
    buffer_index: u8,
    initialized: bool,
}

// SAFETY: `Send` promises that *moving* a value of this type to another thread is
// safe, and two independent things make that true here.
//
// 1. The scratch buffer. `alloc_buffer` claims a pool bit with a compare-exchange
//    before returning the pointer, so no other `PlaitsVoice` can be handed the
//    same 16 KB, and the pool is `static`, so the address stays valid wherever
//    this struct goes. `Drop` returns the same bit, so a moved voice still frees
//    the slot it owns.
// 2. The C++ object. It is placement-new'd into `storage`, which travels *with*
//    the struct, and it holds no self-pointers: `EngineRegistry` stores offsets
//    from itself rather than cached member addresses (see
//    `vendor/plaits/dsp/engine/engine.h`, and the `voice_survives_relocation`
//    test below), and `stmlib::DelayLine` holds an inline array rather than a
//    pointer (`vendor/stmlib/dsp/delay_line.h`). The only external address it
//    keeps is the pool buffer covered by (1).
//
// `Send` deliberately and not `Sync`: sharing a `&PlaitsVoice` between threads
// would let two callers race inside one `plaits::Voice`, and `stmlib::Random`,
// which several engines draw from, is one process-global LCG. A voice may move;
// it may not be rendered from two threads at once.
unsafe impl Send for PlaitsVoice {}

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

    /// Release this voice's scratch buffer back to the pool.
    ///
    /// Kept separate from [`Self::new_in_place`] because that also runs on
    /// never-constructed memory — a `.uninit` static on firmware — where
    /// reading the old `buffer_index` would mean reading uninitialised bytes
    /// and freeing a garbage index. Callers reloading a slot they already
    /// built free it first, via the `Slot::load_in_place` hook.
    ///
    /// # Safety
    ///
    /// `ptr` must point to a `PlaitsVoice` previously built by
    /// [`Self::new_in_place`] that has not already been freed.
    #[allow(unsafe_code)]
    pub unsafe fn free_in_place(ptr: *mut PlaitsVoice) {
        if core::ptr::addr_of!((*ptr).initialized).read() {
            let index = core::ptr::addr_of!((*ptr).buffer_index).read();
            free_buffer(index);
            core::ptr::addr_of_mut!((*ptr).initialized).write(false);
        }
    }

    /// Initialize a Plaits voice directly at `ptr`.
    ///
    /// This avoids putting the voice on the stack, which is useful when the
    /// surrounding struct is huge. The buffer still comes from the static pool,
    /// so the resulting voice can be moved safely.
    ///
    /// Any voice already at `ptr` has its buffer leaked, not freed: `ptr` may
    /// be uninitialised memory, so there is no old `buffer_index` that can be
    /// trusted. Reloading a constructed slot must therefore call
    /// [`Self::free_in_place`] first.
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

    /// One 24-sample block of a fixed patch, for comparing two renders.
    ///
    /// Engine 8 (virtual-analog oscillator) is chosen because it draws nothing
    /// from `stmlib::Random`. That generator is a single process-global LCG, so
    /// an engine that used it would make the two render paths in the test below
    /// diverge for reasons that have nothing to do with relocation — the second
    /// voice inherits the first voice's draws.
    fn engine_8_block(voice: &mut PlaitsVoice, trigger: f32) -> [i16; 24] {
        let patch = MiPlaitsPatch {
            note: 60.0,
            harmonics: 0.5,
            timbre: 0.25,
            morph: 0.5,
            frequency_modulation_amount: 0.0,
            timbre_modulation_amount: 0.0,
            morph_modulation_amount: 0.0,
            engine: 8,
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
            trigger,
            level: 0.8,
            frequency_patched: 0,
            timbre_patched: 0,
            morph_patched: 0,
            trigger_patched: 1,
            level_patched: 0,
        };
        let mut out = [0i16; 24];
        let mut aux = [0i16; 24];
        voice.render(&patch, &modulations, &mut out, &mut aux, 24);
        out
    }

    /// Build a voice where it will stay.
    ///
    /// `PlaitsVoice::new()` returns by value, and in an unoptimised build that is
    /// a real `memcpy`: the voice is built in a temporary and then moved, so the
    /// C++ object's engine registry ends up pointing at a temporary that is dead
    /// by the time anyone renders. It works, because nothing reuses that stack.
    /// With offsets in the registry it also works *because it is correct*, which
    /// is the point — but a test about relocation cannot measure itself against
    /// a voice that was already relocated behind its back, so it builds in place.
    ///
    /// Returns the voice's address, which the test needs in order to build a copy
    /// somewhere else.
    ///
    /// # Safety
    ///
    /// `storage` must be a live, writable `MaybeUninit<PlaitsVoice>` — its size
    /// and alignment are the type's own, so the caller only has to mean it.
    unsafe fn voice_in_place(storage: &mut MaybeUninit<PlaitsVoice>) -> *mut PlaitsVoice {
        let voice = storage.as_mut_ptr();
        PlaitsVoice::new_in_place(voice);
        voice
    }

    /// A voice renders identically after its bytes have been copied to a new
    /// address, with the source moved on behind it.
    ///
    /// This is the invariant `EngineRegistry`'s offsets buy and cached member
    /// pointers would not: `plaits::Voice::Init` hands the registry the addresses
    /// of its own member engines, and an absolute address points wherever the
    /// voice used to be. `unsafe impl Send for PlaitsVoice` — and with it
    /// `MiDrumEngine`, which the host's live device mode needs — rests on this
    /// holding, so it is asserted rather than argued.
    ///
    /// Two details are load-bearing, and both were found by watching this test
    /// pass against the buggy version:
    ///
    /// - The source must be built with [`voice_in_place`]. A voice built with
    ///   `PlaitsVoice::new()` has already been relocated before the test starts.
    /// - The source must be *advanced* after the copy. A copy that kept the
    ///   source's addresses would then render the source's later state instead of
    ///   the state it was copied with. Without that step both versions render the
    ///   same untouched state and the test proves nothing.
    #[test]
    fn voice_survives_relocation() {
        const BLOCKS: usize = 8;
        const SPLIT: usize = 3;
        // Alternate the gate so the voice's state moves every block; a constant
        // trigger would leave a decaying tail that hides a stale pointer.
        fn trigger(i: usize) -> f32 {
            if i.is_multiple_of(2) {
                1.0
            } else {
                0.0
            }
        }
        // Engine 8 (virtual analog) keeps its state in the object and takes only
        // a scratch buffer from the allocator, so advancing one voice does not
        // disturb the other through the pool they share. It also draws nothing
        // from `stmlib::Random`, the process-global LCG that would otherwise make
        // two renders diverge for reasons unrelated to relocation.
        const {
            assert!(
                BLOCKS > SPLIT,
                "the copy must be followed by at least one block"
            );
        }

        // The whole render, with the voice never moved.
        let mut reference_store = MaybeUninit::<PlaitsVoice>::uninit();
        let want: Vec<_> = {
            // SAFETY: see `voice_in_place`.
            let reference = unsafe { voice_in_place(&mut reference_store) };
            (0..BLOCKS)
                .map(|i| engine_8_block(unsafe { &mut *reference }, trigger(i)))
                .collect()
        };

        // The same render, interrupted by a byte copy to another address — what a
        // `memcpy` of the containing engine does when it crosses threads.
        let mut source_store = MaybeUninit::<PlaitsVoice>::uninit();
        let mut copy_store = MaybeUninit::<PlaitsVoice>::uninit();
        let want_tail = {
            // SAFETY: see `voice_in_place`.
            let source = unsafe { voice_in_place(&mut source_store) };
            for i in 0..SPLIT {
                engine_8_block(unsafe { &mut *source }, trigger(i));
            }

            // SAFETY: `source_store` holds an initialised `PlaitsVoice` and
            // `copy_store` is `size_of::<PlaitsVoice>()` bytes of aligned,
            // writable storage, so this is a plain byte move into valid storage.
            unsafe {
                core::ptr::copy_nonoverlapping(
                    source.cast::<u8>(),
                    copy_store.as_mut_ptr().cast::<u8>(),
                    core::mem::size_of::<PlaitsVoice>(),
                );
            }
            // The copy is never dropped: it shares the source's pool slot, which
            // is returned once, below. `source` stays mortal and is freed there.
            let mut moved = core::mem::ManuallyDrop::new(unsafe { copy_store.assume_init() });
            assert_ne!(
                source as usize,
                core::ptr::addr_of!(*moved) as usize,
                "relocated voice landed where it was copied from"
            );

            // Advance the source past the copy point, then render the copy from
            // where the source left it.
            for i in SPLIT..BLOCKS {
                engine_8_block(unsafe { &mut *source }, trigger(i));
            }
            (SPLIT..BLOCKS)
                .map(|i| engine_8_block(&mut moved, trigger(i)))
                .collect::<Vec<_>>()
        };

        for (i, (want, got)) in want[SPLIT..].iter().zip(want_tail.iter()).enumerate() {
            assert_eq!(
                want,
                got,
                "block {} differs after the voice was relocated",
                SPLIT + i
            );
        }

        // SAFETY: both voices are built by `new_in_place` above; the copy never
        // dropped, so the source is the only owner of the pool slot.
        unsafe { PlaitsVoice::free_in_place(reference_store.as_mut_ptr()) };
        unsafe { PlaitsVoice::free_in_place(source_store.as_mut_ptr()) };
    }
}
