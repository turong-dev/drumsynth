// Copyright 2026 drumsynth authors.
//
// C shim over the Mutable Instruments Plaits `Voice` class. The Rust wrapper
// owns aligned byte storage and placement-news the C++ object into it; this
// file only exposes block-rate extern "C" functions.
//
// All vendored Mutable Instruments code remains under its MIT license.

#ifndef MI_DSP_SHIM_H_
#define MI_DSP_SHIM_H_

#include <stddef.h>
#include <stdint.h>

#ifdef __cplusplus
extern "C" {
#endif

// Opaque handle to a Plaits voice. The Rust side holds storage sized to
// PLAITS_VOICE_STORAGE_SIZE and aligned to PLAITS_VOICE_STORAGE_ALIGN.
typedef struct MiPlaitsVoice MiPlaitsVoice;

// Parameter/control patch.
typedef struct {
  float note;
  float harmonics;
  float timbre;
  float morph;
  float frequency_modulation_amount;
  float timbre_modulation_amount;
  float morph_modulation_amount;
  int engine;
  float decay;
  float lpg_colour;
} MiPlaitsPatch;

typedef struct {
  float engine;
  float note;
  float frequency;
  float harmonics;
  float timbre;
  float morph;
  float trigger;
  float level;
  uint8_t frequency_patched;
  uint8_t timbre_patched;
  uint8_t morph_patched;
  uint8_t trigger_patched;
  uint8_t level_patched;
} MiPlaitsModulations;

// Required storage size and alignment for MiPlaitsVoice. These are supplied
// by the build script so the Rust array is exactly the right shape.
#define PLAITS_VOICE_STORAGE_SIZE 12288
#define PLAITS_VOICE_STORAGE_ALIGN 8

// Scratch buffer size passed to Init. Mirrors Plaits' shared_buffer.
#define PLAITS_VOICE_BUFFER_SIZE 16384

// Returns the number of engine models available.
int mi_plaits_num_engines(void);

// Seed the process-global `stmlib::Random` LCG that every Plaits engine draws
// its noise from. There is exactly one generator for all voices, so a render
// is only reproducible from a known seed — see mi_dsp::seed_random.
void mi_dsp_seed_random(uint32_t seed);

// ---------------------------------------------------------------------------
// Processing stages (Phase 14 spike)
//
// Block-rate post-voice stages, each an opaque aligned storage block the Rust
// side owns and placement-news into. Same FFI rule as the voice: one call per
// stage per block, never per sample.
// ---------------------------------------------------------------------------

// plaits::LPGEnvelope + plaits::LowPassGate, as one fused stage. The pair is
// how Plaits itself gets its character: a vactrol-modelled envelope driving a
// combined VCA+VCF. Sizes are generous upper bounds, asserted in the .cc.
#define MI_LPG_STORAGE_SIZE 64
#define MI_OVERDRIVE_STORAGE_SIZE 16
#define MI_RESONATOR_STORAGE_SIZE 2048
#define MI_STAGE_STORAGE_ALIGN 8

void mi_lpg_init(void* storage);
void mi_lpg_trigger(void* storage);
// `attack`, `short_decay`, `decay_tail` and `hf` are the LPGEnvelope ping
// parameters; the envelope's gain/frequency/hf_bleed then drive the gate.
void mi_lpg_process(
    void* storage,
    float attack,
    float short_decay,
    float decay_tail,
    float hf,
    float* in_out,
    size_t size);

void mi_overdrive_init(void* storage);
void mi_overdrive_process(void* storage, float drive, float* in_out, size_t size);

void mi_resonator_init(void* storage, float position, int resolution);
void mi_resonator_process(
    void* storage,
    float f0,
    float structure,
    float brightness,
    float damping,
    const float* in,
    float* out,
    size_t size);

// ---------------------------------------------------------------------------
// Warps
// ---------------------------------------------------------------------------

#define MI_WARPS_STORAGE_SIZE 4112
#define MI_WARPS_STORAGE_ALIGN 16

// `algorithm` is 0..1 mapped to the 9 Warps algorithms; `parameter` is the
// algorithm timbre; `drive` is the per-channel drive.
void mi_warps_init(void* storage, float sample_rate);
void mi_warps_process(
    void* storage,
    const float* in_l,
    const float* in_r,
    float* out_l,
    float* out_r,
    size_t size);
// `carrier_shape`: 0 uses the input as its own carrier (cross-modulation);
// 1..5 selects an internal Warps oscillator as the carrier, FM-indexed by the
// input, at MIDI pitch `note`.
void mi_warps_set_parameters(
    void* storage,
    float algorithm,
    float parameter,
    float drive,
    int32_t carrier_shape,
    float note);

// ---------------------------------------------------------------------------
// Stages
// ---------------------------------------------------------------------------

#define MI_STAGES_STORAGE_SIZE 4184
#define MI_STAGES_STORAGE_ALIGN 8

// Segment types matching stages::segment::Type.
#define MI_STAGES_SEGMENT_RAMP 0
#define MI_STAGES_SEGMENT_STEP 1
#define MI_STAGES_SEGMENT_HOLD 2
#define MI_STAGES_SEGMENT_ALT 3

void mi_stages_init(void* storage);
// Configure as a single looping segment (LFO / free-running oscillator).
void mi_stages_configure_single(
    void* storage,
    int type,
    int loop,
    int has_trigger,
    float primary,
    float secondary);
// Configure as a two-segment AD envelope (attack ramp + decay ramp).
void mi_stages_configure_ad(void* storage, float attack, float decay);
// Trigger the envelope. Call at note-on; ignored by LFO modes.
void mi_stages_trigger(void* storage);
// Process one block. `gate_flags` is a uint8_t per sample (0 or non-zero).
void mi_stages_process(
    void* storage,
    const uint8_t* gate_flags,
    float* out,
    size_t size);

// ---------------------------------------------------------------------------
// Clouds
// ---------------------------------------------------------------------------

#define MI_CLOUDS_STORAGE_SIZE 9096
#define MI_CLOUDS_STORAGE_ALIGN 8

void mi_clouds_init(
    void* storage,
    void* large_buffer,
    size_t large_buffer_size,
    void* small_buffer,
    size_t small_buffer_size);
void mi_clouds_process(
    void* storage,
    const float* in_l,
    const float* in_r,
    float* out_l,
    float* out_r,
    size_t size);
void mi_clouds_prepare(void* storage);
void mi_clouds_set_parameters(
    void* storage,
    float position,
    float size,
    float pitch,
    float density,
    float texture,
    float dry_wet,
    float stereo_spread,
    float feedback,
    float reverb,
    int freeze,
    int trigger,
    int gate);

// Placement-new a voice into `memory` (which must be at least
// PLAITS_VOICE_STORAGE_SIZE bytes and aligned to PLAITS_VOICE_STORAGE_ALIGN),
// using `buffer` (PLAITS_VOICE_BUFFER_SIZE bytes) for engine scratch.
void mi_plaits_voice_init(MiPlaitsVoice* voice, void* buffer);

// Render one block. Output is signed 16-bit, planar (out then aux).
void mi_plaits_voice_render(
    MiPlaitsVoice* voice,
    const MiPlaitsPatch* patch,
    const MiPlaitsModulations* modulations,
    int16_t* out,
    int16_t* aux,
    size_t frames);

#ifdef __cplusplus
}
#endif

#endif  // MI_DSP_SHIM_H_
