# Design

Long-lived design decisions for the drum engine. For the benchmark process that
governs whether a change is affordable, see `BENCHMARKS.md`. For the still-active
MI drum fixed-strip work, see `PLAN.md`.

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

### The gate

A voice can be held for as long as a key is down, and the note-off that ends
it is routed all the way from the wire:

```text
MIDI 0x8n (or 0x9n vel 0) → MidiEvent::NoteOff
  → EngineEvent::NoteOff  (sample-accurate, via TimedQueue)
  → Engine::release_channel → Track::release → Slot::release
```

- `Slot::release` **defaults to a no-op**, and that default is the contract for
  a one-shot voice: a drum hit is not a gate, and a note-off arriving after the
  attack has passed must not shorten it. A voice overrides it only if it can
  actually sustain.
- `AhdEnv` has two hold modes. `Timed` is the original self-timed gesture.
  `Gated` waits for `release()` instead, which is what makes a voice
  sustained.
- **Every gate is watchdogged** (`GATED_MAX_HOLD_S`, 10 s). `is_active` is the
  engine's per-track cost gate, so a held gate keeps a track at full DSP
  price. A note-off that never arrives — a stuck key, a dropped cable, a drum
  grid that has no off state — would otherwise cost full price for the life of
  the device. The watchdog is a backstop for a missing event, not the note
  length; set it well above the longest note you intend to hold.
- A gate also ends when its voice does. If the output falls below the slot's
  silence floor for `SILENCE_BLOCKS`, the note is over and the gate closes with
  it. Without this, a one-shot drum machine triggered with no note-off would
  ring for the full watchdog after every kick.
- `Track::release` deliberately does not touch the strip amp envelope or the
  de-click window. The strip amp is a fixed A-H-D gesture the voice sits
  inside, not a second gate; layering a release on it would double-shape every
  note.

**What the gate does not do:** it does not give the release a *time*. On
Plaits, `modulations.trigger` pings the outer LPG (`voice.cc:241`,
`ProcessPing`) and the LPG then decays on `patch.decay` — dropping the gate
starts the release but does not set its length. For the eight
`already_enveloped` engines the amplitude belongs to the engine outright, and
for `SixOp1`/`2`/`3` the gate *is* the amplitude authority, so those do follow
the key. A macro-controlled release time is a separate change, and the
dedicated amplitude envelope in `PLAN.md` is the design for it.

### Sin table is mandatory

512-entry quarter-wave table with linear interpolation, ~2KB flash, ~10 cycles
per lookup. This is the single highest-leverage optimization and the gate for
scaling beyond 3 voices. Accessed through `fast::sin_turns`.

### Sample-accurate triggers

`TimedEvent { offset, event }` lets hits land at the exact sample inside the
block. `TimedQueue` drains at the top of `process()`. The firmware feeds
arrival-sample offsets from the audio sample counter. `NoteOff` is queued the
same way as `NoteOn` — a release applied between blocks rather than at its
offset would make a held note's length quantise to the block grid in one
direction only.

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
                Stages modulation (2 LFOs + 2 AD envelopes)
```

- **6 tracks**: tracks 0–3 run Peaks drum models, tracks 4–5 run Plaits macro-
  oscillators. Eight tracks (4 + 4) was the original Phase 14 shape and does not
  fit — at 4 Stages segments per track the engine is 545,424 B against a 500 KB
  cap, and the send bus is 262,656 B of the 268,560 B non-track overhead, so
  Clouds has nowhere to go. Six is the settled count; 8 is only reachable if
  Peaks tracks drop to 1 LFO + 1 AD.
- **Warps** is the audio-processing stage for every track, used as a
  two-input cross-modulator with a mono result:
  - **Carrier** is the voice's main output.
  - **Modulator** is `WARP.IN`, a crossfade between the voice's main output
    and **Plaits' aux output**. Plaits renders two outputs and the slot used
    to discard the second; feeding it here costs nothing and is the patch
    Warps is built for. Handing the same signal to both inputs is degenerate:
    a comparator has nothing to compare, and `ALGORITHM_XFADE` collapses to
    `x * (fade_in + fade_out)`, which makes `WARP.TIM` a trim rather than a
    timbre control and turns `LFO.WRP` / `AD.WRP` into tremolo. A Peaks voice
    has no aux, so a Peaks track stays self-modulated.
  - **Output** is `WARP.OUT`, a crossfade between Warps' main output (the
    cross-modulation) and its aux output, which in this path is the sum of the
    two *saturated inputs* — a drive-only tap.
  - **`WARP.MIX`** then blends the whole stage against the dry voice. Warps
    has no mix control of its own: `Modulator::Process` is 100% wet and
    `channel_drive` only feeds the input saturators, so without this the only
    way to dial Warps back was the `WARP.DRV = 0` bypass detent. Default 0.35.
- **`WARP.DRV` is remapped, not passed through.** Warps' `SaturatingAmplifier`
  computes pre-gain as `0.5·drive` blended towards `24·drive⁵`, with post-gain
  normalising it back, which has two consequences that are invisible in a
  number and very audible in a mix:
  - **`drive = 0` is silence, not clean.** So `WARP.DRV = 0` is a real
    `Modulator::set_bypass` — a bit-transparent copy — and is the only way to
    get an uncoloured section.
  - **The top half of Warps' knob covers 48× of gain.** That is where the
    "gets crazy past halfway" reputation comes from. The macro is rescaled
    `0.0..1.0` onto Warps' `0.50..1.00` so the knob is strictly monotonic and
    spends its whole travel on the usable part of the curve, with the
    destructive region at the top where you can see it coming.
  - Default is 0.2 — light colour, well clear of the destructive half.
  - Measured across the sweep (`render -- warps`): crest factor falls 7.1 →
    2.6 dB and the high-to-low spectral balance rises 4.3 dB from bypass to
    full drive, which is the saturation signature rather than a level change.
- **Ripples** is implemented in Rust as a multimode SVF (low-pass / band-pass /
  high-pass with FM input). Mutable Instruments Ripples is an analog module,
  so the strip ports `stmlib::Svf` rather than vendoring C++ source. `RIP.FM`
  is audio-rate, self-sourced cutoff modulation — the post-Warps sample
  displaces the cutoff it is about to be filtered by, +/-2 octaves at full
  scale. It shares the cutoff exponent with the two control-rate routes, so it
  costs an add rather than a second `powf`.
- **Stages** provides modulation only: 2 LFOs and 2 AD envelopes per track. The
  hardware module is six *segments*, each with one DAC output, and the vendored
  `stages::SegmentGenerator` models exactly one — so "6 LFOs + 3 AD envelopes"
  is 9 instances per track, not 2. At 4,184 B each that is ~226 KB and ~+63,000
  cycles/block on top of the 2 + 2 that ships. 6 + 3 is a follow-up that needs
  RAM and cycles recovered first.
- Modulation patching is **static** in the first deliverable. The four shipped
  routes are LFO 1 → Ripples cutoff, LFO 2 → Warps timbre, AD 1 → Ripples
  cutoff, AD 2 → Warps timbre. Depth per route is a macro parameter.
- **Clouds** lives on the shared send FX bus alongside delay and reverb. Each
  track gets a Clouds send amount. It is not landed yet.
- The 32 macro slots are reused/reinterpreted for the new modules; slot indices
  and the CC-map ABI do not change. Only one MOD bank of 8 CC-addressable slots
  is available for modulation, so many per-source parameters are not CC-mapped
  in the first deliverable. NRPN is the intended long-term fix.

`DeviceModel::macro_info` is still per-device; because the strip is fixed, the
grid does not need to relabel knobs when a "stage" changes. The old
`macro_info(&self, stages: StageConfig)` widening is no longer required.

### mi-drum gate

`MiSlot` holds the gate high from note-on to note-off. Before this it set
`modulations.trigger` high for a single 24-sample `VOICE_BLOCK` and low for
the rest of the note, which Plaits reads as a gate *level* as well as an edge
(`voice.cc:143`-`:150`). That starved the three `SixOp` engines, whose FM
operator envelopes rest at exactly zero and only rise while the gate is high:
they emitted digital silence for the whole hit, and three of the 28 machines
in the rendered baseline were exactly `-inf`. Holding the level fixes them and
gives every level-reading engine a note to sustain, while the edge still fires
once because Plaits derives both from the same value with hysteresis.

- Peaks gets real per-sample gate flags instead: `GATE_RISING` on the note-on
  sample, `GATE_HIGH` for the rest of the note, `GATE_FALLING` on the note-off
  sample. The shim translates these to Peaks' own bit values, which are **not**
  stmlib's — see `docs/peaks-vendoring.md`.
- The gate also closes when the voice's own envelope runs out, so a one-shot
  drum model triggered from a grid with no note-off still comes to rest.
- Release is the engine's own, not a cut. Dropping the gate starts Plaits' LPG
  decay and Peaks' gate-processor release.

### mi-drum determinism

`stmlib::Random` is one process-global LCG shared by every Plaits engine, not
one per voice. A render reproduces only from a known seed
(`mi_dsp::seed_random`), and two concurrent renders interleave draws and both
diverge. On target this is harmless today (one engine, fixed track order); a
per-voice generator would require patching vendored code.

The mi-drum baseline is a committed **FNV-1a digest**, not a WAV (`*.wav` is
gitignored). `cargo test` asserts it via `mi_drum_baseline_is_unchanged`. The
redesign invalidates the old 6-track Plaits digest; a new 6-track digest is
pinned once the engine stabilises. Until the uninitialised-read bug is fixed
the gate passes or fails by luck, so it must not be re-pinned in the meantime.

The gate change moved the digest again, deliberately: the render now includes a
held-note-and-release pass, and the machine windows it covers are no longer
silent. **The digest is still stale and has not been re-pinned**, for the
uninitialised-read reason above. `every_catalogued_engine_makes_sound` in
`mi-drum-engine` is the test that actually catches a regression here, and it
does not depend on the digest.

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
| Note-off | routed end to end; `Slot::release` is a no-op for one-shots |
| Sin table | mandatory, `fast::sin_turns` |
| Sample-accurate triggers | built via `TimedQueue` + `schedule_midi` |
| Transport | USB MIDI device on the Deluge's host port |
| VA machine family | `BridgedT` lives in `dsp/`; BD VA is the first machine |
| Multi-output | 8 channels = 4× PCM5102A on SAI1 TX data lines |
| FX machines (sustained) | gated: note-on opens, note-off releases; bounded by a watchdog |
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
