//! Raw FFI bindings to the C++ shim.

use core::ffi::{c_int, c_void};

// Storage shape constants. These must match the C header exactly.
/// Bytes required for the C++ `plaits::Voice` object.
pub const PLAITS_VOICE_STORAGE_SIZE: usize = 12288;
/// Required alignment for the C++ `plaits::Voice` object.
pub const PLAITS_VOICE_STORAGE_ALIGN: usize = 8;
/// Bytes required for the Plaits scratch/allocator buffer.
pub const PLAITS_VOICE_BUFFER_SIZE: usize = 16384;

/// Bytes required for the C++ `warps::Modulator` object.
pub const MI_WARPS_STORAGE_SIZE: usize = 4112;
/// Required alignment for the C++ `warps::Modulator` object.
pub const MI_WARPS_STORAGE_ALIGN: usize = 16;
/// Storage for one `warps::Oscillator`.
pub const MI_WARPS_OSC_STORAGE_SIZE: usize = 96;
/// Alignment for one `warps::Oscillator`.
pub const MI_WARPS_OSC_STORAGE_ALIGN: usize = 8;

/// Bytes required for the largest C++ Peaks drum model (SnareDrum, 188 B).
pub const MI_PEAKS_STORAGE_SIZE: usize = 192;
/// Required alignment for the C++ Peaks drum models.
pub const MI_PEAKS_STORAGE_ALIGN: usize = 4;
/// Model tags for the four Peaks drums, matching the shim.
/// Bass drum model tag.
pub const MI_PEAKS_MODEL_BASS_DRUM: i32 = 0;
/// Snare drum model tag.
pub const MI_PEAKS_MODEL_SNARE_DRUM: i32 = 1;
/// High hat model tag.
pub const MI_PEAKS_MODEL_HIGH_HAT: i32 = 2;
/// FM drum model tag.
pub const MI_PEAKS_MODEL_FM_DRUM: i32 = 3;

/// Bytes required for the C++ `stages::SegmentGenerator` object.
pub const MI_STAGES_STORAGE_SIZE: usize = 4184;
/// Required alignment for the C++ `stages::SegmentGenerator` object.
pub const MI_STAGES_STORAGE_ALIGN: usize = 8;
/// Segment type constants matching `stages::segment::Type`.
pub const MI_STAGES_SEGMENT_RAMP: i32 = 0;
/// Step segment type.
pub const MI_STAGES_SEGMENT_STEP: i32 = 1;
/// Hold segment type.
pub const MI_STAGES_SEGMENT_HOLD: i32 = 2;
/// Alt segment type (oscillator/LFO).
pub const MI_STAGES_SEGMENT_ALT: i32 = 3;

/// Bytes required for the C++ `clouds::GranularProcessor` object.
pub const MI_CLOUDS_STORAGE_SIZE: usize = 9096;
/// Required alignment for the C++ `clouds::GranularProcessor` object.
pub const MI_CLOUDS_STORAGE_ALIGN: usize = 8;

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

#[allow(missing_docs)]
extern "C" {
    /// Returns the number of available Plaits synthesis models.
    pub fn mi_plaits_num_engines() -> c_int;

    /// Seed the process-global `stmlib::Random` generator.
    pub fn mi_dsp_seed_random(seed: u32);

    /// Fused LPG envelope + gate: init, trigger, block process.
    pub fn mi_lpg_init(storage: *mut c_void);
    /// Arm the vactrol envelope's attack ramp.
    pub fn mi_lpg_trigger(storage: *mut c_void);
    /// Advance the envelope one block and apply the gate in place.
    pub fn mi_lpg_process(
        storage: *mut c_void,
        attack: f32,
        short_decay: f32,
        decay_tail: f32,
        hf: f32,
        in_out: *mut f32,
        size: usize,
    );

    /// Gain-compensated soft-clip overdrive.
    pub fn mi_overdrive_init(storage: *mut c_void);
    /// Apply overdrive in place.
    pub fn mi_overdrive_process(storage: *mut c_void, drive: f32, in_out: *mut f32, size: usize);

    /// 24-mode modal resonator bank.
    pub fn mi_resonator_init(storage: *mut c_void, position: f32, resolution: i32);
    /// Excite the resonator with `input`, writing to `output`.
    pub fn mi_resonator_process(
        storage: *mut c_void,
        f0: f32,
        structure: f32,
        brightness: f32,
        damping: f32,
        input: *const f32,
        output: *mut f32,
        size: usize,
    );

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

    // Warps
    pub fn mi_warps_init(storage: *mut c_void, sample_rate: f32);
    pub fn mi_warps_process(
        storage: *mut c_void,
        in_l: *const f32,
        in_r: *const f32,
        out_l: *mut f32,
        out_r: *mut f32,
        size: usize,
    );
    pub fn mi_warps_set_parameters(
        storage: *mut c_void,
        algorithm: f32,
        parameter: f32,
        drive: f32,
        carrier_shape: i32,
        note: f32,
    );
    /// Construct Warps' bare oscillator in `storage`.
    pub fn mi_warps_osc_init(storage: *mut c_void, sample_rate: f32);
    /// Render `size` samples of Warps' oscillator at `shape` and `note`.
    pub fn mi_warps_osc_render(
        storage: *mut c_void,
        shape: c_int,
        note: f32,
        modulation: *const f32,
        out: *mut f32,
        size: usize,
    );
    /// Route Warps' input to its output unchanged.
    ///
    /// `drive = 0` is silence, not clean, so the clean end of the drive axis
    /// has to be a bypass rather than a knob position.
    pub fn mi_warps_set_bypass(storage: *mut c_void, bypass: c_int);

    // Stages
    pub fn mi_stages_init(storage: *mut c_void);
    pub fn mi_stages_configure_single(
        storage: *mut c_void,
        segment_type: i32,
        loop_: i32,
        has_trigger: i32,
        primary: f32,
        secondary: f32,
    );
    pub fn mi_stages_configure_ad(storage: *mut c_void, attack: f32, decay: f32);
    pub fn mi_stages_set_segment_parameters(
        storage: *mut c_void,
        index: i32,
        primary: f32,
        secondary: f32,
    );
    pub fn mi_stages_trigger(storage: *mut c_void);
    pub fn mi_stages_process(
        storage: *mut c_void,
        gate_flags: *const u8,
        out: *mut f32,
        size: usize,
    );

    // Peaks
    pub fn mi_peaks_init(storage: *mut c_void, model: i32);
    pub fn mi_peaks_configure(storage: *mut c_void, model: i32, parameters: *const u16);
    pub fn mi_peaks_process(
        storage: *mut c_void,
        model: i32,
        gate_flags: *const u8,
        out: *mut f32,
        size: usize,
    );

    // Clouds
    pub fn mi_clouds_init(
        storage: *mut c_void,
        large_buffer: *mut c_void,
        large_buffer_size: usize,
        small_buffer: *mut c_void,
        small_buffer_size: usize,
    );
    pub fn mi_clouds_process(
        storage: *mut c_void,
        in_l: *const f32,
        in_r: *const f32,
        out_l: *mut f32,
        out_r: *mut f32,
        size: usize,
    );
    pub fn mi_clouds_prepare(storage: *mut c_void);
    pub fn mi_clouds_set_parameters(
        storage: *mut c_void,
        position: f32,
        size: f32,
        pitch: f32,
        density: f32,
        texture: f32,
        dry_wet: f32,
        stereo_spread: f32,
        feedback: f32,
        reverb: f32,
        freeze: i32,
        trigger: i32,
        gate: i32,
    );
}
