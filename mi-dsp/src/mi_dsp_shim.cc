// Copyright 2026 drumsynth authors.
//
// C shim over the Mutable Instruments Plaits `Voice` class.

#include "mi_dsp_shim.h"

#include <new>

#include "plaits/dsp/voice.h"
#include "stmlib/utils/buffer_allocator.h"

// Ensure the Rust-side storage is large enough for the real object.
static_assert(
    sizeof(plaits::Voice) <= PLAITS_VOICE_STORAGE_SIZE,
    "PLAITS_VOICE_STORAGE_SIZE too small for plaits::Voice");
static_assert(
    alignof(plaits::Voice) <= PLAITS_VOICE_STORAGE_ALIGN,
    "PLAITS_VOICE_STORAGE_ALIGN too small for plaits::Voice");

// Ensure the scratch buffer is the size the original Plaits firmware expects.
static_assert(
    PLAITS_VOICE_BUFFER_SIZE == 16384,
    "PLAITS_VOICE_BUFFER_SIZE must match Plaits shared_buffer");

extern "C" {

int mi_plaits_num_engines(void) { return plaits::kMaxEngines; }

void mi_plaits_voice_init(MiPlaitsVoice* voice, void* buffer) {
  stmlib::BufferAllocator allocator(buffer, PLAITS_VOICE_BUFFER_SIZE);
  new (voice) plaits::Voice();
  reinterpret_cast<plaits::Voice*>(voice)->Init(&allocator);
}

void mi_plaits_voice_render(
    MiPlaitsVoice* voice,
    const MiPlaitsPatch* patch,
    const MiPlaitsModulations* modulations,
    int16_t* out,
    int16_t* aux,
    size_t frames) {
  plaits::Voice* v = reinterpret_cast<plaits::Voice*>(voice);

  plaits::Patch p;
  p.note = patch->note;
  p.harmonics = patch->harmonics;
  p.timbre = patch->timbre;
  p.morph = patch->morph;
  p.frequency_modulation_amount = patch->frequency_modulation_amount;
  p.timbre_modulation_amount = patch->timbre_modulation_amount;
  p.morph_modulation_amount = patch->morph_modulation_amount;
  p.engine = patch->engine;
  p.decay = patch->decay;
  p.lpg_colour = patch->lpg_colour;

  plaits::Modulations m;
  m.engine = modulations->engine;
  m.note = modulations->note;
  m.frequency = modulations->frequency;
  m.harmonics = modulations->harmonics;
  m.timbre = modulations->timbre;
  m.morph = modulations->morph;
  m.trigger = modulations->trigger;
  m.level = modulations->level;
  m.frequency_patched = modulations->frequency_patched != 0;
  m.timbre_patched = modulations->timbre_patched != 0;
  m.morph_patched = modulations->morph_patched != 0;
  m.trigger_patched = modulations->trigger_patched != 0;
  m.level_patched = modulations->level_patched != 0;

  // The Voice writes interleaved short frames into a contiguous buffer.
  // Allocate a temporary block on the stack — frames is at most the host
  // engine block size (32), so this is tiny.
  plaits::Voice::Frame temp[64];
  v->Render(p, m, temp, frames);

  for (size_t i = 0; i < frames; ++i) {
    out[i] = temp[i].out;
    aux[i] = temp[i].aux;
  }
}

}  // extern "C"
