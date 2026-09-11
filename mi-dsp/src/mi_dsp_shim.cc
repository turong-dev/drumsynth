// Copyright 2026 drumsynth authors.
//
// C shim over the Mutable Instruments Plaits `Voice` class.

#include "mi_dsp_shim.h"

#include <new>

#include "plaits/dsp/voice.h"
#include "stmlib/utils/buffer_allocator.h"
#include "stmlib/utils/random.h"
#include "plaits/dsp/envelope.h"
#include "plaits/dsp/fx/low_pass_gate.h"
#include "plaits/dsp/fx/overdrive.h"
#include "plaits/dsp/physical_modelling/resonator.h"

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

void mi_dsp_seed_random(uint32_t seed) { stmlib::Random::Seed(seed); }

// --- Processing stages -----------------------------------------------------

namespace {

// The LPG is a pair: the vactrol envelope produces gain/frequency/hf_bleed,
// the gate applies them. Kept together so the Rust side owns one object.
struct LpgStage {
  plaits::LPGEnvelope envelope;
  plaits::LowPassGate gate;
};

}  // namespace

static_assert(sizeof(LpgStage) <= MI_LPG_STORAGE_SIZE, "MI_LPG_STORAGE_SIZE too small");
static_assert(sizeof(plaits::Overdrive) <= MI_OVERDRIVE_STORAGE_SIZE,
              "MI_OVERDRIVE_STORAGE_SIZE too small");
static_assert(sizeof(plaits::Resonator) <= MI_RESONATOR_STORAGE_SIZE,
              "MI_RESONATOR_STORAGE_SIZE too small");

void mi_lpg_init(void* storage) {
  LpgStage* s = new (storage) LpgStage();
  s->envelope.Init();
  s->gate.Init();
}

void mi_lpg_trigger(void* storage) {
  reinterpret_cast<LpgStage*>(storage)->envelope.Trigger();
}

void mi_lpg_process(
    void* storage,
    float attack,
    float short_decay,
    float decay_tail,
    float hf,
    float* in_out,
    size_t size) {
  LpgStage* s = reinterpret_cast<LpgStage*>(storage);
  s->envelope.ProcessPing(attack, short_decay, decay_tail, hf);
  s->gate.Process(
      s->envelope.gain(),
      s->envelope.frequency(),
      s->envelope.hf_bleed(),
      in_out,
      size);
}

void mi_overdrive_init(void* storage) {
  plaits::Overdrive* o = new (storage) plaits::Overdrive();
  o->Init();
}

void mi_overdrive_process(void* storage, float drive, float* in_out, size_t size) {
  reinterpret_cast<plaits::Overdrive*>(storage)->Process(drive, in_out, size);
}

void mi_resonator_init(void* storage, float position, int resolution) {
  plaits::Resonator* r = new (storage) plaits::Resonator();
  r->Init(position, resolution);
}

void mi_resonator_process(
    void* storage,
    float f0,
    float structure,
    float brightness,
    float damping,
    const float* in,
    float* out,
    size_t size) {
  reinterpret_cast<plaits::Resonator*>(storage)->Process(
      f0, structure, brightness, damping, in, out, size);
}

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
