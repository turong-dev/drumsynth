//! Raw FFI bindings to the C++ shim.

use core::ffi::{c_int, c_void};

// Storage shape constants. These must match the C header exactly.
/// Bytes required for the C++ `plaits::Voice` object.
pub const PLAITS_VOICE_STORAGE_SIZE: usize = 12288;
/// Required alignment for the C++ `plaits::Voice` object.
pub const PLAITS_VOICE_STORAGE_ALIGN: usize = 8;
/// Bytes required for the Plaits scratch/allocator buffer.
pub const PLAITS_VOICE_BUFFER_SIZE: usize = 16384;

/// Opaque storage for a Plaits voice.
#[repr(C, align(8))]
pub struct MiPlaitsVoice {
    _data: [u8; PLAITS_VOICE_STORAGE_SIZE],
}

/// Parameter/control patch.
#[derive(Clone, Copy)]
#[repr(C)]
pub struct MiPlaitsPatch {
    /// Base note (MIDI number).
    pub note: f32,
    /// Harmonics amount.
    pub harmonics: f32,
    /// Timbre amount.
    pub timbre: f32,
    /// Morph amount.
    pub morph: f32,
    /// Depth of frequency (V/Oct) modulation.
    pub frequency_modulation_amount: f32,
    /// Depth of timbre modulation.
    pub timbre_modulation_amount: f32,
    /// Depth of morph modulation.
    pub morph_modulation_amount: f32,
    /// Engine index (0 .. num_engines() - 1).
    pub engine: i32,
    /// LPG/decay time.
    pub decay: f32,
    /// LPG colour.
    pub lpg_colour: f32,
}

/// Per-block modulations.
#[derive(Clone, Copy)]
#[repr(C)]
pub struct MiPlaitsModulations {
    /// Engine selection CV.
    pub engine: f32,
    /// Note offset CV.
    pub note: f32,
    /// Frequency offset CV.
    pub frequency: f32,
    /// Harmonics modulation CV.
    pub harmonics: f32,
    /// Timbre modulation CV.
    pub timbre: f32,
    /// Morph modulation CV.
    pub morph: f32,
    /// Trigger/gate input.
    pub trigger: f32,
    /// Level/velocity input.
    pub level: f32,
    /// 1 if the frequency input is patched, 0 otherwise.
    pub frequency_patched: u8,
    /// 1 if the timbre input is patched, 0 otherwise.
    pub timbre_patched: u8,
    /// 1 if the morph input is patched, 0 otherwise.
    pub morph_patched: u8,
    /// 1 if the trigger input is patched, 0 otherwise.
    pub trigger_patched: u8,
    /// 1 if the level input is patched, 0 otherwise.
    pub level_patched: u8,
}

extern "C" {
    /// Returns the number of available Plaits synthesis models.
    pub fn mi_plaits_num_engines() -> c_int;

    /// Seed the process-global `stmlib::Random` generator.
    pub fn mi_dsp_seed_random(seed: u32);

    /// Placement-new a `plaits::Voice` into `voice` using `buffer` for scratch.
    pub fn mi_plaits_voice_init(voice: *mut MiPlaitsVoice, buffer: *mut c_void);

    /// Render `frames` samples of the voice into planar `out`/`aux` buffers.
    pub fn mi_plaits_voice_render(
        voice: *mut MiPlaitsVoice,
        patch: *const MiPlaitsPatch,
        modulations: *const MiPlaitsModulations,
        out: *mut i16,
        aux: *mut i16,
        frames: usize,
    );
}
