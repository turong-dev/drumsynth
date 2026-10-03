# Plan: Phase 14 — MI drum engine redesign

Active plan for the `mi-drum` device. Earlier project history, architecture
decisions, and benchmark process have moved to `DESIGN.md` and `BENCHMARKS.md`.

*Goal:* replace the current Plaits-only `mi-drum` with a 6-track
Mutable-Instruments drum engine built around a **fixed strip**:

```text
source → Warps → Ripples
        ↗          ↗
Stages A         (modulation)
(6 × LFO)        (3 × AD envelope)
```

- **6 tracks**: 4 Peaks drum models + 2 Plaits macro-oscillators.
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

Track assignment is currently **fixed**: tracks 0–3 are Peaks, tracks 4–5 are
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

The Plaits-only 6-track baseline this plan started from was **262,515 cycles
(65.6%)** peak — `6 sounding` in `bench-results/mi-baseline.json`, before any
strip module. The strip adds three MI modules per track, so the budget is worth
taking seriously even at six tracks.

Restated for the shipped shape — 4 Peaks + 2 Plaits, 2 LFOs + 2 AD envelopes
per track, plus Clouds on the send bus. Per-unit figures marked *measured* are
derived from committed bench JSON, not guessed: a Plaits voice from
`6 sounding − idle` in `mi-stages-chain.json`, one Stages segment from the
`6 + LPG` delta (LPG *is* a Stages segment, so it is a direct proxy).

| Component | Count | Per-unit | Subtotal |
|---|---|---|---|
| Fixed engine + send bus, every track silent | 1 | ~29,700 cy (measured) | ~29,700 cy |
| Plaits voice (tracks 4–5) | 2 | ~38,900 cy (measured) | ~77,800 cy |
| Peaks drum voice (tracks 0–3) | 4 | ~5,000–15,000 cy (est.) | ~20,000–60,000 cy |
| Warps (per track) | 6 | ~2,000–5,000 cy (est.) | ~12,000–30,000 cy |
| Ripples SVF, Rust (per track) | 6 | ~500–1,500 cy (est.) | ~3,000–9,000 cy |
| Stages segment, 2 LFO + 2 AD (per track) | 24 | ~2,100 cy (measured) | ~50,400 cy |
| Clouds send FX | 1 | ~20,000–50,000 cy (est.) | ~20,000–50,000 cy |
| **Total** | | | **~213,000–307,000 cy** |

Mid-point: **~260,000 cycles = ~65% of budget**. That sits just under the
informal ~70% ceiling, with Clouds as the one unmeasured term. The earlier
8-track version of this table came out at ~325,000 / ~81%, i.e. over the
ceiling — **the cut to six tracks is what brought it back**, not a change in
the per-unit costs.

The deferred 6 LFO + 3 AD target is not free in cycles either: 9 segments per
track is 54 instances, ~113,000 cy, i.e. **+~63,000 over the 2 + 2 that
shipped**, putting the worst case at ~276,000–370,000 cy (69–93%) — over the
ceiling. It has to find cycles as well as bytes.

What this means for the phases:

- **14.1 skeleton** must prove the segment restructure is free.
- **14.2 Warps + Ripples** and **14.3 Stages** must be measured as they land.
  The measured `idle` figure above is the floor: it is the cost of the send
  bus plus the strip with nothing sounding, and nothing can go below it.
- **14.4 Peaks voices** are estimated at ~5,000–15,000 cy against a measured
  ~38,900 cy for a Plaits voice. Under the original 8-track shape that made
  tracks 4–7 cheap and the extra track nearly free; under six tracks it is
  only two tracks' worth of saving, and the count no longer rises to absorb it.
  This is an estimate, not a measurement — the first `6 sounding` bench with
  the Peaks kit loaded is what settles it.
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
  baseline will be pinned once the 6-track engine is stable.
- The **per-stage selector mechanism** (`StageKind` on `SLOT_FILT_0`, the spike
  wrappers) is replaced by the fixed strip.
- The **future sub-phases** (14.1 env, 14.2 colour, 14.3 drive, 14.4 LFO, 14.5
  source, 14.6 send FX) are replaced by the phases below.

### Resolved: the Plaits gate is a real gate now

**Status: fixed.** The gate is held high from note-on to note-off instead of
being a one-block pulse. All four open sub-decisions below were answered and
implemented; the reasoning is kept because the shape of the fix is not
obvious from the code.

**What was wrong.** `MiSlot::render_if_needed` set
`self.modulations.trigger = if triggered { 1.0 } else { 0.0 }` — high for one
`VOICE_BLOCK` (24 samples, 0.5 ms), then low for the rest of the note. Plaits
reads that as the *gate input level* and derives `p.trigger` from it
(`voice.cc:143`-`:150`).

**Only three engines died, and the reason is a two-contract mismatch.** Eight
Plaits engines are registered `already_enveloped = true`, which switches off
Plaits' outer LPG/envelope (`voice.cc:230`) and makes each engine responsible
for its own amplitude. Six of those read the gate as an *edge*, three read it
as a *level*:

| engine | reads | 1-block pulse |
|---|---|---|
| `bd` `sd` `hh` `string` `modal` | `trigger & TRIGGER_RISING_EDGE` | survives — fires once, runs its own decay |
| **`SixOp1` `SixOp2` `SixOp3`** | `trigger & TRIGGER_HIGH` | **silence** — the envelope closes 24 samples in |

`SixOpEngine` sets `p->gate = (trigger & TRIGGER_HIGH)` and feeds it straight
into the FM operator envelopes, which rest at exactly zero
(`fm/envelope.h:71` parks the envelope in the release stage at level 0.0).

**The fix, and why holding the level is enough.** `voice.cc:97`-`:152` derives
*both* the level and the edge from the same trigger value, with a 1 ms delay
and 0.3/0.1 hysteresis. Holding it high therefore needs no edge-detection
work of our own: `TRIGGER_HIGH` stays set for the note and
`TRIGGER_RISING_EDGE` still fires exactly once, on the first block where the
delayed value crosses 0.3. Measured on the same voice, held rather than pulsed:

| engine | before | after |
|---|---|---|
| SixOp1 | digital silence | 0.50 sustained at 8 s, gate held |
| SixOp2 | digital silence | 0.48 at 50 ms, own envelope ends it at ~1 s |
| SixOp3 | digital silence | 0.40 at 250 ms, 0.17 still at 8 s |

**How the sub-decisions landed.**

- *Where does note-off come from?* Wired end to end: `MidiEvent::NoteOff`
  (from `0x8n` **and** zero-velocity `0x9n`, which is how most controllers
  report a release), `EngineEvent::NoteOff` scheduled through the same
  `TimedQueue` as a note-on, `DeviceEngine::release_channel`, `Track::release`,
  and `Slot::release` with a no-op default.
- *Is the gate policy per voice type?* **No — one uniform policy, and the gate
  also closes when the voice's own envelope runs out.** This turned out to be
  better than splitting by voice type: a one-shot drum model triggered from a
  grid that never sends a note-off ends the note when its own envelope does, so
  the drum behaviour is unchanged with no special-casing, and a sustained engine
  holds until the key comes up. The per-track `is_active` early-out is what
  makes this work, and it stays intact.
- *Does the envelope replace the internal Warps carrier behaviour?* Not
  addressed here. The strip multiplies nothing; the gate is the voice's own
  amplitude authority, and Warps' carrier is a separate question that is still
  open.
- *Length source.* The watchdog, at 10 s. `MACH 7` is still dead on the eight
  `already_enveloped` engines (`EngineParameters` has no `decay` field; the
  only two uses are `voice.cc:156` and `:237`, both inside the `!lpg_bypass`
  branch), so it was not used for this. Confirmed by measurement: setting
  `MACH 7` to 0.5 and 0.9 produces byte-identical output on `SixOp1`.

**What is still not fixed, deliberately.** The gate gives the note an *end*,
not a release *time*. On Plaits, `trigger` pings the outer LPG
(`voice.cc:241`, `ProcessPing`) and the LPG then decays on `patch.decay`, so
dropping the gate starts the release without setting its length. Measured: 22
of 24 Plaits engines fall silent within 3 s of note-off; `chiptune` keeps
ringing and ignores the gate entirely. A macro-controlled release time is the
dedicated amplitude envelope below, and it is a separate change.

### Still open: the dedicated amplitude envelope

The gate work above made sustained notes possible. It did not make the
*release* a controllable parameter, and that is what this decision is for.
One envelope, in the strip, whose output is the voice's amplitude authority.
It must do **two** jobs, not one, and the second is the one that is easy to
miss:

1. **Amplitude.** A sample-wise multiply in the strip, alongside the existing
   `env1`/`env2` Stages segments. Cheap — they already render per-segment
   buffers.
2. **Gate hold.** The envelope's length is also how long the Plaits gate is held
   high. A strip-only volume envelope does **not** fix SixOp: the internal FM
   envelope would still be clipped to 24 samples, and the strip would just be
   shaping a click. Holding the gate for the envelope's length is what makes
   the engine sound, and the strip multiply is what makes it fade.

Unifying them is the point — one control, one length, one shape, and the edge
still fires on note-on so the edge-triggered engines keep working unchanged.
Job 2 is now done by the gate; job 1 is what remains, and `MACH 7` is the
obvious home for its length on the eight engines where that macro is currently
a no-op.

Two measurements to keep in mind when sizing it:

- Multiplying in the strip also gates Warps' free-running internal carrier and
  the reverb send, since those sit downstream of the same buffer. Probably
  correct, but it is a behaviour change and it will be audible on the carrier
  pass.
- It must not double-shape the two drum machines that now have a real release.
  `Track::release` deliberately does not touch the strip amp envelope for
  exactly this reason.

**Unrelated but found alongside:** `kMaxEngines` is 24 in the vendored
`voice.h` while `voice.cc` calls `RegisterInstance` 28 times, so the last four
are silently dropped and indices 24–27 clamp to 23 (hi-hat) — measured, all
four return hi-hat. `MiMachineId` maps the four Peaks ids to exactly 24–27, so
anything that routes a Peaks id into the Plaits path gets a hi-hat with no
error. Safe today only because the quantiser clamps; `EngineRegistry::Init`
never nulls `engine_[]`. A SIGSEGV in `Voice::Render` (null `Engine*`, engine
index 8) was hit once and vanished after an unrelated rebuild, which is the
signature of the uninitialised-read bug below rather than of this one.

**Also measured, not a gate issue:** `particle`, `additive` and `speech` hold a
note but are very quiet (1e-5 to 8e-5 at 2 s held). They are not silent; they
are just low, and that is a level question, not a gate question.


## Phase breakdown

> **Track count: 6, not 8.** The original plan specified 8 tracks (4 Peaks +
> 4 Plaits). The decision was cut to **6 tracks — 0–3 Peaks, 4–5 Plaits** —
> after measuring the engine at 474,864 B with all six tracks, and deferring
> the send-FX question to 14.5. Eight tracks in this shape does not fit: the
> send bus alone is 262,656 B (192,000 B stereo delay + 70,656 B reverb), so
> Clouds cannot be added without removing or externalising both.
>
> The cycle and RAM estimates in this document have since been redone against
> six tracks, so the "8 track" numbers that remain in prose below are the
> *phase gates* still written for the 8-track shape and are what the next
> sub-phase has to correct. The landed `DEFAULT_KIT` is already the 6-track
> split, and `TRACKS = 6` in `mi-drum-engine`.

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
bench delta reported for worst case (all 6 tracks, all modulators active).

#### As built (2026-09-26): 2 LFOs + 2 AD envelopes, not 6 + 3

The plan's source count is not affordable, and the reason is worth recording
because it is not obvious from the hardware. One Mutable Instruments Stages
module is six *segments*, each with its own DAC output. The vendored
`stages::SegmentGenerator` models **one segment**, i.e. one output. So "two
Stages modules per track" is nine `SegmentGenerator` instances per track, not
two — there is no multi-output class to collapse them into, and a wrapper owning
six of them costs the same six.

At 4,184 bytes each, 9 segments × 6 tracks is ~226 KB. The engine with no
Stages instances at all measures 374,448 B, so 6 LFO + 3 AD lands at ~600 KB —
past both the 500 KB cap and the 512 KB OCRAM limit, and 54 segments instead of
24 is ~+63,000 cycles per block on the cycle side. 14.3 therefore ships **2
LFOs + 2 AD envelopes** (4 instances/track, 24 total) and the 6 + 3 target
becomes a follow-up that needs memory *and* cycles back from somewhere.

Two consequences for the macro map, both forced by the 32-slot ceiling:

- The MOD bank's 8 slots are fully consumed, so `LFO.DEPTH` is a master scalar
  on the LFO bus and the per-target depths are what actually route.
- MOD 0..7 collide with the core `Track` LFO1/LFO2 slots, so `Track::set_macro`
  skips its strip/LFO interception when `strip_bypass` is set. Without that the
  slot never sees its own depth macros and the routes are silently dead — the
  first symptom was a modulation route that changed nothing at all.

The static map that shipped, all four sources routed:

| Source | Target | Depth macro |
|---|---|---|
| LFO 1 | Ripples cutoff (±3 octaves) | `LFO.FILT` |
| LFO 2 | Warps timbre | `LFO.WRP` |
| AD env 1 | Ripples cutoff (±3 octaves) | `AD.FILT` |
| AD env 2 | Warps timbre | `AD.WRP` |

`Ripples mode` and the LFO 2 → Ripples FM route are still unbuilt. `Ripples mode`
is now free to add — FILT 7 is the last free `resv()` in the FILT bank and
`SvfMode` already has `Bp`/`Hp`/`Notch` implemented and tested in `core`.

### Warps internal carrier (landed with 14.3)

`Parameters::carrier_shape` selects one of Warps' five internal oscillators
(sine, triangle, saw, pulse, band-limited noise) *in place of* the external
input, with the input as its FM index and a MIDI note as the centre pitch. The
shim hardcoded `carrier_shape = 0`, so the module could only cross-modulate its
own input. `WARP.CAR` on FILT 6 now selects it, quantised over six positions
with 0.0 keeping the old behaviour, and the oscillator is pitched from the
voice's own note so Warps tracks pitch. Costs no RAM.

Two corrections to earlier claims in this document, both worth keeping:

- Warps does **not** have nine reachable algorithms. The front-panel
  `ModulationAlgorithm` enum lists `SPECTRAL`, `MORPH` and `VOCODER`, but the
  DSP-side `ALGORITHM_` enum is trimmed to six plus `NOP`, because those three
  all route through the vocoder whose member is commented out for memory. The
  existing 0..1 mapping already spans all six.
- `Modulator`'s constructor is empty and `Init` only seeds
  `previous_parameters_`, so `parameters_` was read as whatever was in the
  placement-new storage — uninitialised OCRAM in the firmware. `mi_warps_init`
  now seeds it.


Two DSP details that cost real debugging time and are easy to get wrong again:

- **Stages LFOs must be driven free-running.** Passing *any* gate array,
  including an all-low one, switches `SegmentGenerator::Process` onto its
  gate-clocked ramp extractor. The segment then sits near-constant and every
  depth routed to it is dead. `Stages::process_free_running` passes a null
  pointer; `process` is for gate-clocked segments only.
- **Do not average the LFO across a Warps chunk.** Warps is limited to 32
  samples and interpolates its own parameters between calls, so the modulator
  is point-sampled at the chunk start. Averaging a 32-sample window against a
  ~1.4 Hz LFO cancels most of the sweep — the route measured exactly zero.

The `SEGMENT_ALT` shape is already bipolar; the AD envelopes are unipolar. The
shim also had to learn `GATE_RISING`/`GATE_FALLING` — it folded everything
non-zero into `HIGH`, which left one-shot AD envelopes stuck at their start.

#### Memory headroom after 14.3

`MiDrumEngine` was **473,520 bytes** at 14.3 against the 500 KB cap — **26,480
bytes spare**, about 6 `SegmentGenerator` instances total, i.e. **one more per
track** if all six go to modulation. It is **474,864 bytes / 25,136 spare**
today, the difference being the `PeaksVoice` that 14.4 added to every slot.
Clouds (14.5) does not fit in that and must live outside the engine. Measured,
not estimated — the number is printed by `engine_size_fits_ocram_budget`.

### Phase 14.4 — Peaks drum voices

Add the Peaks drum voice path for tracks 0–3. The track source becomes voice-
type-aware. Plaits tracks remain unchanged.

*Gate:* all 6 tracks render; Peaks voices trigger and decay; combined bench
under the budget ceiling.

#### Vendoring: closed 2026-09-26

The missing files were in **`pichenettes/eurorack`**, not `pichenettes/peaks`.
`gate_processor.h`, `resources.h` and `resources.cc` are now vendored verbatim
and all four models compile and sound. Two traps are recorded in
`docs/peaks-vendoring.md`:

- `GateFlags` is a **Peaks-local `uint8_t`** with its own bit values, not
  `stmlib::GateFlags`, and `ControlMode` is two states, not three. A shim that
  passes `stmlib` gate flags through will mistrigger.
- Peaks has **no gain staging, no limiter and no per-voice velocity**, and none
  is being bypassed: `peaks.cc:116` sends the model straight to the DAC. The
  models peak at full scale by design (a saturating drum circuit is the sound),
  so no trim is needed.
- **Velocity is deliberately ignored for now.** `Slot::trigger(velocity)` has
  nothing to drive, and the obvious fix — scaling it onto the output — is a
  commitment that cannot be unwound later without changing how the voice sounds.
  Gain-scaled velocity is not the same as excitation-scaled velocity, and
  `Excitation::Trigger(level)` sits right there accepting a level, so the better
  answer may be to drive that instead. Deciding with a Peaks voice in context
  beats committing now. Peaks voices therefore take velocity and discard it, and
  `LEVEL` is the only amplitude control. This is an explicit exception to the
  `velocity-scales` rule in AGENTS.md, pinned by a test so it reads as a known
  gap rather than a bug. See `docs/peaks-vendoring.md` for the three options
  when it is revisited.

`resources.cc` is 376 KB and only ~5 of its tables are used; the rest is
`wav_digits` and wavefolding tables belonging to the display engine. Vendored
whole to keep the tree a faithful copy; it is the obvious trim if flash gets
tight. Not compiled until the Rust wrapper lands, to avoid adding 40 KB of dead
data to the image for nothing.


#### Memory: the plan is to give Peaks tracks 1 LFO + 1 AD envelope

Per-track Stages instances dominate the budget, and each is 4,184 B. Measured
component sizes, all `std::mem::size_of` on the wrappers as they stand:

| | bytes |
|---|---|
| one Stages segment (`SegmentGenerator`) | 4,184 |
| one Plaits voice (`PlaitsVoice`) | 12,304 |
| one Peaks voice (`PeaksVoice`, any of the four models) | 193 |
| one Warps | 4,112 |
| one Clouds processor (buffers not included) | 9,096 |
| one `Track` as landed (2 LFO + 2 AD) | 35,280 |
| non-track overhead (`Engine` minus the six `Track`s) | 263,184 |
| **`MiDrumEngine` as landed** | **474,864** |

**Correction to an earlier claim in this document:** the Peaks/Plaits split
does *not* move the memory needle. Every slot carries a `PlaitsVoice` *and* a
`PeaksVoice` unconditionally — the loaded machine decides which one sounds — so
a Peaks track costs exactly what a Plaits track costs. Peaks voices are ~64×
cheaper to store than Plaits voices, and that is a *cycle* win, not a RAM one.
What the split actually costs in RAM is nothing, and what the earlier version
of this table got wrong (it credited "4 Plaits + 440" and read the 193-byte
figure as an aggregate) is that it traded a saving that does not exist.

Modelling the engine as `tracks × Track + 263,184`, with one Stages segment at
4,184 B inside each `Track`:

| config | tracks | Stages segments | `MiDrumEngine` | vs 500 KB cap |
|---|---|---|---|---|
| **6 tracks (4 Peaks + 2 Plaits), 2 LFO + 2 AD — as landed** | 6 | 24 | **474,864** (measured) | **25,136 spare** |
| 6 tracks, 1 LFO + 1 AD | 6 | 12 | ~424,656 | ~75,344 spare |
| 6 tracks, no modulation | 6 | 0 | ~374,448 | ~125,552 spare |
| 8 tracks, 2 LFO + 2 AD | 8 | 32 | ~545,424 | **45,424 over** |
| 8 tracks, 1 LFO + 1 AD | 8 | 16 | ~478,480 | ~21,520 spare |

The first row is measured; the rest are arithmetic on the measured component
sizes, and `engine_size_fits_ocram_budget` prints the real number.

The second row is the plan, and the reason for it has changed. At six tracks
2 LFO + 2 AD *already fits*, so 1 + 1 is no longer about feasibility — it buys
**~50 KB of headroom** for Clouds and for the 6 + 3 modulation follow-up. It is
also musically defensible: MI's own Peaks module has no modulation concept at
all, just a raw parameter array, so one LFO and one envelope on a drum voice
is already generous. Note the last row: 8 tracks is only reachable at 1 + 1, so
if the track count ever goes back up, that is the configuration it requires.

**Clouds must not live inside the engine.** Its buffers are 118,784 + 65,536 B
plus a 9,096 B processor — ~193 KB, which no row above absorbs. It is a send-bus
effect, not per-track, so it belongs in a firmware `.uninit` static fed from the
engine's existing `wet_l`/`wet_r` output, leaving one macro slot (`TRACK 6`, now
a free `resv()`) for the send amount. Decide this before 14.5; if Clouds ends up
inside `SendFx` the whole plan is dead.


### Phase 14.5 — Clouds send FX

Add a Clouds instance on the shared send FX bus with a per-track Clouds send
amount. Clouds is vendored and wrapped alongside the per-track modules in 14.0,
but it lands last because it is the heaviest unknown in the budget estimate.

*Gate:* Clouds produces audible wet output when sent to; no NaNs; bench delta
reported for the worst-case send configuration.

### Phase 14.6 — New baseline and kit

Pin the new 6-track baseline digest, update the default kit to use Peaks drums
on tracks 0–3 and Plaits voices on tracks 4–5, and update `mi-bench` scenarios.

*Gate:* `mi_drum_baseline_is_unchanged` passes with the new digest; bench
worst-case under the ~70% ceiling; `MiDrumEngine` size still fits OCRAM.

**Unblocked, and the digest is pinned.** Three things had to happen first.
The uninitialised reads are fixed, so a digest is worth pinning at all. The
Plaits gate is real, so the three `SixOp` engines are no longer digital
silence in the sweep. And the render itself had drifted: the machine sweep and
the kit pattern both ran on Warps' internal carrier, so five of six tracks and
all 28 machine hits were an oscillator rather than the voice under test — the
carrier is External for both now, with the five internal carriers covered by
their own pass, and the kit plays one bar at `WARP.DRV` 0 before the same bars
driven, so the baseline carries its own reference for what the strip does.

`BASELINE_DIGEST` is `0x7b5d_4ef0_174f_d745`, verified identical across 20
separate processes. The `DEFAULT_KIT` half of this phase was already done in
`100ade8`.

### Phase 14.7 (future) — Mod matrix

User-configurable modulation patching. Not part of the first deliverable.

## Resolved: mi-drum rendered differently in every process

**Status: fixed.** Two vendored voices read state their `Init` never wrote.
The baseline digest is re-pinned and the 14.6 gate is unblocked.

### What it was

- `peaks::HighHat::Init` never initialised `uint32_t phase_[6]`, the six
  square-oscillator phases.
- `plaits::SyntheticBassDrum::Init` left out `transient_env_` and
  `transient_env_lp_`. The second is a `ONE_POLE` accumulator read and
  rewritten from its own previous value on the first sample, and assigned
  nowhere else.

Neither is a bug upstream. On hardware both objects are zeroed statics whose
`Init` runs once at boot, so the fields are zero because the BSS is. Here the
engine is heap-allocated and a slot is re-initialised whenever its machine
changes, so the forgotten fields picked up whatever the previous model left at
those addresses — including, when those bytes had held a pointer, values that
move with ASLR. Hence: deterministic within a process, different in every new
one, and sensitive to `MallocScribble`.

Both fixes are marked `LOCAL FIX` in the vendored tree and written up in
`docs/peaks-vendoring.md` and `docs/plaits-vendoring.md`. Re-apply them if the
vendored tree is refreshed.

### How it was found, since the next one will hide the same way

The earlier bisect stalled because it was reasoning about *subsystems*. What
worked was localising in *time*: render twice into two WAVs, find the first
differing sample, and map that offset onto the render's own section timeline.
That pointed at one 0.75 s machine window rather than at a subsystem.

1. Two renders, diff the PCM, find the first divergent frame. It was 15.76 s,
   which is machine 21 of the sweep — `mi-bd`.
2. Digest each machine in isolation across three processes: only `mi-bd`.
   Fixing it left `pk-hh` as the only unstable window, and `pk-hh` diverged
   from the *first sample* of its window.
3. `pk-hh` was stable on a fresh engine and unstable after any other machine
   had been loaded — including after loading `pk-hh` itself. That is the
   signature of re-initialisation over dirty storage, which named the fault
   precisely.

Everything else that looked unstable was downstream contamination: `pk-hh` is
the quietest machine in the catalogue, so a send-FX tail carrying divergence
from an earlier window shows up there first.

### Checking it stays fixed

The failure was *between* processes, so one green run proves little:

```bash
for i in $(seq 1 20); do
  cargo test -q -p render mi_drum_baseline 2>&1 | grep -oE "got 0x[0-9a-f]+"
done | sort | uniq -c
```

No output means every run matched. More than one distinct digest means
something is reading uninitialised memory again.

`devices/mi-drum/tests/slot_reuse.rs` is the in-process guard: every machine
must render identically on a clean slot and on a slot that has held something
else. It is in its own test binary because it compares renders exactly, and
`stmlib::Random` is a process-global generator that other rendering tests
would interleave with. It catches the hi-hat class. It does **not** catch the
bass drum one — by the end of a previous hit that accumulator has decayed to
approximately zero, so in-process the stale and correct values are
indistinguishable. Poisoning the allocation does not help either: a slot is
built on the stack and moved into place, so the fill never reaches the
vendored storage. For that class the multi-process digest check above is the
only backstop.

### Tooling notes, kept

ASan reports no out-of-bounds or use-after-free here and structurally cannot
see uninitialised reads. MSan is the tool that names the bytes and is
Linux-only. Neither was needed in the end; a WAV diff and a section timeline
were enough, and are a lot cheaper to reach for.

If ASan is wanted for something else, this is the configuration that links:
instrument only the C++ and let clang supply the runtime. Instrumenting Rust
*and* C++ collides on `_asan.module_ctor`.

```bash
CFLAGS="-fsanitize=address -fno-omit-frame-pointer -g" \
CXXFLAGS="-fsanitize=address -fno-omit-frame-pointer -g" \
RUSTFLAGS="-C link-arg=-fsanitize=address" \
cargo test -p render
```

## Measured: Warps is the strip's whole character, and its drive is also its mix

Asked by ear ("the render sounds crusty"), answered by rendering the same kit,
pattern and seed with progressively less of the Warps stage in the path and
differencing the results against the Warps-free version:

| variant | residual vs no Warps |
|---|---|
| `WARP.DRV` 0 (the bypass detent) | **-93.6 dBFS**, peak 0.0001 |
| Warps' DSP bypassed, shim's int16 round trip still in | -93.6 dBFS |
| `WARP.DRV` 0.15 on every track | -24.0 dBFS, peak 1.36 |
| `WARP.DRV` 0.50-0.80, as the render's kit sets it | **-14.1 dBFS**, peak 1.49 |

Three things follow.

- **The shim's `f32 -> int16 -> f32` round trip is not audible.** It is -93.6
  dBFS, i.e. ordinary 16-bit quantisation, and it is present even when Warps is
  bypassed because the conversion happens either side of `Process`. It was the
  obvious suspect and it is not the cause.
- **`WARP.DRV = 0` is genuinely transparent**, to that same -93.6 dBFS floor.
  The detent works.
- **There is no subtle setting in between, because Warps has no mix control
  at all.** `Modulator::Process` is 100% wet: `channel_drive[0..1]` go only to
  `amplifier_[i].Process(...)`, the per-input `SaturatingAmplifier`
  (`modulator.cc:210`-`:222`), and nothing downstream blends the input back
  in. A -24 dBFS residual at `WARP.DRV` 0.15 is therefore not a partly-wet
  signal, it is a fully wet one that happens to be lightly driven. Combined
  with the measured rolloff (10 kHz is -7 dB at light drive and -17 dB at
  full, `mi-dsp/src/warps.rs`), an engaged Warps makes every track
  substantially darker and more saturated, and the only way to dial it back is
  the bypass detent.

  *(An earlier version of this section claimed drive doubled as a dry/wet mix,
  citing `wet_dry` at `modulator.cc:175`. That line is inside
  `ProcessEasterEgg`, the frequency shifter, which this build never enables.
  The measurements above were unaffected — only the mechanism was misread.)*

If Warps is wanted as a colour rather than a transform, the strip has to
supply the mix Warps does not have: keep the dry chunk before `warps.process`
and blend after, leaving `WARP.DRV` to control saturation only. Not done.

### The algorithm knob is not the lever, and two of its positions are dead

Measured the same way — one bar of the kit per algorithm at the shipped
`WARP.DRV` of 0.2, differenced against the same kit bypassed:

| `WARP.ALG` | mode | residual | level | tilt |
|---|---|---|---|---|
| 0.000 | XFADE (the current default) | -23.5 dB | +0.8 dB | -0.1 dB |
| 0.125 | FOLD | -10.9 dB | +7.0 dB | +4.6 dB |
| 0.250 | ANALOG RING MOD | -19.2 dB | +2.3 dB | -3.2 dB |
| 0.375 | DIGITAL RING MOD | -12.1 dB | +3.1 dB | -5.1 dB |
| 0.500 | XOR | -23.1 dB | -1.4 dB | +1.5 dB |
| 0.625 | COMPARATOR | -23.3 dB | +0.9 dB | -0.1 dB |
| 0.750 | NOP | -23.3 dB | +0.9 dB | -0.1 dB |

**The default is already the gentlest**, tied with COMPARATOR and NOP, so there
is no subtler algorithm to move to. The two to stay away from are FOLD and
DIGITAL RING MOD. The spread between best and worst is ~13 dB of residual,
against the ~79 dB that separates bypass from the kit's own drive settings —
the algorithm is a detail, the drive is the effect.

The reason so many modes collapse onto the same numbers is structural:
`Warps::process` passes the same buffer as **both** the carrier and the
modulator (`mi-dsp/src/warps.rs`), so every cross-modulation is a signal
against itself. A comparator fed two identical inputs has nothing to compare.
Warps is built for two different signals and is being given one.

Two knobs are flat as a result:

- **`WARP.ALG` above 0.75 does nothing.** `modulation_algorithm` is scaled by 8
  and clamped to 5.999 (`modulator.cc:250`), so the top quarter of the macro's
  travel is all the same mode. Same shape of bug as the old `RIP.CUT` top end.
- **`WARP.TIM` is a gain control at the default algorithm, not a timbre
  control.** `Xmod<ALGORITHM_XFADE>` returns `x_1 * fade_in + x_2 * fade_out`
  (`modulator.cc:299`); with `x_1 == x_2` that is `x * (fade_in + fade_out)`, a
  scalar that runs from unity at either end to +3 dB at centre. So **`LFO.WRP`
  and `AD.WRP` — two of the four shipped modulation routes — are tremolo**,
  not timbral modulation, until either the algorithm moves off XFADE or Warps
  is given a real second input.

The levers that would actually make Warps subtle-but-present, in order of
effort: blend dry/wet in the strip so `WARP.DRV` controls saturation only; or
feed the carrier from something other than the voice itself, which
`WARP.CAR` already does for the five internal oscillators.

**Smaller, real, not the cause:** the shim's input cast is
`(int16_t)(x * 32767.0f)` -- truncation toward zero, no dither, no clamp. Eight
samples in the baseline render exceed 1.0, which is UB on that cast. Warps' own
output is `Clip16`'d, so only the input side is exposed.

## Landed: the Warps stage has inputs, an output tap, and a mix

Three controls added and one dead one wired, in response to the measurements
above. Slot placement is deliberately expedient — the macro-to-CC map is being
replaced with NRPN next, so these took the three free slots rather than
logical ones.

| slot | macro | what |
|---|---|---|
| FILT 4 | `RIP.FM` | **was dead**, now audio-rate self-FM of the filter cutoff, +/-2 octaves |
| FILT 7 | `WARP.MIX` | dry voice vs Warps, default 0.35 |
| TRACK 6 | `WARP.IN` | Warps' modulator input: voice main output (0) to Plaits aux (1), default 1 |
| TRACK 7 | `WARP.OUT` | Warps' main output (0) to its aux output (1), default 0 |

`Warps::process_dual` is the new wrapper entry point: separate carrier and
modulator in, both outputs back. `MiSlot` captures Plaits' aux per sample in
`tick` rather than reading `block_aux` in the strip, because the engine
collects source samples interleaved across tracks and `block_pos` has moved on
by the time the strip runs.

**Not yet benched.** The strip gained a dry copy, a modulator build and two
blends per sample, plus a second FFI output buffer. `MiDrumEngine` is
**476,016 bytes** (was 474,864), still inside the 500 KB cap.

### Fixed alongside: `Warps` no longer breaks when moved

`warps::Modulator` holds pointers into its own buffers, so moving the Rust
wrapper left them stale and the next `Process` read freed stack. The engine
worked around this with a `warps_initialized` flag and a lazy `init` on the
first strip call; tests did not, and it faulted twice in one session — once
from adding a call frame, once from constructing in a test rather than in the
engine. `Warps` now records the address it was initialised at and re-inits if
it finds itself somewhere else: one pointer compare per chunk.
`surviving_a_move_is_the_wrapper_s_job` is the guard.

## Known gaps in the landed Peaks integration

`100ade8` added the Peaks voices but deliberately stopped short of three things
the design calls for. The fourth was found later, while measuring the rendered
baseline. All are additive.

1. **Peaks pitch is not chromatic.** The wrapper expects parameter 0 to be
   pitch, but `MiSlot::set_macros` currently places `MACH 1` there, so a Peaks
   track ignores the incoming note. Fix by deriving pitch from the note plus
   the TUNE offset, mapping `MACH 1..3` to the remaining model parameters, and
   reconfiguring on `retune`.
2. **"Tracks 0–3 are Peaks" is a default, not a constraint.** The machine
   selector still reaches all 28 machines on any track.
3. **Peaks tracks still run 2 LFO + 2 AD** rather than the intended 1 + 1.
4. **`pk-hh` is ~25 dB quieter than its neighbours.** Measured on the rendered
   baseline, the Peaks high hat peaks at -26.6 dBFS (rms -55.3) where the
   other three Peaks drums sit between -3.1 and -0.2. Not a bug: the model has
   no parameters of its own, so it inherits `default_macros`' `MACH 1..4` of
   0.30 / 0.50 / 0.30 / 0.0, and that block was written for the drum models
   rather than for a hat. A per-model default is the likely fix.

## Gates

Bench and RAM are gated **per sub-phase**, because the new architecture adds
three MI modules to every track:

- Every sub-phase reports a delta against 14.1 (the skeleton).
- The standing worst case is **6 tracks active, every modulator active, Clouds
  send fully driven**. Restated against six tracks the estimate lands at
  ~213,000–307,000 cy (~53–77%), mid-point ~65% — under the ceiling, but only
  because two Plaits voices became four Peaks voices and only because 14.3
  shipped 4 Stages segments per track instead of 9.
- The worst case stays under the Phase 13.5 ceiling of ~70% of the 400,000-cycle
  budget at 600 MHz, block 32, or the project explicitly revises that ceiling
  based on measured data.
- RAM asserted in the bench. `MiDrumEngine` already lives in cached OCRAM; the
  new modules grow it further. The 500 KB test cap may need revisiting.

The old 6-track Plaits baseline is intentionally broken by design; a new
6-track digest is pinned in 14.6.

## Risks and mitigations

| Risk | Mitigation |
|---|---|
| 6 tracks with three MI modules each exceed cycle budget | Per-sub-phase bench gates; if the worst case exceeds the ceiling, modulation density is reduced before merging — track count is already the floor, since 8 tracks does not fit in RAM at 2 LFO + 2 AD |
| 32 macro slots cannot cover all new parameters | Prioritise the most useful global and grouped-depth controls in the single MOD bank; NRPN is the intended long-term fix |
| Vendoring Warps/Ripples/Stages widens the license surface | Same MIT/CC terms as the existing vendored tree; verify per file |
| Segment restructure changes the sound | 14.1 is a pure-refactor phase with a bit-identity gate before any module is added |
| Peaks and Plaits have incompatible patch/modulation structs | Track source becomes an enum; keep the strip interface mono-in/mono-out so the voice type is encapsulated |
| Stages configuration (6 LFOs / 3 envelopes) is not directly supported by the upstream code | Investigate the segment API first; if it cannot be coerced, document and choose a supported configuration |
| The deferred 6 + 3 modulation target costs both RAM and cycles | 9 segments/track is 54 instances = ~226 KB and ~113,000 cy; needs memory and cycles recovered before it is in scope |
| The Plaits gate has no note-off, so a held gate may never release on a sustained engine | **Done.** Note-off is routed end to end and the gate is held for the note. All three `SixOp` engines sound again. See "Resolved: the Plaits gate is a real gate now" |
| Clouds dominates the send FX budget | Measure as a send in 14.5 with quality/density controls exposed; be prepared to gate it behind a lower-quality mode or a per-kit enable |

(End of file)
