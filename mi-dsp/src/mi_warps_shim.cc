// Copyright 2026 drumsynth authors.
//
// C shim over the Mutable Instruments Warps `Modulator` class.

#include "mi_dsp_shim.h"

#include <new>

#include "warps/dsp/modulator.h"

static_assert(sizeof(warps::Modulator) <= MI_WARPS_STORAGE_SIZE,
              "MI_WARPS_STORAGE_SIZE too small");
static_assert(alignof(warps::Modulator) <= MI_WARPS_STORAGE_ALIGN,
              "MI_WARPS_STORAGE_ALIGN too small for warps::Modulator");

extern "C" {

void mi_warps_init(void* storage, float sample_rate) {
  warps::Modulator* m = new (storage) warps::Modulator();
  m->Init(sample_rate);
}

void mi_warps_process(
    void* storage,
    const float* in_l,
    const float* in_r,
    float* out_l,
    float* out_r,
    size_t size) {
  warps::Modulator* m = reinterpret_cast<warps::Modulator*>(storage);

  // Warps operates on interleaved 16-bit frames. Convert f32 input, process,
  // then convert back.
  warps::ShortFrame in[96];
  warps::ShortFrame tmp[96];
  for (size_t i = 0; i < size; ++i) {
    in[i].l = static_cast<int16_t>(in_l[i] * 32767.0f);
    in[i].r = static_cast<int16_t>(in_r[i] * 32767.0f);
  }

  m->Process(in, tmp, size);

  for (size_t i = 0; i < size; ++i) {
    out_l[i] = static_cast<float>(tmp[i].l) / 32768.0f;
    out_r[i] = static_cast<float>(tmp[i].r) / 32768.0f;
  }
}

void mi_warps_set_parameters(
    void* storage,
    float algorithm,
    float parameter,
    float drive) {
  warps::Modulator* m = reinterpret_cast<warps::Modulator*>(storage);
  warps::Parameters* p = m->mutable_parameters();
  p->modulation_algorithm = algorithm;
  p->modulation_parameter = parameter;
  p->channel_drive[0] = drive;
  p->channel_drive[1] = drive;
  p->carrier_shape = 0;  // external carrier
}

}  // extern "C"
