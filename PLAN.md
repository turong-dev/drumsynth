# Plan: Syntakt-style machine engine, 8 tracks

A plan to move the engine from a 3-voice prototype toward an Elektron
Syntakt-style machine architecture: a catalog of named synthesis models,
each exposing 8 normalized macro knobs, wrapped in a per-track strip
(filter, amp envelope, overdrive, pan), with LFO modulation, velocity
routing, choke/layer, and sound locks.

## Current state

- 3 fixed voices (kick, snare, hat) hard-wired as fields on `DrumEngine`.
- Human-unit params (`decay_s`, `start_hz`); `set_params` at setup rate;
  per-sample `tick()` with early-outs.
- DSP: `DecayEnv`, `SineOsc`, `Noise`, one-pole filters, `soft_clip`,
  `exp2_approx`. Deterministic noise. 38 tests green.
- Host renderer with sweep mode — but `SweepParam` is a hand-maintained
  enum that won't scale to ~15 machines x 8 macros.
- MIDI: parser in engine, CC map in firmware.
- Firmware: bench builds and is verified on hardware; audio output
  (`TODO(sai)`) not yet implemented.

## Bench findings (measured on hardware, 2026-08-05)

```
core 600 MHz, 48000 Hz, block 32
budget 400000 cycles per block

idle     avg=   3638 cy  peak=   3638 cy    0.9% of budget  (113 cy/frame)
sounding avg= 125907 cy  peak= 125934 cy   31.5% of budget  (3935 cy/frame)
```

The README projected ~22k / 5.5% for the sounding case. The real number
is 126k / 31.5% — 5.7x higher.

### Root cause: `libm::sinf`

The kick (1 sine osc) + snare (2 sine oscs) call `sinf` 3 times per
sample, 96 times per block. `libm::sinf` is a pure-software
transcendental costing ~1,000-1,300 cycles on the M7. That is ~115k
cycles — 90% of the measured total. Everything else (envelopes,
filters, noise, soft-clip, interleave) is ~11k.

### Budget implications

Without optimization, 8 tracks of similar complexity = ~84% of budget.
Over.

With the sin-table swap that `fast.rs` was already designed for, `sinf`
drops from ~1,200 cy to ~10 cy. Three voices go from 126k to ~13k. Then:

| scenario | est. cycles/block | est. % budget |
|---|---|---|
| 8 tracks, current-complexity machines | ~35k | ~9% |
| 8 tracks, 2x heavier (FM, multi-osc) | ~65k | ~16% |
| 8 tracks + SVF + AHD + LFOs per track | ~85k | ~21% |
| 8 tracks + sends (delay + reverb) | ~120-150k | ~30-38% |

Comfortable. The 8-track decision is validated by these numbers.

## Architecture target

```
DrumEngine
 +-- tracks: [Track; 8]
 |    +-- slot: MachineSlot          enum dispatch, match in tick()
 |    +-- macros: [f32; 8]          0..1, the stored user-facing state
 |    +-- strip: TrackStrip          SVF (LP/HP/BP/notch) + filter env + AHD amp + drive + pan
 |    +-- mod: ModState              2 LFOs + routing table + 4 velocity-mod slots
 |    +-- choke/layer masks: u8
 +-- sends: SendFx                   Phase 5, bench-gated
 +-- midi: parser (exists) + note/CC map tables
```

Key decisions:

- **Macros are primary.** Machines store 8 normalized macros (0..1,
  CC-friendly) plus a `recalc()` that derives internal coefficients (Hz,
  seconds, gains) via documented mapping curves. Current human-unit param
  structs become derived state, not stored state. This is what makes
  generic host sweeps possible without a hand-maintained enum.
- **Enum dispatch, not trait objects.** `enum MachineSlot { BdClassic(..),
  BdFm(..), SdNatural(..), ... }`, `match` in `tick()`. Memory = largest
  machine x 8 — a few KB, nothing in 1MB.
- **Control-rate pass.** Once per 32-frame block: advance LFOs, sum
  modulation onto macros, recompute coefficients only for dirty params.
  The audio callback stays thin.
- **A `Sound` is `{ machine_id, macros, strip_params }`** — `Copy`, ~100
  bytes. `load_sound(track, &Sound)` at trig time = sound locks; pool of
  128 = ~13KB, firmware/host-side.
- **Envelope upgrade**: AHD envelope for the track amp stage (attack,
  hold, decay). `DecayEnv` stays for internal machine envelopes.
- **Filter upgrade**: TPT state-variable filter (LP/HP/BP/notch),
  coefficients precomputed at control rate. One-poles stay inside
  machines where adequate.
- **Sin table is mandatory, not optional.** 512-entry quarter-wave table
  with linear interpolation, ~2KB flash, ~10 cy/lookup. This is the single
  highest-leverage optimization and the gate for scaling beyond 3 voices.
  The swap is behind `fast::sin_turns` — one function, as designed.

## Constraints preserved

- `#![no_std]`, no `alloc`, `f32` only, compile-time sized.
- Engine crate knows nothing about hardware.
- Host render workflow with sweeps (now generic over machine + macro).
- Deterministic noise (bit-comparable host vs. target).
- CI: cross-compile for `thumbv7em-none-eabihf` + clippy on every push.

## Phases

### Phase 0 — Measure (DONE)

- [x] Install tooling: `cargo-binutils`, `teensy_loader_cli`.
- [x] Fix first-ever compile: workspace exclude, `teensy4-panic 0.3`,
      `DWT::unlock`, PIT `&mut`.
- [x] Build bench HEX, flash, capture numbers.
- [x] Analyze: `libm::sinf` dominates at 90% of the sounding budget.

### Phase 1 — Re-architecture + sin table (no new sounds)

The sin table goes first because nothing else makes sense until the
budget is real:

1. **`fast::sin_turns` table swap.** 512-entry quarter-wave, linear
   interpolation. Re-run bench to confirm the drop. A/B via host render to
   confirm inaudible for percussion.
2. **Machine trait + `MachineSlot` enum.** Port kick/snare/hat to
   machines: BD Classic, SD Natural, CH/OH Classic (same machine, longer
   DEC).
3. **`Track` struct**: machine slot + strip (SVF, AHD, drive, pan).
4. **Macro layer**: normalized 0..1 macros with names/ranges, `recalc()`.
5. **Control-rate pass**: per-block `control()`, LFO stub, dirty-flag
   coefficient recompute.
6. **Renderer**: generic `sweep <machine> <macro>` and `machine <name>
   --macro K=V` modes. Kill the `SweepParam` enum.
7. **MIDI**: note-to-track map table, CC-to-(track, macro) map table.
8. **Tests**: port all 38 existing tests to machines. Add macro-corner
   NaN sweeps per machine. Add determinism test (two engines, same seed,
   identical output).

### Phase 2 — Core kit breadth

- Tom (pitch-swept sine, no drive — the kick with long pitch decay
  already does this, but a dedicated machine is cleaner).
- CP (clap: multi-burst noise envelope + body, ~20ms of repeated env
  triggers then tail).
- RS (rimshot: short 2-osc + noise tick).
- BD FM (2-op FM kick).
- SD FM (FM snare + noise).

### Phase 3 — Modulation + performance

- 2 LFOs per track (tri/sine/square/saw/exp/random, free/trig/one-shot
  modes, per-block update).
- Velocity-to-parameter routing (4 slots with depth).
- Choke/layer masks (u8 per track).
- `load_sound(track, &Sound)` per-trig = sound locks.
- Firmware CC map becomes table-driven.

### Phase 4 — Extended catalog

- HH Basic (6-detuned-osc 808 hat — the README already flags this as the
  right second iteration).
- Metallic CY (ring-mod oscillator cluster + HP noise).
- Metallic CB (2-osc cowbell, BP).
- HH Lab (6 separately tunable oscillators).
- Utility machines: Noise Gen (white noise + filter), Impulse (pings the
  track filter into tom-like tones — nearly free since SVF exists).
- SY Tone (2-op FM tonal synth).
- SY Dual VCO (dual analog osc tonal synth).
- SY Bits (bit/sample-rate reduction — can pull forward if lo-fi desired
  earlier).

### Phase 5 — Send FX (bench-gated)

- Static-buffer stereo delay (no tempo sync v1).
- Dattorro-plate-ish reverb.
- Per-track send levels (delay, reverb).
- FX bus overdrive.
- With 8 tracks at ~21% post-optimization, there is headroom for this.

### Phase 6 — Optional: sample machine

- SP Twinshot-style dual sample player.
- Kit loaded from SDIO into RAM at boot (README's existing suggestion).
- 16-bit/48kHz mono WAV, ~1MB holds a sensible kit.

## Settled decisions

| question | answer |
|---|---|
| Track count | 8, any machine anywhere |
| Macro surface | normalized 0..1 primary; human units derived internally |
| Machine dispatch | enum, not trait objects |
| Send FX | in scope, last phase, bench-gated |
| Sample machine | optional, Phase 6 |
| Sequencer | external (MIDI-driven); engine gains p-lock/sound-lock hooks |
| Machine ordering | kit-first (Phase 2 before tonal machines) |
| Sin table | mandatory, Phase 1 first task |
| teensy4-bsp version | stay on 0.5 for bench; bump to 0.6 when SAI work begins |

## Hardware

- Teensy 4.1 (600MHz Cortex-M7, 1MB RAM, microSD on SDIO)
- PCM5102A I2S breakout (for audio output only — not needed for bench)
- The analog/digital track split on Syntakt is about physical circuits;
  meaningless here. Any machine loads on any track.