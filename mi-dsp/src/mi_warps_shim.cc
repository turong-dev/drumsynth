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

  // `Modulator`'s constructor is empty and `Init` only seeds
  // `previous_parameters_`, so `parameters_` is left as whatever was in the
  // placement-new storage — in the firmware that is uninitialised OCRAM. Seed
  // it to a sane external-carrier cross-modulator so a `Process` before any
  // `set_parameters` is still well-defined rather than reading garbage.
  warps::Parameters* p = m->mutable_parameters();
  p->channel_drive[0] = 1.0f;
  p->channel_drive[1] = 1.0f;
  p->modulation_algorithm = 0.0f;
  p->modulation_parameter = 0.5f;
  p->frequency_shift_pot = 0.5f;
  p->frequency_shift_cv = 0.5f;
  p->phase_shift = 0.5f;
  p->note = 48.0f;
  p->carrier_shape = 0;  // external carrier
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
    float drive,
    int32_t carrier_shape,
    float note) {
  warps::Modulator* m = reinterpret_cast<warps::Modulator*>(storage);
  warps::Parameters* p = m->mutable_parameters();
  p->modulation_algorithm = algorithm;
  p->modulation_parameter = parameter;
  p->channel_drive[0] = drive;
  p->channel_drive[1] = drive;
  // 0 = external carrier (the input cross-modulates itself). 1..5 selects an
  // internal oscillator as the carrier, with the input as its FM index; the
  // shape is `OscillatorShape(carrier_shape - 1)` and `note` is a MIDI pitch.
  p->carrier_shape = carrier_shape;
  p->note = note;
}

void mi_warps_set_bypass(void* storage, int32_t bypass) {
  warps::Modulator* m = reinterpret_cast<warps::Modulator*>(storage);
  // `Process` short-circuits to a straight copy of input to output, which is
  // bit-transparent apart from the f32 <-> int16 round trip the shim does
  // anyway. This is what makes "no drive" mean *clean* rather than *silent*:
  // `SaturatingAmplifier`'s pre-gain is `0.5*drive` blended towards
  // `24*drive^5`, so `drive = 0` attenuates to nothing rather than to unity.
  m->set_bypass(bypass != 0);
}

}  // extern "C"
