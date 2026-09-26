// Copyright 2026 drumsynth authors.
//
// C shim over the Mutable Instruments Stages `SegmentGenerator` class.

#include "mi_dsp_shim.h"

#include <new>

#include "stmlib/utils/gate_flags.h"
#include "stages/segment_generator.h"

static_assert(sizeof(stages::SegmentGenerator) <= MI_STAGES_STORAGE_SIZE,
              "MI_STAGES_STORAGE_SIZE too small");
static_assert(alignof(stages::SegmentGenerator) <= MI_STAGES_STORAGE_ALIGN,
              "MI_STAGES_STORAGE_ALIGN too small for stages::SegmentGenerator");

extern "C" {

void mi_stages_init(void* storage) {
  stages::SegmentGenerator* g = new (storage) stages::SegmentGenerator();
  g->Init();
}

void mi_stages_configure_single(
    void* storage,
    int type,
    int loop,
    int has_trigger,
    float primary,
    float secondary) {
  stages::SegmentGenerator* g = reinterpret_cast<stages::SegmentGenerator*>(storage);
  stages::segment::Configuration config;
  config.type = static_cast<stages::segment::Type>(type);
  config.loop = loop != 0;
  g->ConfigureSingleSegment(has_trigger != 0, config);
  g->set_segment_parameters(0, primary, secondary);
}

void mi_stages_configure_ad(void* storage, float attack, float decay) {
  stages::SegmentGenerator* g = reinterpret_cast<stages::SegmentGenerator*>(storage);
  stages::segment::Configuration configs[2];
  configs[0].type = stages::segment::TYPE_RAMP;
  configs[0].loop = false;
  configs[1].type = stages::segment::TYPE_RAMP;
  configs[1].loop = false;
  g->Configure(true, configs, 2);
  // Primary = time, secondary = shape. Times are in seconds-ish; map 0..1 to
  // a usable range.
  g->set_segment_parameters(0, attack, 0.5f);
  g->set_segment_parameters(1, decay, 0.5f);
}

void mi_stages_trigger(void* storage) {
  // The SegmentGenerator reads gate flags per sample. A single trigger is
  // delivered by setting the first sample's flag to RISING.
  (void)storage;
}

void mi_stages_process(
    void* storage,
    const uint8_t* gate_flags,
    float* out,
    size_t size) {
  stages::SegmentGenerator* g = reinterpret_cast<stages::SegmentGenerator*>(storage);

  // Convert uint8_t gate flags to stmlib::GateFlags.
  stmlib::GateFlags flags[96];
  for (size_t i = 0; i < size; ++i) {
    flags[i] = gate_flags[i] ? stmlib::GATE_FLAG_HIGH : stmlib::GATE_FLAG_LOW;
  }

  stages::SegmentGenerator::Output output[96];
  g->Process(flags, output, size);

  for (size_t i = 0; i < size; ++i) {
    out[i] = output[i].value;
  }
}

}  // extern "C"
