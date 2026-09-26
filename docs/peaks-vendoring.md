# Peaks vendoring

Phase 14.4 needs the Peaks drum voices. This note records where the tree came
from, what is present, and the two things about it that are not obvious.

## Provenance

The four drum models were vendored in `86e9e7e` with no source URL recorded, and
two files they `#include` were left behind. Both gaps are now closed.

The missing files are in **`pichenettes/eurorack`**, not `pichenettes/peaks` —
the latter 404s for every path. Fetched from:

```
https://raw.githubusercontent.com/pichenettes/eurorack/refs/heads/master/peaks/
```

| File | Fetched | Note |
|---|---|---|
| `gate_processor.h` | 2026-09-26 | upstream file, verbatim |
| `resources.h` | 2026-09-26 | upstream file, verbatim |
| `resources.cc` | 2026-09-26 | upstream file, verbatim (376 KB, see below) |

`gate_processor.h` was briefly hand-written as a local reduction before the
correct repository was found. That stub was **wrong, not merely incomplete** —
see "GateFlags is Peaks-local" below — and was replaced with the upstream file.
Falling into that trap while verifying is what produced the bogus saturation
figures corrected in section 2.

## Present

```
peaks/gate_processor.h
peaks/resources.h
peaks/resources.cc
peaks/drums/bass_drum.{h,cc}
peaks/drums/fm_drum.{h,cc}
peaks/drums/high_hat.{h,cc}
peaks/drums/snare_drum.{h,cc}
peaks/drums/excitation.h
peaks/drums/svf.h
```

All four models compile and produce audio (verified: peak > 0.01, no NaN, all
four triggered and ringing).

Footprint, which is the reason Peaks is attractive for the extra tracks:
`BassDrum` 104 B, `FmDrum` 40 B, `HighHat` 108 B, `SnareDrum` 188 B —
**440 B for all four**, against 12,304 B for one Plaits voice.

## Two things that are not obvious

### 1. `GateFlags` is Peaks-local, not `stmlib::GateFlags`

`peaks/gate_processor.h` defines its own bit flags and typedefs `GateFlags` to
`uint8_t`:

```cpp
typedef uint8_t GateFlags;
enum GateFlagsBits {
  GATE_FLAG_LOW = 0, GATE_FLAG_HIGH = 1, GATE_FLAG_RISING = 2, GATE_FLAG_FALLING = 4, ...
};
```

These are **not** `stmlib::GateFlags`, and the bit values differ. The models do
`GateFlags gate_flag = *gate_flags++;` and test `gate_flag & GATE_FLAG_RISING`
unqualified, so a shim that passes `stmlib::GateFlags` through will compile or
silently mistrigger depending on the values. A wrapper must convert.

`ControlMode` is also only two states upstream — `CONTROL_MODE_FULL = 0`,
`CONTROL_MODE_HALF = 1` — not the three-way enum one might assume.

### 2. There is no gain staging, no limiter, and no velocity — and that is correct

An earlier version of this note claimed the models "saturate" and need a
per-model trim because the real module applies gain staging and a limiter we
bypass. **That was wrong.** Checked against the upstream audio path
(`peaks/peaks.cc:116`):

```cpp
processors[i].Process(block->input[i], output_buffer, size);
block->output[i][j] = calibration_data.DacCode(i, output_buffer[j]);
```

That is the whole chain. `Processors` is a thin forwarder
(`processors.h:66`, a macro that just calls `variable.Process`), and `Dac` is a
raw SPI/I2S writer. There is no limiter and no output gain stage anywhere in
Peaks. The models are *meant* to run at up to full scale and hand that straight
to the converter — `BassDrum::Process` ends in a deliberate `CLIP(output)`, and
drives its resonator with a hardcoded `12 * 32768 * 0.7` excitation, because a
saturating analogue-modelled drum circuit *is* the sound.

Measured correctly — trigger once, then release the gate, so it can decay:

| model | peak | samples at full scale | silent after |
|---|---|---|---|
| BassDrum | 32767 | 0.07% | ~82 ms |
| SnareDrum | 32767 | 1.80% | ~92 ms |
| HighHat | 714 | 0.00% | ~13 ms |
| FmDrum | 32700 | 0.00% | ~198 ms |

Peak at full scale with well under 2% of samples there is ordinary drum
material, not flat-topping. **No trim is needed**; the `LEVEL` macro (0.85) and
the engine's master clipper are the right place for gain staging in this
architecture.

> Getting those numbers took driving the models wrong twice first: with
> `stmlib` gate flags instead of `peaks::GateFlags`, and then re-triggering on
> every block so the drum never decayed. Both made it look like 99.98%
> saturation. If a Peaks voice ever sounds like a square wave, suspect the
> harness before the model.

**Velocity is a genuine gap, but not a bypassed one.** Peaks has no per-voice
velocity at all — the excitation level is a literal in `Process`, and
`Slot::trigger(velocity)` would be ignored. Our `Slot` contract requires
velocity to affect amplitude, so the wrapper has to apply it as an output gain.
That is a new feature for Peaks voices rather than the restoration of something
the vendor tree had.


## Table sizes in `resources.cc`

The file is 376 KB because it serves the whole Peaks module, not just the drums.
Only five tables are needed here:

| Symbol | Type | Size | Used by |
|---|---|---|---|
| `lut_svf_cutoff` | `uint16_t[]` | 257 | all four models, via `peaks::Svf` |
| `lut_svf_damp` | `uint16_t[]` | 257 | all four models |
| `lut_env_expo` | `uint16_t[]` | 257 | `fm_drum` |
| `lut_env_increments` | `uint32_t[]` | 257 | `fm_drum` |
| `lut_oscillator_increments` | `uint32_t[]` | **97** | `fm_drum` |

The other ~40 KB is `wav_digits` (36,824 entries) plus four 1025-entry
wavefolding tables, all belonging to the digital/display engine the drum models
never touch. It is dead weight in flash — harmless now that `.rodata` lives
there, but it is the obvious trim if flash ever gets tight. The table is
vendored whole rather than cut down so the tree stays a faithful copy.

## Not started

The Rust side. `mi-dsp` has no `peaks.rs` and no shim; `build.rs` compiles no
`peaks/` sources. The wrapper needs Peaks' 16-bit
`Configure(uint16_t*, ControlMode)` parameter interface rather than Plaits'
float `Patch`, so `MiSlot` has to become voice-type-aware — the enum split the
Phase 14 risk table already anticipates.
