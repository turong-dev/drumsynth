# Plan: Phase 14 — MI drum engine redesign

Active plan for the `mi-drum` device. Earlier project history, architecture
decisions, and benchmark process have moved to `DESIGN.md` and `BENCHMARKS.md`.

*Goal:* replace the current 6-track Plaits-only `mi-drum` with an 8-track
Mutable-Instruments drum engine built around a **fixed strip**:

```text
source → Warps → Ripples
        ↗          ↗
Stages A         (modulation)
(6 × LFO)        (3 × AD envelope)
```

- **8 tracks**: 4 Peaks drum models + 4 Plaits macro-oscillators.
- **Source stage**: one Peaks drum or one Plaits voice per track, selectable by
the track's machine/macro slot.
- **Audio strip**: every track runs through **Warps** then **Ripples**.
- **Modulation strip**: every track owns **two Stages modulators**:
  - **Stages A** configured as **6 LFOs**.
  - **Stages B** configured as **3 AD envelopes**.
- **Send FX bus**: a **Clouds** granular/textural processor sits on the shared
send FX bus alongside the existing delay and reverb. Each track gets a Clouds
send amount.
- **Static patching** for the first deliverable; a user-configurable mod matrix
comes later.

This replaces the previous Phase 14 "per-stage catalog" idea (independent
selectors for env, colour, drive, LFO, source, send FX) with a single, rich,
fixed topology. The segment-based block-rate architecture from the earlier plan
is kept because every MI module here is block-rate.

## Why this shape

The spike in the previous plan showed that freely selectable MI stages could
not all run on six tracks within the cycle budget: two cheap stages already
pushed the worst case past the ~70% ceiling, and `Resonator` was 10× cheaper as
a send than per track. That made the catalog approach a product minefield.

The new design fixes the topology so that:

- the voice and the strip are always the same code path — no combinatoric
  explosion of machine × stage;
- Warps + Ripples + Stages are the actual Mutable Instruments modules people
  associate with a complete MI voice, not arbitrary stage substitutions;
- modulation is first-class from the start, rather than retrofitted through a
  selector system;
- the strip cost is bounded and predictable per track.

## The fixed strip in detail

### Source

Each track carries exactly one of:

- **Peaks drum** — the existing `peaks/drums/` models (bass drum, snare, hi-hat,
  etc.). These are cheaper than Plaits and give the device a drum-machine core.
- **Plaits macro-oscillator** — the existing 24-model Plaits catalog.

Track assignment is currently **fixed**: tracks 0–3 are Peaks, tracks 4–7 are
Plaits. Whether the voice type becomes selectable per track is a follow-up
product decision; the macro slot for machine selection still exists.

### Warps

Mutable Instruments Warps runs after the source. In the Eurorack module it is a
cross-modulation / wavefolder / ring-modulator / VCA stage with a selectable
algorithm. In this strip it is used as a **mono processor**: the voice output
feeds the carrier input, the modulator input is normalised to the same signal,
and the combined result becomes the strip output. This gives the wavefolding
and ring-modulation character without requiring a separate modulator source.

### Ripples

The **Ripples** position in the strip is a multimode resonant SVF implemented
in Rust in `core/dsp/`. Mutable Instruments Ripples is an analog module with no
DSP source in the open-source repo, so the strip uses a port of `stmlib::Svf`
(low-pass / band-pass / high-pass with FM input) instead. It follows Warps and
provides the final colour stage.

### Stages modulation

Mutable Instruments Stages provides the modulation sources. One Stages instance
per track is configured as **6 independent LFOs**; the second is configured as
**3 AD envelopes**.

Static routing (initial deliverable):

| Source | Target |
|---|---|
| LFO 1 | Ripples cutoff |
| LFO 2 | Ripples FM |
| LFO 3 | Warps timbre |
| LFO 4 | Warps algo |
| LFO 5 | Plaits morph |
| LFO 6 | Plaits timbre |
| AD env 1 | Ripples cutoff |
| AD env 2 | unassigned |
| AD env 3 | unassigned |

The depth of each routed modulation is a macro parameter. The mod matrix that
lets the user repatch sources and targets is explicitly out of scope for the
first deliverable.

## Macro surface

The device still exposes exactly `NUM_MACROS = 32` slots; the CC-map ABI and
slot indices are unchanged. mi-drum reinterprets those slots for the new strip.
The full surface (Warps, Ripples, Stages, Clouds, per-target modulation depths)
cannot fit in 32 CC-addressable slots, so **NRPN is the intended long-term
solution**. A second MOD page with additional parameters will be possible once
the macro system moves to NRPN. Until then, the plan does the best it can with
the existing CC map: one MOD bank of 8 slots, prioritising the global and
grouped-depth controls that shape the static routings.

The exact allocation is part of the implementation work, but the categories are:

- **MACH bank (0–7)**: voice-specific parameters.
  - Peaks: model, pitch, timbre, decay, etc.
  - Plaits: model, harmonics, timbre, morph, FM amount, etc.
- **FILT bank (8–15)**: Warps and Ripples parameters.
  - Warps algorithm, Warps timbre.
  - Ripples cutoff, resonance, FM amount.
  - Remaining slots for stage-specific colour controls.
- **TRACK bank (16–23)**: track routing and mixing.
  - Machine select, output, pan, level, delay/reverb sends.
- **MOD bank (24–31)**: modulation parameters.
  - Stages LFO rate(s), AD attack/decay.
  - Modulation depths for the static routings.

Because Peaks and Plaits do not share the same voice parameters, the MACH bank
is interpreted per voice type. The machine selector already switches the
underlying voice, so this is consistent with the existing model.

### Rough macro mapping

This is the working draft. It will tighten during implementation, but it gives
a sanity-check that 32 slots can cover the new surface.

| Slot | Bank | Proposed meaning |
|---|---|---|
| 0 | MACH 0 | Voice pitch / tune |
| 1 | MACH 1 | Voice timbre / harmonics / tone (voice-type-specific) |
| 2 | MACH 2 | Voice timbre or decay (voice-type-specific) |
| 3 | MACH 3 | Voice morph or FM / noise amount (voice-type-specific) |
| 4 | MACH 4 | Voice-specific param 4 |
| 5 | MACH 5 | Voice-specific param 5 |
| 6 | MACH 6 | Voice-specific param 6 |
| 7 | MACH 7 | Voice decay / release |
| 8 | FILT 0 | Warps algorithm |
| 9 | FILT 1 | Warps timbre |
| 10 | FILT 2 | Ripples cutoff |
| 11 | FILT 3 | Ripples resonance |
| 12 | FILT 4 | Ripples FM amount |
| 13 | FILT 5 | Ripples mode (LP/BP/HP) or reserved |
| 14 | FILT 6 | Reserved |
| 15 | FILT 7 | Reserved |
| 16 | TRACK 0 | Machine select |
| 17 | TRACK 1 | Output routing |
| 18 | TRACK 2 | Pan |
| 19 | TRACK 3 | Level |
| 20 | TRACK 4 | Delay send |
| 21 | TRACK 5 | Reverb send |
| 22 | TRACK 6 | Clouds send |
| 23 | TRACK 7 | Reserved |
| 24 | MOD 0 | LFO master rate |
| 25 | MOD 1 | LFO master depth |
| 26 | MOD 2 | AD attack |
| 27 | MOD 3 | AD decay |
| 28 | MOD 4 | LFO depth → filter (cutoff + FM) |
| 29 | MOD 5 | LFO depth → Warps (timbre + algo) |
| 30 | MOD 6 | LFO depth → Plaits (morph + timbre) |
| 31 | MOD 7 | AD depth → cutoff |

The MOD bank cannot cover every modulation parameter. In particular, the
independent rates for LFOs 2–6 and the attack/decay of AD envelopes 2–3 have no
CC slot in this draft. They will default to sensible values and become
addressable once the macro surface moves to NRPN.

### Parameters waiting for NRPN

The following controls are intentionally not on the 32-slot CC map. They will
live on a second MOD page (and any further pages) once NRPN replaces the fixed
CC layout:

- Independent LFO rates for LFOs 2–6.
- Independent LFO depths for each of the six routed targets.
- AD envelope 2 and 3 attack/decay and depths.
- Clouds quality/density parameters.
- Warps/Ripples secondary parameters (e.g. Ripples mode, Warps carrier/modulator
  balance if exposed).

## Block-rate segment architecture

Every MI stage class is written block-wise, while `device_core::Track::tick` is
per-sample. The settled "block-rate FFI only" rule means `mi-drum` keeps its own
segment path:

```text
for each segment (bounded by event offsets, at most BLOCK long):
    fill dry[..n] from slot.tick()      unchanged — preserves voice phasing
    warps.process(dry, n)               1 call
    ripples.process(dry, n)             1 call
    stages_a.render(lfos, n)            1 call
    stages_b.render(envs, n)            1 call
    apply static modulation             sample-wise over the segment
    level / pan / sends                 sample-wise over the segment
```

`MiSlot`'s buffer-and-drip `tick()` stays, for the same reason as before: the
voice is block-rate internally and `tick()` is just a buffer read. The strip is
what becomes block-rate. Per-sample behaviour currently in `Track::tick` — the
choke fade-out ramp and the retrigger de-click crossfade — becomes a sample-wise
pass over the segment buffer, using the same constants.

## Vendor additions needed

The vendored tree already contains Plaits and `peaks/drums/`. The new design
needs:

- **`warps/`** — full Warps DSP source, under the same vendoring decision as the
  rest of the MI tree (copy + LICENSE, not submodules).
- **`stages/`** — full Stages DSP source. Confirm the license header; Stages is
  open-source Mutable Instruments code with the same MIT/CC terms as the
  existing vendored tree.
- **`clouds/`** — full Clouds DSP source for the send FX bus. Same vendoring
  decision; Clouds is larger than the per-track modules, so it lives on the
  shared send bus rather than per track.

Ripples is **not vendored**: the hardware module is analog and has no DSP source
in the open-source repo. The strip's filter is a Rust port of `stmlib::Svf` in
`core/dsp/`.

Each needs a Rust wrapper in `mi-dsp/src/` with aligned storage, placement-new
init, and a block-rate `process` call. `mi-dsp/src/stages.rs` currently holds
the Phase-14-spike wrappers (`Lpg`, `Overdrive`, `Resonator`); it should be
renamed or moved so that `mi_dsp::stages` can refer to the Stages module.

## Benchmark expectations

The previous 6-track Plaits-only worst case was **262,515 cycles (65.6%)**.
The redesign adds more tracks and three per-track MI modules, so the budget is
likely to be challenged.

Rough order-of-magnitude estimate for the full 8-track, all-modulators-active
worst case:

| Component | Count | Per-unit guess | Subtotal |
|---|---|---|---|
| Plaits voice | 4 | ~44,000 cy | ~176,000 cy |
| Peaks drum voice | 4 | ~5,000–15,000 cy | ~20,000–60,000 cy |
| Warps per track | 8 | ~2,000–5,000 cy | ~16,000–40,000 cy |
| Ripples SVF (Rust) per track | 8 | ~500–1,500 cy | ~4,000–12,000 cy |
| Stages (2× per track) | 8 | ~3,000–8,000 cy | ~24,000–64,000 cy |
| Clouds send FX | 1 | ~20,000–50,000 cy | ~20,000–50,000 cy |
| **Total** | | | **~260,000–382,000 cy** |

Mid-point: **~325,000 cycles = ~81% of budget**. That exceeds the informal
~70% ceiling before any headroom for parameter smoothing, note parsing, or
future additions.

What this means for the phases:

- **14.1 skeleton** must prove the segment restructure is free.
- **14.2 Warps + Ripples** and **14.3 Stages** must be measured as they land.
  If the per-track strip alone pushes the worst case over the ceiling, the
  scope must be reduced (e.g. fewer active modulators, lower Clouds quality,
  or a hard voice-count limit) before 14.4 adds Peaks.
- **14.4 Peaks voices** are expected to be cheaper than Plaits, so they may
  actually help the average case even though the track count rises to 8.
- **Clouds** is the biggest unknown. It must be measured as a send in 14.5;
  if it dominates, it may need a quality/oversampling trade-off or a separate
  feature gate.

The plan assumes the first real measurement in 14.2 will decide whether the
ceiling can be held or whether the budget target itself has to move.

## Historical context: what was done before the redesign

The following work from the previous Phase 14 direction is retained and still
valid:

- **Thread-safe Plaits buffer pool** (14.0-pre).
- **Pool sized per target** — 8 buffers on firmware, 128 on host (14.0-pre).
- **`render mi-drum`** baseline renderer and FNV-1a digest gate (14.0-pre).
- **`mi-bench` firmware binary** and the `tools/benchloop.py --bin` support
  (14.0-pre).
- **FlexRAM rebalance** to ITCM 8 / DTCM 8 (14.0-pre).
- **Spike measurements** for LPG, Overdrive, Resonator-as-send, and the finding
  that per-call setup dominates small-block cost.

What changes with the redesign:

- The **6-track Plaits baseline** and its pinned digest are invalidated. A new
  baseline will be pinned once the 8-track engine is stable.
- The **per-stage selector mechanism** (`StageKind` on `SLOT_FILT_0`, the spike
  wrappers) is replaced by the fixed strip.
- The **future sub-phases** (14.1 env, 14.2 colour, 14.3 drive, 14.4 LFO, 14.5
  source, 14.6 send FX) are replaced by the phases below.

### Known issue carried forward

`SixOp1`/`SixOp2`/`SixOp3` render at ~-84 dBFS — the problem predates mi-drum
and is independent of the redesign. It remains a separate investigation.

## Phase breakdown

### Phase 14.0 — Vendoring and wrappers

Bring Warps, Stages, and Clouds into `mi-dsp` with no audio-path integration.
Each module compiles, has a Rust wrapper, and passes a minimal smoke test
(initialise, process a block, output is finite). Rename or relocate the spike
stages file so the name `stages` refers to the Stages module. Ripples is not
vendored; its DSP is a Rust SVF port in `core/dsp/`.

*Gate:* cross-compile succeeds; unit tests for each wrapper pass.

### Phase 14.1 — MiStrip skeleton

Segment-based strip with **no new modules**: the segment loop, sample-wise
choke/de-click passes, and the fixed parameter plumbing. The source is still the
existing Plaits-only voice; Warps/Ripples SVF/Stages are stubbed to pass-through.

*Gate:* mi-drum output bit-identical to the pre-redesign render, bench delta
within the 50-cycle noise floor. This proves the restructure is free.

### Phase 14.2 — Warps and Ripples SVF

Wire Warps into the strip and add the Rust SVF port for the Ripples position.
No modulation yet. Establish the new macro layout for the FILT bank.

*Gate:* sound changes predictably with Warps algo/timbre and Ripples cutoff/
resonance/FM; no NaNs; output bounded; bench delta reported.

### Phase 14.3 — Stages modulation

Add the two Stages modulators (6 LFOs + 3 AD envelopes) and the static routing
to the six targets. Add modulation-depth macros.

*Gate:* each routed target responds to its source; static map is correct;
bench delta reported for worst case (all 8 tracks, all modulators active).

### Phase 14.4 — Peaks drum voices

Add the Peaks drum voice path for tracks 0–3. The track source becomes voice-
type-aware. Plaits tracks remain unchanged.

*Gate:* all 8 tracks render; Peaks voices trigger and decay; combined bench
under the budget ceiling.

### Phase 14.5 — Clouds send FX

Add a Clouds instance on the shared send FX bus with a per-track Clouds send
amount. Clouds is vendored and wrapped alongside the per-track modules in 14.0,
but it lands last because it is the heaviest unknown in the budget estimate.

*Gate:* Clouds produces audible wet output when sent to; no NaNs; bench delta
reported for the worst-case send configuration.

### Phase 14.6 — New baseline and kit

Pin the new 8-track baseline digest, update the default kit to use Peaks drums
on tracks 0–3 and Plaits voices on tracks 4–7, and update `mi-bench` scenarios.

*Gate:* `mi_drum_baseline_is_unchanged` passes with the new digest; bench
worst-case under the ~70% ceiling; `MiDrumEngine` size still fits OCRAM.

### Phase 14.7 (future) — Mod matrix

User-configurable modulation patching. Not part of the first deliverable.

## Gates

Bench and RAM are gated **per sub-phase**, because the new architecture adds
three MI modules to every track:

- Every sub-phase reports a delta against 14.1 (the skeleton).
- The standing worst case is **8 tracks active, every modulator active, Clouds
  send fully driven**.
- The worst case stays under the Phase 13.5 ceiling of ~70% of the 400,000-cycle
  budget at 600 MHz, block 32, or the project explicitly revises that ceiling
  based on measured data.
- RAM asserted in the bench. `MiDrumEngine` already lives in cached OCRAM; the
  new modules grow it further. The 500 KB test cap may need revisiting.

The old 6-track Plaits baseline is intentionally broken by design; a new
8-track digest is pinned in 14.6.

## Risks and mitigations

| Risk | Mitigation |
|---|---|
| 8 tracks with three MI modules each exceed cycle budget | Per-sub-phase bench gates; if the worst case exceeds the ceiling, voice count or modulation density is reduced before merging |
| 32 macro slots cannot cover all new parameters | Prioritise the most useful global and grouped-depth controls in the single MOD bank; NRPN is the intended long-term fix |
| Vendoring Warps/Ripples/Stages widens the license surface | Same MIT/CC terms as the existing vendored tree; verify per file |
| Segment restructure changes the sound | 14.1 is a pure-refactor phase with a bit-identity gate before any module is added |
| Peaks and Plaits have incompatible patch/modulation structs | Track source becomes an enum; keep the strip interface mono-in/mono-out so the voice type is encapsulated |
| Stages configuration (6 LFOs / 3 envelopes) is not directly supported by the upstream code | Investigate the segment API first; if it cannot be coerced, document and choose a supported configuration |
| Clouds dominates the send FX budget | Measure as a send in 14.5 with quality/density controls exposed; be prepared to gate it behind a lower-quality mode or a per-kit enable |

(End of file)
