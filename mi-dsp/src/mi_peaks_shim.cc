// Copyright 2026 drumsynth authors.
//
// C shim over the Mutable Instruments Peaks drum models.
//
// Peaks models speak 16-bit fixed point and a Peaks-local gate-flag byte. Both
// are converted here rather than in Rust so the bit mapping sits next to
// `peaks::GATE_FLAG_*`, which is the whole point: Peaks defines its own
// `GateFlags` as a `uint8_t` with LOW=0, HIGH=1, RISING=2, FALLING=4, and those
// are NOT `stmlib::GateFlags`. The Rust side passes the `mi_dsp::stages::GATE_*`
// convention and this file owns the translation.

#include "mi_dsp_shim.h"

#include <new>

#include "peaks/gate_processor.h"
#include "peaks/drums/bass_drum.h"
#include "peaks/drums/fm_drum.h"
#include "peaks/drums/high_hat.h"
#include "peaks/drums/snare_drum.h"

// One voice holds whichever model is selected. Sized for the largest.
static_assert(sizeof(peaks::SnareDrum) <= MI_PEAKS_STORAGE_SIZE,
              "MI_PEAKS_STORAGE_SIZE too small for peaks::SnareDrum");
static_assert(sizeof(peaks::HighHat) <= MI_PEAKS_STORAGE_SIZE,
              "MI_PEAKS_STORAGE_SIZE too small for peaks::HighHat");
static_assert(sizeof(peaks::BassDrum) <= MI_PEAKS_STORAGE_SIZE,
              "MI_PEAKS_STORAGE_SIZE too small for peaks::BassDrum");
static_assert(sizeof(peaks::FmDrum) <= MI_PEAKS_STORAGE_SIZE,
              "MI_PEAKS_STORAGE_SIZE too small for peaks::FmDrum");
static_assert(alignof(peaks::SnareDrum) <= MI_PEAKS_STORAGE_ALIGN,
              "MI_PEAKS_STORAGE_ALIGN too small for peaks::SnareDrum");

namespace {

// Which model currently lives in the storage. The Rust side owns this value;
// the shim only needs it to dispatch Process/Configure.
enum class Model { kBassDrum, kSnareDrum, kHighHat, kFmDrum };

// Translate the drumsynth gate convention (0 low, 1 high, 2 rising, 3 falling)
// into Peaks' bit flags. The `switch` is deliberate: `falling` must become
// GATE_FLAG_FALLING (4), not 3.
peaks::GateFlags ToPeaksGate(uint8_t gate) {
  switch (gate) {
    case 1: return peaks::GATE_FLAG_HIGH;
    case 2: return peaks::GATE_FLAG_RISING;
    case 3: return peaks::GATE_FLAG_FALLING;
    default: return peaks::GATE_FLAG_LOW;
  }
}

}  // namespace

extern "C" {

void mi_peaks_init(void* storage, int32_t model) {
  switch (static_cast<Model>(model)) {
    case Model::kBassDrum: {
      peaks::BassDrum* d = new (storage) peaks::BassDrum();
      d->Init();
      break;
    }
    case Model::kSnareDrum: {
      peaks::SnareDrum* d = new (storage) peaks::SnareDrum();
      d->Init();
      break;
    }
    case Model::kHighHat: {
      peaks::HighHat* d = new (storage) peaks::HighHat();
      d->Init();
      break;
    }
    default: {
      peaks::FmDrum* d = new (storage) peaks::FmDrum();
      d->Init();
      break;
    }
  }
}

void mi_peaks_configure(void* storage, int32_t model, const uint16_t* parameters) {
  uint16_t p[4] = {0, 0, 0, 0};
  for (int i = 0; i < 4; ++i) {
    p[i] = parameters[i];
  }
  switch (static_cast<Model>(model)) {
    case Model::kBassDrum:
      reinterpret_cast<peaks::BassDrum*>(storage)->Configure(
          p, peaks::CONTROL_MODE_FULL);
      break;
    case Model::kSnareDrum:
      reinterpret_cast<peaks::SnareDrum*>(storage)->Configure(
          p, peaks::CONTROL_MODE_FULL);
      break;
    case Model::kHighHat:
      // Peaks' HighHat::Configure is empty -- the model has no parameters in
      // this source drop. Accept the call and ignore it rather than failing.
      break;
    default:
      reinterpret_cast<peaks::FmDrum*>(storage)->Configure(
          p, peaks::CONTROL_MODE_FULL);
      break;
  }
}

void mi_peaks_process(
    void* storage,
    int32_t model,
    const uint8_t* gate_flags,
    float* out,
    size_t size) {
  int16_t raw[96];
  peaks::GateFlags flags[96];
  for (size_t i = 0; i < size; ++i) {
    flags[i] = ToPeaksGate(gate_flags[i]);
  }
  switch (static_cast<Model>(model)) {
    case Model::kBassDrum:
      reinterpret_cast<peaks::BassDrum*>(storage)->Process(flags, raw, size);
      break;
    case Model::kSnareDrum:
      reinterpret_cast<peaks::SnareDrum*>(storage)->Process(flags, raw, size);
      break;
    case Model::kHighHat:
      reinterpret_cast<peaks::HighHat*>(storage)->Process(flags, raw, size);
      break;
    default:
      reinterpret_cast<peaks::FmDrum*>(storage)->Process(flags, raw, size);
      break;
  }
  for (size_t i = 0; i < size; ++i) {
    out[i] = raw[i] / 32768.0f;
  }
}

}  // extern "C"
