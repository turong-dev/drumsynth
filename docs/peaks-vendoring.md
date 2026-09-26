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

### 2. The raw output saturates; the module relies on an output stage we do not have

Measured through `Process` with no output limiting, neutral-ish parameters:

| model | peak | samples at full scale |
|---|---|---|
| BassDrum | 1.0000 | 99.98% |
| SnareDrum | 1.0000 | 57.08% |
| HighHat | 1.0000 | — |
| FmDrum | 0.0853 | — |

That is flat-topping, not merely loud: the int16 output is clipping, and that
is unrecoverable. The real Peaks module applies gain staging and a limiter after
these models; we bypass it, so **the wrapper needs a per-model trim** (order
1/8 looks about right for BassDrum/Snare, which are peaking near 8x) before the
signal reaches the engine, or every Peaks voice will be a square wave.

This is a wrapper concern, not a vendoring one, but it is the first thing that
will bite.

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
