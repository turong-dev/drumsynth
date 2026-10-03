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

**Velocity is absent, and that is a decision, not an oversight.** Peaks has no
per-voice velocity: the excitation level is a literal in `Process` and the gate
is binary. So `Slot::trigger(velocity)` has nothing to drive.

The obvious reading — apply velocity as an output gain — is **deliberately not
taken yet** (2026-09-26). It is trivially easy to write and impossible to
unwind later without changing how the voice sounds, and it is a real design
choice rather than plumbing: gain-scaled velocity sounds different from
excitation-scaled velocity, and Peaks' `Excitation::Trigger(level)` is right
there accepting a level, so the better answer may well be to drive *that* rather
than the output. Better to decide with a Peaks voice in context than to commit
now and regret it.

Until then, Peaks voices take velocity and discard it: a soft and a hard hit
sound the same, and the track `LEVEL` macro is the only amplitude control. This
is an intentional exception to the `velocity-scales` rule in AGENTS.md for new
machines, and `peaks_velocity_is_intentionally_ignored` in the mi-drum tests
pins it as a known gap so it cannot be mistaken for a bug.

If velocity is added later, the three options in rough order of fidelity are:
(1) scale `Excitation::Trigger(level)` per model, which is what the hardware
class was designed for but needs per-model plumbing since the level is currently
a literal in each `Process`; (2) scale the model output, cheap and immediately
available; (3) drive the excitation *and* let timbre follow, closest to a real
drum machine.



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

## 3. The tail settles onto a limit cycle, so "silence" needs a looser gate

Each model ends in a fixed-point `CLIP`, and the filter settles rather than
reaching zero. Measured tail peaks, int16, at blocks 25/100/175/250/325/400
after one trigger:

| parameters | envelope |
|---|---|
| punch 0.3 (wrapper default) | 11035, 1585, 129, **103, 103, 103** |
| punch 0.125 | 6317, 15, 15, 15, 15, 15 |
| punch 0 | 13619, 1809, 159, 62, 62, 62 |
| punch 0, long decay | 24670, 18431, 3760, 7734, 4400, 2399 |

So a bass drum decays properly and then **parks at 60-100 int16, about
-70 dBFS**, instead of reaching zero. The `mi-drum` slot's `SILENCE_THRESHOLD`
is `1.0e-6`, which a Peaks voice can never satisfy — a voice-type-aware slot
has to gate Peaks more loosely or the track would stay active forever. The
wrapper's own test gate is `3.0e-3` (-50 dBFS); export it rather than
re-deriving it.

Note the long-decay row is still ringing at block 400 (0.8 s). That is correct
behaviour, not a leak, so a silence test needs a generous window.

## Rust wrapper: landed

`mi-dsp/src/peaks.rs` with `PeaksVoice` + `PeaksModel`, the C shim in
`mi_peaks_shim.cc`, and storage sized 192 B (the largest model,
`SnareDrum`, is 188 B). Flash cost measured at +19.4 KB on `mi-bench` — less
than the 40 KB of dead `wav_digits` suggested, because the linker drops what is
unreferenced.

Per-model parameter mapping, following Peaks' own front panel:

| slot | BassDrum | SnareDrum | FmDrum | HighHat |
|---|---|---|---|---|
| 0 | pitch (centred) | pitch (centred) | pitch (absolute MIDI) | — |
| 1 | punch | tone | FM amount | — |
| 2 | tone | snap | decay | — |
| 3 | decay | decay | noise | — |

BassDrum and SnareDrum read parameter 0 as a *signed* offset
(`parameter[0] - 32768`) so it is centred on 0.5; FmDrum reads it as an absolute
MIDI pitch, so it is mapped 24..120.

## Still to do

`MiSlot` has to become voice-type-aware to host these — the enum split the
Phase 14 risk table already anticipates. Per the plan the Plaits tracks keep 2
LFOs + 2 AD envelopes and the Peaks tracks get 1 + 1, which costs no extra
Stages instances and frees ~41 KB.


## Local modification: `HighHat::Init` does not initialise its phases

`peaks::HighHat` holds `uint32_t phase_[6]`, one per square oscillator, and
upstream's `Init()` never writes them. On the hardware that is harmless:
the object is a zeroed static and `Init` runs once at boot, so the phases are
zero because the BSS is.

Here a slot is re-initialised every time its machine changes — the shim
placement-news the model over shared storage and calls `Init` — so the phases
came up holding whatever the previous model had left at those addresses.
Sometimes that was another drum's state; sometimes it was bytes that had held
a pointer, which move with ASLR. The result was a render that differed in
every process, which is what made the committed baseline digest meaningless
for most of Phase 14.

The fix is four lines in `Init` zeroing `phase_[]`, marked `LOCAL FIX` in
`vendor/peaks/drums/high_hat.cc`. Re-apply it if the vendored tree is ever
refreshed from upstream.

`devices/mi-drum/tests/slot_reuse.rs` is the regression guard: it renders each
machine on a clean slot and on a slot that has held something else, and
requires the two to be bit-identical.
