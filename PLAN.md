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

*(Pre-implementation estimate — measured Phase 5 numbers came in ~2x
higher than the "8 tracks + sends" row; see the Phase 5 bench results
below for what actually shipped and why.)*

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

### Phase 1 — Re-architecture + sin table (DONE)

- [x] `fast::sin_turns` table swap: 512-entry quarter-wave, linear interp,
      const-fn Taylor-built. Dropped 3-voice from 125,907 cy (31.5%) to
      18,699 cy (4.7%).
- [x] Machine trait + `MachineSlot` enum. Ports: BD Classic, SD Natural,
      CH/OH Classic.
- [x] `Track` struct + `Strip` (SVF, AHD, drive, equal-power pan, level).
- [x] Macro layer: 8 normalized knobs/machine, names/abbrevs/defaults,
      `set_macro` does CC-rate coefficient recompute.
- [x] Control-rate-pass scaffolding (per-block dirty-flag recompute lives
      behind `Track::set_macro` / `Track::set_strip`).
- [x] Renderer: generic `sweep <machine> <macro>`, `machine <name>
      --macro K=V`, `render` (8-track pattern). `SweepParam` enum deleted.
- [x] MIDI: note-to-track map on `DrumEngine`, CC-to-(track, macro)
      table-driven in firmware.
- [x] Tests: 62 green (38 ported + 24 new — SVF, AHD, choke/layer, pan law,
      macro mapping, MIDI routing, NaN at extreme macros).
- [x] Bench: 8-track worst case = **17.0%** of budget (68,090 cy/block).
      Idle 1.7%, 3-voice 9.0%. Headroom validated for Phase 2-5.

### Phase 2 — Core kit breadth (DONE)

- [x] Tom (pitch-swept sine + stick transient, no drive)
- [x] CP (clap: multi-burst noise env + FM body, BurstEnv walks 3 on/off
      crunch segments then decays)
- [x] RS (rimshot: 2 detuned oscillators + noise tick)
- [x] BD FM (2-operator FM kick, `tick_with_phase_bias` on SineOsc for
      phase-modulation FM)
- [x] SD FM (FM snare + noise, same FM phasor as BD FM)
- [x] Default kit updated: BdClassic, SdNatural, Hat (closed), Hat (open),
      Cp, Tom, Rs, BdFm — with OH/CH choke relation.
- [x] MIDI note map expanded: 35/36/37/38/39/42/46/50 → tracks 0..7.
- [x] 83 tests green (21 new machine tests). Fmt + clippy clean.
- [x] Bench rebuilt: 85KB text. Awaiting hardware measurement.

### Phase 3 — Modulation + performance (DONE)

- [x] `dsp/lfo.rs`: 2 LFOs per track, 6 waveforms (tri/sine/square/saw/
      ramp/exp), 4 modes (Free/Trig/Hold/OneShot), single dest + bipolar
      depth, block-rate advance.
- [x] `ModDest` covers machine macros (0..7) + strip params (FilterCutoff,
      FilterReso, Drive, Pan, Level, AmpDecay).
- [x] Per-block `Track::control()`: advances LFOs, sums LFO + velocity mod
      onto base macros/strip, recomputes coefficients (dirty-flag gated).
      Fast no-op when no modulation is active.
- [x] Velocity modulation: 4 slots per track (`VelMod { dest, depth }`),
      applied at trigger time, sustained through the hit.
- [x] `Sound` struct: `{ machine_id, macros, strip }`, Copy. `DrumEngine::
      load_sound` + `trigger_with_sound` = sound locks.
- [x] Choke/layer masks still handled by `DrumEngine::trigger` (from Phase 1).
- [x] Renderer demo: slow sine LFO on snare filter cutoff + velocity→drive
      on kick.
- [x] 96 tests green (5 new: LFO macro mod, velocity→cutoff, Sound round-trip,
      trigger_with_sound, control no-op). Fmt + clippy clean.
- [x] Bench rebuilt: 100KB text. Awaiting hardware measurement.

### Phase 4 — Extended catalog (DONE)

- [x] HH Basic (6-detuned-osc 808-style hat, square-via-sign-of-sine)
- [x] CY Metallic (ring-modulated 2-osc cymbal + HP noise transient)
- [x] CB Classic (2-osc cowbell, square pair through bandpass)
- [x] SY Tone (2-op FM tonal synth with modulator feedback, exp2 pitch)
- [x] Catalogue now 12 machines. Renderer `MachineArg` updated.
- [x] 114 tests green (18 new machine tests). Fmt + clippy clean.
- [x] Bench rebuilt: 105KB text. Awaiting hardware measurement.

### Phase 5 — Send FX (DONE)

- [x] Static-buffer stereo delay (`dsp/fx/delay.rs`): 24 KB stereo ring
      buffer, 500 ms max, `time/feedback/tone/mix`, two independent
      one-pole tone LPs in the feedback path. Feedback clamped to 0.98.
- [x] Dattorro-plate-ish reverb (`dsp/fx/reverb.rs`): predelay → 2 allpass
      diffusors → 6 damped combs per side. ~42 KB total. Tank sizes
      deliberately different for L vs R so the stereo image spreads
      without cross-feedback. No random state — fully deterministic.
- [x] Per-track send levels (`send_delay`, `send_reverb` on `StripParams`).
      Post-fader, like a mixer aux. Cached as `eff_send_*` on `Track`.
- [x] FX bus overdrive (`SendFx.drive`) — wet sum soft-clipped pre-master.
- [x] `DrumEngine.send_fx: SendFx` owned next to `tracks`. `process()`
      accumulates sends into per-block stack buses, runs delay + reverb
      block-by-block, sums wet into master before the master clip.
- [x] `ModDest::SendDelay` / `ModDest::SendReverb` wired through
      `Track::control()` (LFO + velocity mod). End-of-control push of
      `eff_*` to `self.eff_*` so a strip change while mod is active
      propagates even when the LFO targets something else.
- [x] Renderer demo: snare → reverb (ambient space), clap → delay
      (slap-back), cowbell → light reverb. 5.7s demo WAV.
- [x] Bench rebuilt with two FX scenarios ("8 FX idle" for the empty-FX
      overhead, "8 + FX" for the realistic worst case). 270 KB hex
      (~9 KB flash growth from Phase 4).
- [x] Bench measured on hardware (2026-08-06), steady state:

      ```
      idle       avg=  92540 cy  peak=  92646 cy   23.2% of budget  (2895 cy/frame)
      3 sounding avg= 149982 cy  peak= 150161 cy   37.5% of budget  (4692 cy/frame)
      8 sounding avg= 272397 cy  peak= 272476 cy   68.1% of budget  (8514 cy/frame)
      8 FX idle  avg= 272330 cy  peak= 272408 cy   68.1% of budget  (8512 cy/frame)
      8 + FX     avg= 272330 cy  peak= 272408 cy   68.1% of budget  (8512 cy/frame)
      ```

      (First loop iteration reads a little low on the two FX rows —
      271,514 / 272,293 cy — before settling at the above; cache warm-up,
      not signal. Everything past iteration 1 repeats to the cycle.)

      Three things stand out:

      - **"8 sounding" and "8 + FX" are statistically the same number.**
        The two tracks actually routed to sends (snare→reverb,
        clap→delay) cost nothing measurable on top of running FX at all —
        the fixed per-block cost of ticking the delay line and reverb
        tank dominates completely over the per-sample send math.
      - **"8 FX idle" ≈ "8 + FX" too** — FX are always advanced every
        block regardless of whether any track sends to them (see
        `SendFx`/`Track::process`), so there is no such thing as a cheap
        "FX present but unused" state. Budgeting for FX means budgeting
        for FX running flat out, always.
      - **The estimate table above was optimistic by ~2x.** It projected
        120-150k cy / 30-38% for "8 tracks + sends"; measured is 272k cy /
        68.1% — the same shape of miss as the pre-sin-table `sinf`
        estimate in Phase 0, just smaller magnitude. `idle` alone (92.5k
        cy, no track sounding, FX ticking on silence) is already higher
        than the whole Phase 1 8-track *sounding* figure of 68,090 cy —
        the FX block-rate cost, not the per-voice cost, is now the
        dominant term.

      68.1% leaves real but not generous headroom — `bench`'s own
      70%-warning threshold (`firmware/src/bin/bench.rs`) sits 2 points
      above the current worst case. MIDI parsing, `set_macro` (`expf`
      calls), and any future per-block work (Phase 6 sample playback,
      more sends) all have to fit in the remaining ~32%, and the delay
      line's 24,000-sample buffer is the biggest single lever left if that
      gets tight (see Phase 6/backlog).
- [x] 127 tests green (8 new FX tests + 5 new send-routing tests).
      Delay round-trip, feedback decay, tone attenuation, NaN at extremes;
      reverb impulse → tail, stereo from mono, no-runaway, NaN at extremes;
      send routing for delay and reverb independently; LFO modulates
      send_delay; bit-identical re-render determinism. Fmt + clippy clean
      (only pre-existing warnings remain in lfo.rs / sy_tone.rs).

### Phase 6 — Optional: sample machine

- SP Twinshot-style dual sample player.
- Kit loaded from SDIO into RAM at boot (README's existing suggestion).
- 16-bit/48kHz mono WAV, ~1MB holds a sensible kit.

### Phase 7 — Note layer (DONE)

- [x] Per-machine `retune(semis)` on all 12 machines: scales every oscillator
      the voice owns so the sweep, FM ratio, and detune all move together.
      Noise-only machines (Hat Classic) no-op. Absolute, not incremental.
- [x] Each machine stores an internal `freq_scale` so `set_macros` re-applies
      the transpose — a later macro CC recompute keeps the note. Verified by
      tests on BdClassic and SyTone.
- [x] `MachineSlot::retune` dispatch + `Track::retune` passthrough — control
      rate, never in the per-sample path.
- [x] `dsp::fast::semitone_ratio` (exp2f under the hood, same speed family as
      the existing `exp2_approx`; control-rate calls are negligible).
- [x] `SineOsc::freq()` getter added so FM/sweep machines can read the current
      carrier back for scaling.
- [x] Renderer: per-step semitone lane on `Pattern` (track 7 bassline), the
      firmware-style "retune before trigger" discipline.
- [x] 134 tests green (3 new: BdClassic retune+recompute, SyTone octave
      transpose, Track-level transpose that survives a macro recompute).
      Fmt + clippy clean (only pre-existing lfo.rs / sy_tone.rs warnings).

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