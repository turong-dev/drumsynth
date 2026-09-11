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
