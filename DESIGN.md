# Design

Long-lived design decisions for the drum engine. For the benchmark process that
governs whether a change is affordable, see `BENCHMARKS.md`. For the still-active
MI drum stage-substitution work, see `PLAN.md`.

## Constraints

- `#![no_std]`, no `alloc`, `f32` only, compile-time sized.
- Engine crates know nothing about hardware.
- Host renderer + firmware share the same engine code unmodified.
- Deterministic output: identical input + parameters produce bit-identical
  output on host and target.
- CI cross-compiles for `thumbv7em-none-eabihf` and runs clippy on every push.

## Architecture

```text
DrumEngine
 +-- tracks: [Track; 8]
 |    +-- slot: MachineSlot          enum dispatch, match in tick()
 |    +-- macros: [f32; 8]          0..1, stored user-facing state
 |    +-- strip: TrackStrip          SVF + filter env + AHD amp + drive + pan
 |    +-- mod: ModState              2 LFOs + routing table + velocity slots
 |    +-- choke/layer masks: u8
 +-- sends: SendFx                   delay + reverb
 +-- output routing                  MasterOnly or Multi (8 channels)
 +-- midi: parser + note/CC map tables
```

## Key decisions

### Macros are primary

Machines store 8 normalized macros (0..1, CC-friendly) plus a `set_macros()`
that derives internal coefficients (Hz, seconds, gains) via documented mapping
curves. Human-unit param structs are derived state, not stored state. This is
what makes generic host sweeps possible without a hand-maintained enum.

### Enum dispatch, not trait objects

`enum MachineSlot { BdClassic(..), BdFm(..), SdNatural(..), ... }`, matched in
`tick()`. Memory = largest machine × 8 — a few KB.

### Control-rate pass

Once per 32-frame block: advance LFOs, sum modulation onto macros, recompute
coefficients only for dirty params. The audio callback stays thin.

### Sound struct

A `Sound` is `{ machine_id, macros, strip_params }` — `Copy`, ~100 bytes.
`load_sound(track, &Sound)` at trig time gives sound locks. Pool of 128 is ~13KB.

### Envelopes and filters

- Track amp stage: `AhdEnv` (attack, hold, decay).
- Machine-internal envelopes: `DecayEnv`.
- Track filter: TPT state-variable filter (LP/HP/BP/notch), coefficients
  precomputed at control rate.
- One-pole filters stay inside machines where adequate.

### Sin table is mandatory

512-entry quarter-wave table with linear interpolation, ~2KB flash, ~10 cycles
per lookup. This is the single highest-leverage optimization and the gate for
scaling beyond 3 voices. Accessed through `fast::sin_turns`.

### Sample-accurate triggers

`TimedEvent { offset, event }` lets hits land at the exact sample inside the
block. `TimedQueue` drains at the top of `process()`. The firmware feeds
arrival-sample offsets from the audio sample counter.

## Device framework

The repo is structured for multiple devices on a shared framework:

```text
drumsynth/
├── core/                 `device-core` — Rust-only, no_std, no alloc
├── mi-dsp/               vendored MI C++ + Rust FFI wrappers
├── devices/
│   ├── drum/             existing engine crate
│   └── mi-drum/          Mutable Instruments drum device
├── render/               host binary with `--device` registry
└── firmware/             one crate, one bin per device
```

A device is one engine crate containing: a machine catalog
(`MachineId`/`MachineSlot`/`MACHINE_INFO`), an engine struct over
`core::Track<YourSlot>`, a default kit, and a `DeviceEngine` impl. The rest
(MIDI router, grid UX, firmware bin, render harness, bench scaffolding, SendFx)
comes through shared traits.

Static dispatch via the `Slot` trait; `Track<S, N>` monomorphizes. No trait
objects, no allocation.

## Mutable Instruments integration

- Vendored into `mi-dsp/vendor/`, not submodules.
- FFI boundary is **block-rate only**; no per-sample `extern` calls.
- C++ objects are placement-new'd into aligned Rust-owned storage; `unsafe` is
  confined to `mi-dsp`.
- Core and device crates stay `#![deny(unsafe_code)]`.
- Wrap one engine, bench `8×` worst case before adding breadth.

### mi-drum strip rate

MI stage classes are written block-wise (`Process(…, size)`), while the core
strip is per-sample. mi-drum uses a **segment** path: a run of the engine block
between timed-event offsets. The voice stays on its existing `tick()` drip to
preserve 24-sample Plaits phasing against the 32-sample engine block; the strip
is hoisted into per-stage loops over the segment. Same arithmetic in the same
order — bit-identical by construction.

### mi-drum strip topology

Phase 14 was redesigned from a per-track selectable stage catalog to a **fixed
strip**. The mi-drum chain is:

```text
Peaks or Plaits source → Warps → Ripples
                              ↗
                Stages modulation (6 LFOs + 3 AD envelopes)
```

- **8 tracks**: tracks 0–3 run Peaks drum models, tracks 4–7 run Plaits macro-
  oscillators.
- **Warps** is the audio-processing stage for every track. It is used as a
  mono processor: source feeds the carrier, modulator is normalised to the same
  signal, and the combined output continues down the strip.
- **Ripples** is implemented in Rust as a multimode SVF (low-pass / band-pass /
  high-pass with FM input). Mutable Instruments Ripples is an analog module,
  so the strip ports `stmlib::Svf` rather than vendoring C++ source.
- **Stages** provides modulation only: one Stages instance configured as 6 LFOs,
  another as 3 AD envelopes.
- Modulation patching is **static** in the first deliverable. LFOs 1–6 map to
  Ripples cutoff, Ripples FM, Warps timbre, Warps algo, Plaits morph, and
  Plaits timbre respectively. AD envelope 1 also maps to Ripples cutoff;
  AD envelopes 2 and 3 are unassigned. Depth per route is a macro parameter.
- **Clouds** lives on the shared send FX bus alongside delay and reverb. Each
  track gets a Clouds send amount.
- The 32 macro slots are reused/reinterpreted for the new modules; slot indices
  and the CC-map ABI do not change. Only one MOD bank of 8 CC-addressable slots
  is available for modulation, so many per-source parameters are not CC-mapped
  in the first deliverable. NRPN is the intended long-term fix.

`DeviceModel::macro_info` is still per-device; because the strip is fixed, the
grid does not need to relabel knobs when a "stage" changes. The old
`macro_info(&self, stages: StageConfig)` widening is no longer required.

### mi-drum determinism

`stmlib::Random` is one process-global LCG shared by every Plaits engine, not
one per voice. A render reproduces only from a known seed
(`mi_dsp::seed_random`), and two concurrent renders interleave draws and both
diverge. On target this is harmless today (one engine, fixed track order); a
per-voice generator would require patching vendored code.

The mi-drum baseline is a committed **FNV-1a digest**, not a WAV (`*.wav` is
gitignored). `cargo test` asserts it via `mi_drum_baseline_is_unchanged`. The
redesign invalidates the old 6-track Plaits digest; a new 8-track digest is
pinned once the engine stabilises.

## Output routing

- `MasterOnly` — stereo master bus, existing behavior, bit-identical host
  renders.
- `Multi` — 8 channels total, 4× PCM5102A breakouts on SAI1's 4 TX data lines.
  Ch 0/1 are master/wet (fixed); ch 2..7 are a per-track-routable pool.
- Per-track `Output`:
  - `Master` (default) — panned stereo sum feeds master, FX sends active.
  - `Channel(n)` — dry mono to channel `n`, removed from master, FX sends 0.
  - `Pair(a,b)` — dry stereo to a pair, removed from master, FX sends 0.
- Tap point is post-strip, post-fader, pre-send.

## Settled decisions

| question | answer |
|---|---|
| Track count | 8, any machine anywhere |
| Macro surface | normalized 0..1 primary; human units derived internally |
| Machine dispatch | enum, not trait objects |
| Send FX | delay + reverb + Clouds; bench-gated; always advanced per block |
| Sequencer | external (MIDI-driven); engine has p-lock/sound-lock hooks |
| Sin table | mandatory, `fast::sin_turns` |
| Sample-accurate triggers | built via `TimedQueue` + `schedule_midi` |
| Transport | USB MIDI device on the Deluge's host port |
| VA machine family | `BridgedT` lives in `dsp/`; BD VA is the first machine |
| Multi-output | 8 channels = 4× PCM5102A on SAI1 TX data lines |
| FX machines (sustained) | self-timed internal `AhdEnv`; no NoteOff path |
| Device crate layout | `core` + per-device engine crates |
| Firmware crate layout | one firmware crate, one bin per device |
| Render tool layout | one binary, `--device` flag with device registry |
| MI source | vendored into `mi-dsp/vendor/` |
| MI FFI boundary | block-rate only |
| MI catalog growth | bench-gated; wrap one, measure `8×` before breadth |
| MI drum strip | fixed Warps → Ripples with Stages modulation; segment-based strip |

## Anti-decisions (do not do these)

- Do not use trait objects or dynamic dispatch in the hot path.
- Do not add `std` or `alloc` to engine crates.
- Do not put `unsafe` in `core` or device crates; confine it to `mi-dsp`.
- Do not reintroduce `libm::sinf` into the per-sample audio path.
- Do not change existing macro slot indices or CC-map layout (binary ABI).
- Do not make mi-drum strip changes without the bit-identity gate.
