// Copyright 2026 drumsynth authors.
//
// C shim over the Mutable Instruments Clouds `GranularProcessor` class.

#include "mi_dsp_shim.h"

#include <new>

#include "clouds/dsp/granular_processor.h"

static_assert(sizeof(clouds::GranularProcessor) <= MI_CLOUDS_STORAGE_SIZE,
              "MI_CLOUDS_STORAGE_SIZE too small");
static_assert(alignof(clouds::GranularProcessor) <= MI_CLOUDS_STORAGE_ALIGN,
              "MI_CLOUDS_STORAGE_ALIGN too small for clouds::GranularProcessor");

extern "C" {

void mi_clouds_init(
    void* storage,
    void* large_buffer,
    size_t large_buffer_size,
    void* small_buffer,
    size_t small_buffer_size) {
  clouds::GranularProcessor* p = new (storage) clouds::GranularProcessor();
  p->Init(large_buffer, large_buffer_size, small_buffer, small_buffer_size);
}

void mi_clouds_process(
    void* storage,
    const float* in_l,
    const float* in_r,
    float* out_l,
    float* out_r,
    size_t size) {
  clouds::GranularProcessor* p = reinterpret_cast<clouds::GranularProcessor*>(storage);

  clouds::ShortFrame in[32];
  clouds::ShortFrame tmp[32];
  for (size_t i = 0; i < size; ++i) {
    in[i].l = static_cast<int16_t>(in_l[i] * 32767.0f);
    in[i].r = static_cast<int16_t>(in_r[i] * 32767.0f);
  }

  p->Process(in, tmp, size);

  for (size_t i = 0; i < size; ++i) {
    out_l[i] = static_cast<float>(tmp[i].l) / 32768.0f;
    out_r[i] = static_cast<float>(tmp[i].r) / 32768.0f;
  }
}

void mi_clouds_prepare(void* storage) {
  reinterpret_cast<clouds::GranularProcessor*>(storage)->Prepare();
}

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
    int gate) {
  clouds::GranularProcessor* p = reinterpret_cast<clouds::GranularProcessor*>(storage);
  clouds::Parameters* params = p->mutable_parameters();
  params->position = position;
  params->size = size;
  params->pitch = pitch;
  params->density = density;
  params->texture = texture;
  params->dry_wet = dry_wet;
  params->stereo_spread = stereo_spread;
  params->feedback = feedback;
  params->reverb = reverb;
  params->freeze = freeze != 0;
  params->trigger = trigger != 0;
  params->gate = gate != 0;
}

}  // extern "C"
