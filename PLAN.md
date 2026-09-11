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
  +-- outs: OutputMode                 Phase 8-C; per-track Output routing → 8-ch bus
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

### Phase 8 — Deluge integration: audio (SAI) + MIDI

The M3 headline from the design guidance. Two hardware plumbing pieces, split
so MIDI (testable now, and done in the engine) isn't blocked on audio (which
needs the PCM5102A + SAI work). Bench-gated: MIDI + CC handling must fit in
the remaining ~32% alongside the FX cost.

**8-A — Audio (SAI out)** — needs the PCM5102A hardware on hand.

- [x] SAI audio output: PCM5102A on BCLK/LRCLK/OUT1A, interleave `process()`
      output to I2S, engine in a `.uninit` static via `new_in_place`. The
      "bump teensy4-bsp to 0.6" trigger. Implementation in
      `firmware/src/audio.rs`:
  - 48 kHz clock chain (Teensy Audio Library numbers): PLL4 = 24e6 × (28 +
    6720/10000) = 688,128,000 Hz; SAI1_CLK = PLL4/4/14 = 12,288,000 Hz; BCLK =
    SAI1_CLK/4 = 3,072,000 Hz; 32-bit slots × 2 ch → LRCLK = 48,000 Hz.
  - `Sai::without_pins` (asymmetric pin set: RX clock pads 20/21 + TX data pad
    7, no MCLK pad) + `split(32, 2, Packing::None, i2s(bclk_div(4)))` with
    `SyncMode::TxFollowRx` — the RX half is the async clock master, matching
    the Teensy Audio Library. Sample is 16-bit left-justified in the 32-bit
    word (MSB at bit 31).
  - **FIFO-request interrupt, not DMA** (deviation from the earlier plan): the
    SAI driver's DMA only reaches the lowest-numbered data line and imxrt-dma
    has no half-transfer interrupt, so the circular double-buffer pattern isn't
    available. `#[no_mangle] extern "C" fn SAI1()` (overrides the weak
    `DefaultHandler` for vector 56) refills `write_frame_u32` until
    `Status::FIFO_REQUEST` clears; watermark 16 of 32 words → ~333 µs between
    interrupts. Revisit DMA in 8-C where 4 data lines are in play.
  - Threading (revised after the underrun fix): the **main loop owns the
    engine**; the ISR only plays from a double-buffered pair of pre-rendered
    blocks. At each block boundary it flips to the buffer the main loop
    filled via `audio::render_next`, then raises `RENDER_PENDING`. Rendering
    inside the ISR is why the aliasing bug happened — a whole `process()`
    (150–450 µs) cannot fit beside the ~16-frame TX FIFO (~167 µs of runway);
    the main loop has the full ~667 µs block period and the ISR preempts it
    freely to keep the FIFO fed. `sample_counter()` is block-aligned, so
    `arrival_offset` is 0 — block boundary is the earliest a note can play.
  - Wiring detail: SCK tied low (PCM5102 internal PLL); the SAI drives BCLK /
    LRCLK directly, no MCLK pin wired.
- [x] Audio callback in ITCM, hot buffers in DTCM. **DONE — and the premise
      above was wrong.** The ISR was never running from flash/OCRAM:
      `t4link.x` already aliases `REGION_TEXT` to ITCM and `REGION_BSS` to
      DTCM, so all code and the `AudioState` buffers were in fast memory from
      the start. The thing actually stranded in OCRAM was the *engine* —
      `#[link_section = ".uninit"]`, 266 KB of it, mostly the delay line and
      reverb tanks. Moving it to DTCM took the worst-case bench scenario from
      87.2% of budget to 39.3%; moving `fast::QUARTER` too took it to 35.6%.
      See "Optimisation pass" at the end of this file.
- [ ] **Worst-case render-deadline measurement** — the regression tripwire for
      the underrun bug. `bench` cannot see the bug: it times isolated
      `process()` calls and never the interaction between the main-loop render
      and the SAI ISR. Instrument the real path instead — `audio::render_next`
      stamps `DWT::cycle_count()` at the block boundary (in the ISR, before
      raising `RENDER_PENDING`) and again after `process()` returns, tracking
      the worst boundary→done gap against the 400,000-cycle block period. All
      DWT cycle counts, the same primitive `bench` already uses, plus one
      atomic stamp in the ISR — nothing about the audio path changes. Report
      worst gap, best margin below deadline, and how many blocks approached it;
      output over the existing `imxrt_log` USB serial (bench's channel) from a
      debug bin. Run under a full playing load (all 8 tracks + sends) so the
      measured margin is the real one. If a future change pushes the worst gap
      toward the deadline, this number and the LED's fast underrun blink agree.

**8-M — MIDI (USB device)** — the engine side is done; this is firmware.

- [x] USB MIDI device on the one bus the Teensy has. Built our own
      `UsbDevice` (BusAdapter + MIDI class in `firmware/src/usb.rs`) since
      `imxrt-log`'s `log::usbd` owns its whole stack, **and** since
      `usbd-midi` 0.2.0 (the newest on the usb-device 0.2 that `imxrt-usbd`
      0.2.2 is pinned to) only does host→device with wrong descriptors —
      0.5.1 is correct but needs usb-device 0.3. The class mirrors
      `imxrt-log`'s usbd.rs construction/poll so the two stay comparable.
      Enumerate on the Deluge as host (Deluge DC-powered, device connected
      before power-up). Verify on the bench whether the Deluge bus-powers
      the Teensy; powered hub if not.
- [x] Drain the USB MIDI parser from the main loop: `usb::poll` hands the raw
      bytes to the shared `MidiParser`, route NoteOn → `schedule_midi` with
      the sample-counter offset, CC/Panic → `handle_midi` (via
      `schedule_midi`'s immediate path). `apply_cc` stays out of the audio
      interrupt.
- [x] CC → macro smoothing (one-pole ~7.5 ms): **done in the engine** —
      `Track::set_macro_target` + block-rate slew in `control()`, so a CC
      burst costs one coefficient recompute per macro per block, not one per
      message. The firmware side is just `apply_cc`, which now calls it.
- [x] Set the FZ bit in FPSCR at firmware init — protects the long FX tails
      the engine's own `DENORMAL_FLOOR` clamp can't see.
- [x] Bench regate: `8+FX+CC` scenario drives `apply_cc` every block on top
      of playing + routed sends — the steady-state scenarios never exercise
      the CC recompute path. Numbers recorded on the bench (goal: still
      under ~70% of budget).

**8-C — Multi-output (4× PCM5102A, 8 channels, per-track routing)** —
optional, depends on 8-A.

The Syntakt's analog/digital split is, as the plan already notes, "meaningless
here" — but the *physical* part that *is* worth porting is individual outs to a
mixer. Extend 8-A from one stereo pair to an 8-channel output stage with
per-track routing, so any drum can leave the box on its own channel while the
master/wet pair is still present.

**Target topology — 8 channels total, 4 PCM5102A breakouts.** SAI1 on the
i.MXRT1062 exposes exactly 4 TX data lines (OUT1A..OUT1D), so 4 I2S lanes =
8 channels fit one peripheral cleanly — no SAI2, no second clock domain. All
4 boards share BCLK + LRCLK; each gets its own TX data line.

| board | ch | content |
|---|---|---|
| 0 | 0 / 1 | **master/wet** — stereo sum of all tracks routed to master, **post-send-FX** (delay + reverb wet land here) |
| 1 | 2 / 3 | individually-assigned tracks (mono into one ch, or a stereo pair spread here) |
| 2 | 4 / 5 | individually-assigned tracks |
| 3 | 6 / 7 | individually-assigned tracks |

The fixed point is the master stereo pair on 0/1. The other 6 channels are a
pool the tracks are routed into — not a fixed 1:1 track-to-channel mapping,
since 8 tracks > 6 spare channels and the user gets to choose which drums go
out individually and which stay on the master bus.

**Per-track output routing.** Each `Track` gets an `output` destination:

- `Output::Master` (default) — the track's panned stereo sum feeds the
  master bus exactly as it does today, FX sends active. Unchanged behaviour.
- `Output::Channel(n)` where `n ∈ 2..=7` — the track's **dry, post-strip,
  post-fader** mono sample (pan summed to mono) is written to channel `n`,
  **and removed from the master sum**, FX sends forced to 0. This is the
  Syntakt/Rytm convention: an individual-out track has left the main bus,
  so its reverb/delay sends stop too — the dry drum leaves the box, the
  mixer handles anything further.
- `Output::Pair(a, b)` where `(a,b) ∈ {(2,3),(4,5),(6,7)}` — the track's
  panned stereo sample feeds the two channels of a pair, FX sends forced to
  0, removed from master. Same removal semantics as `Channel`; the only
  difference is pan is preserved as a stereo position rather than summed.

Two tracks may target the same `Channel`/`Pair` (sum mixes them on the way
out — handy for layering), and `Output::Master` is always an option so unused
individual channels simply stay silent. No new DSP — only a per-track
destination switch and a wider output bus.

**Engine side — cheap, do regardless of hardware.**
- [ ] Per-track `output: Output` field on `Track` (default `Master`), set via
      `Track::set_output`. `Output` is a small `Copy` enum — `Master`,
      `Channel(u8)`, `Pair(u8, u8)` — fits the same "stored user state, one
      recompute" pattern as the macros.
- [ ] Output bus on `DrumEngine`: in `MasterOnly` mode, `[f32; 2*BLOCK]` (the
      current stereo master, unchanged). In `Multi` mode, `[f32; 8*BLOCK]` —
      ch0/1 master wet, ch2..7 individually-routed tracks. `process()` writes
      each track into its destination buffer instead of (or alongside) the
      master, per `Output`. The master-path code is the existing sum; the
      individual path is one mono store + a skip of the master sum.
- [ ] Output-stage config: `OutputMode { MasterOnly, Multi }` on `DrumEngine`.
      Host renderer keeps `MasterOnly` so WAV renders are bit-identical to
      today. Treat `Multi` as a runtime feature flag, the same way send FX was
      bench-gated in Phase 5. Host can opt into `Multi` for per-channel render
      dumps (eight mono WAVs) — useful for the Phase 10 golden-snapshot work.
- [ ] Send-FX interaction: when a track's `output != Master`, force its
      `eff_send_delay`/`eff_send_reverb` to 0 in `control()` for the block.
      Store the user-facing send levels separately on the track so flipping
      back to `Master` restores them without a re-CC. This is the one semantic
      wrinkle the routing introduces; call it out in tests.
- [ ] Tap point is **post-strip, post-fader, pre-send** (the same point sends
      tap off) — filter, drive, amp env, and pan are all audible on the
      individual out. For `Channel` (mono) pan is summed; for `Pair`/`Master`
      pan is preserved. No new DSP, one existing tap reused.

**Firmware side — the real work, hardware-gated.**
- [ ] SAI1 TX: 4 data lines (OUT1A..OUT1D) on `audio`-class pins, one per
      PCM5102A board. All 4 boards share BCLK + LRCLK (slaves, internal-PLL /
      SCK-grounded mode); each gets its own TX + GND + 3V3. MCLK not needed.
      One peripheral, one clock domain — simpler than the 5-board option this
      replaced. This is the configuration 8-A's teensy4-bsp 0.6 bump lands on
      for a single line; extend to 4 lines in the same driver surface.
- [ ] Audio callback DMA: 8-channel interleave buffer (or 4 per-line stereo
      buffers, whichever the BSP's DMA TCD layout prefers). Still ITCM, still
      DTCM hot buffers — 8-A's deferral lifts for the whole output stage.
- [ ] CC for output routing: pick one CC slot per track (e.g. a high `SLOT`
      in the `SLOT_OUTPUT` bank, parallel to `SLOT_MACHINE`'s quantise-on-recc
      pattern) so the Deluge can reassign outs in a kit. One CC value encodes
      `Master` / `Channel(n)` / `Pair(a,b)` — a small lookup in `apply_cc`.
      Settled in firmware, not engine — the engine takes `Output` directly.

**Bench.**
- [ ] Add `8 + MultiOut` scenario to `firmware/src/bin/bench.rs`: same `8 + FX`
      load as Phase 5 plus per-track output-routing writes (6 tracks to
      individual channels, 2 to master — the realistic mixer case). The extra
      work is one mono store per routed track per sample; expected to be in
      the noise of `8 + FX` given how cheap the send-accumulate path already
      is. DMA cost is invisible to DWT — flag separately, scope if it matters.
- [ ] Confirm the worst-case `Multi` routing (all 8 tracks to 6 channels, 2
      sharing) costs no more than the all-master case — it should, since
      master is the bus that runs the FX.

**Why this scales safely.** The plan's whole budget story is per-sample DSP
cost; routing adds stores, not multiplies, and the master path is exactly what
the engine builds today — the 6 individual channels only carry tracks the user
explicitly pulled *off* master, so total work is bounded by the track count,
not the output count. 8 channels fitting SAI1's 4 TX lines is what makes the
4-board count load-bearing: it's the largest individual-out stage that needs
no second peripheral, and it matches the track count well enough that every
drum can still get its own channel for a 6-channel mixer with the master pair
carrying the wet sum of any leftovers.

### Phase 9 — Sample-accurate event timing

Adopt the guidance's `TimedEvent { offset, event }` contract so a hit lands on
the exact sample the sequencer meant, and the deferred gate inputs drop into
the same plumbing unchanged. Block is already 32 (the guidance's recommended
32–64), so the offset resolution is already fine. The engine side is done;
the firmware sample-counter wiring is Phase 8-M.

- [x] `TimedEvent` queue on the engine; drain at the top of `process()`, start
      each voice at its sample offset inside the block. `TimedQueue` (capacity
      [`MAX_TIMED_EVENTS`], offset clamped to `BLOCK - 1`, offset-sorted
      drain) lives on `DrumEngine`. Tests: no output before the trigger offset
      (a hit at offset 10 is bit-identical to a hit at offset 0 shifted by
      10), a scheduled panic cuts at its offset, queue-full drops are
      reported, events in one block land in offset order.
- [x] MIDI note-on → `TimedEvent` with the arrival sample as the offset.
      `midi::schedule_midi` queues NoteOns and applies CC/Panic immediately.
- [x] Firmware: compute the arrival sample from the audio sample counter and
      feed `schedule_midi` — `main.rs` keeps a `sample_counter` and passes
      `arrival_offset(sample_counter)` to `schedule_midi`. Before SAI this is
      always 0 (the loop only drains between blocks, so a block boundary is
      the earliest a note can play); once the audio interrupt owns the
      counter, the same call becomes arrival-sample timing unchanged.
- [x] Retune-before-trigger discipline stays intact — the timed path routes
      through `trigger_channel`, which already retunes.

### Phase 10 — Tuning & test hardening (design-guidance items)

Measurement + sound work, not architecture. The engine already has the hooks;
these close the loop on the macro philosophy.

- [ ] `render sweep` logs RMS (optionally LUFS) per step; a trim pass derives
      the inverse gain so macros are loudness-compensated along their travel.
- [ ] Macro monotonicity tests: per machine + macro, assert RMS and decay time
      move monotonically across the travel — the automated dead-zone detector.
- [ ] Golden WAV snapshots: default-macro renders committed per machine; CI
      diffs after any refactor.
- [ ] PUNCH-style multi-target macro on BD Classic (pitch-env depth + decay +
      click) once the trim tooling exists.
- [ ] AHD per-segment curve blend on the track amp env (the guidance's
      linear ↔ one-pole lerp, ~2 lines in `ahd.rs`).
- [ ] Preset save/load to SD (the open M4 item) — pairs with Phase 6's SDIO
      kit loader.

### Phase 11 — Analog-modelled (VA) machine family

A second BD option (`BdVa`) and the family it opens up, based on a bridged-T
biquad — the topology the TR-808 actually used for kick, toms, and the low
half of the snare. Source: an external `va-bd-sample.rs` sketch (an
`AnalogKickEngine` driving a `BridgedTBiquad`), evaluated against the
engine's conventions and refit to the macro philosophy.

**Why as a phase, not a single machine.** The bridged-T is the resonant
network behind several 808 voices. Porting the biquad once and exposing it
as a `dsp` primitive unblocks a small family (BD VA, VA tom, VA snare low
half) for roughly the cost of porting BD VA alone. Catalogue goes 12 → 13
now, with the rest available as later one-machine additions under the same
phase.

**Why bench-gated.** The sample drives the biquad retune every sample — a
Taylor sin/cos + a divide inside the per-sample path. The whole project's
budget history is "the per-sample transcendentals cost more than you think"
(see Phase 0's `sinf` finding, Phase 5's 2× miss vs estimate). Before
committing to per-sample retune we measure it; if it blows the budget we
split it: precompute the static denominator in `set_macros`, only update
the moving frequency term per sample. That split is the kind of decision
that earns its own bench row.

#### DSP primitive

- [ ] `dsp/bt.rs` (`BridgedT`): direct-form-II biquad with BP-shaped
      coefficients (b1 = 0). Methods: `new`, `set_coeffs(sr, hz, q)`
      (setup-rate), `process(x)` (per-sample, 5 mul / 4 add as in the
      sketch). Reads `SAMPLE_RATE` from the crate so the caller doesn't pass
      it every call.
- [ ] `set_coeffs` uses `fast::sin_turns` + the existing `1.0 / (1.0 + alpha)`
      divide; rejects the inline Taylor sin/cos and the bespoke
      `fast_inv_sqrt` from the sketch (unused and not needed — there is no
      inverse sqrt in the coefficient path).
- [ ] DF-II states flushed via `DENORMAL_FLOOR` like the one-poles (`env.rs`
      and `filter.rs` set the pattern). Long resonance tails without FZ
      assistance would otherwise denormalise.
- [ ] `pub use` added in `dsp/mod.rs` next to `OnePoleLp`/`OnePoleHp`.
- [ ] Tests: impulse response is bandpass-shaped around `hz`; peak moves
      when `hz` moves; resonance narrows with higher `q`; NaN at extreme
      `q=0`, `hz=sr/2`; reaches zero on silence.

#### Machine 1 — BD VA

- [ ] `machines/bd_va.rs` (`BdVa`): same shape as `BdClassic`/`BdFm` —
      `new`, `set_macros`, `trigger(velocity)`, `reset`, `is_active`, `tick`,
      `retune(semis)`. Machine struct owns one `BridgedT`, two `DecayEnv`s
      (amp + pitch), cached coefficients.
- [ ] Drop the sketch's manual `*= 0.992` / `*= 0.9992` multipliers in
      favour of `DecayEnv` + `decay_coeff`. Gets flush-to-zero, `is_active`,
      and the existing `DecayEnv`-based tests (silent-until-struck,
      decays-to-silence, velocity-scales) for free.
- [ ] Encode the sketch's baked constants as macro mappings, following the
      canonical slot layout in `machines/mod.rs:88`:

      | idx | CC  | name    | range         | maps to |
      |-----|-----|---------|---------------|---------|
      | 0   | 20  | TUNE    | 30..120 Hz    | biquad base `target_hz` |
      | 1   | 21  | SWEEP   | 0..~3×       | pitch-env depth in Hz (sketch: +120) |
      | 2   | 22  | SWP_T   | 5..105 ms     | pitch-env decay coefficient |
      | 9   | 29  | Q       | 0.5..10       | biquad `decay_q` (sketch: 4.5/6.0) |
      | 16  | 36  | LEVEL   | 0..1          | output level |
      | 17  | 37  | PAN     | 0..1          | (per-track strip, ignored here) |
      | 18  | 38  | DEC     | 50..1500 ms   | amp-env decay coefficient |
      | 22  | 42  | SEND.DLY| 0..1          | |
      | 23  | 43  | SEND.RVB| 0..1          | |

      All other slots RESV. No FILTER cutoff slot — the resonator *is* the
      filter, so only its resonance goes in the FILTER bank.

      **Q lives at `SLOT_LPF` (9), not `SLOT_SHAPE` (20).** Slot 9 is
      documented in `machines/mod.rs:106` as the "resonance / lowpass
      cutoff family" — that docstring has been waiting for a resonance user
      since Phase 1 (`HatClassic` and `Cp` use it for LPF cutoff). Q is a
      family-wide parameter: every VA machine (`BdVa`, VA Tom, VA Snare low
      half, VA Woodblock) is built on `BridgedT` and has the same
      resonance parameter, so they all share slot 9 the way
      `BdClassic`/`BdFm`/`Tom` share `SLOT_SWEEP`. Putting Q at 20 would
      burn a slot constant per machine and violate the "same slot, same
      meaning" rule that lets the firmware CC map stay stable.
- [ ] `retune` scales `base_freq` by `fast::semitone_ratio(semis)`, same as
      `bd_classic.rs:99`. Keeps Phase 7's transpose-then-recompute
      discipline intact (re-applied in `set_macros` via stored
      `freq_scale`).
- [ ] Per-sample retune decision (bench-gated):
      - **Option A (faithful to sketch):** call `set_coeffs` every sample
        with the moving `current_hz`. Costs one divide + one `sin_turns`
        + one `cos_turns`-via-sin per sample. The pitch only moves a few Hz
        per sample after the transient, so most of this is wasted.
      - **Option B (split):** compute the alpha/`a0_inv` denominator in
        `set_macros` against a reference `hz`; per-sample, only the
        `cos_w0` / `a1` term moves with `current_hz`. Cheaper, audibly
        identical unless the sweep is extreme.
        Bench both; ship the cheaper one if A/B is inaudible on a render.
- [ ] Machine tests: silent-until-struck, velocity-scales, decays-to-
      silence, pitch-falls-over-time (port from `bd_classic.rs:177`),
      retune+recompute (port from `bd_classic.rs:206`).

#### Plumbing

- [ ] `machines/mod.rs`: `pub mod bd_va;` + `pub use bd_va::BdVa;`.
- [ ] `MachineId`: add `BdVa` variant; `COUNT` 12 → 13; one arm each in
      `ALL`, `name()` (`"bd-va"`), `label()` (`"BD VA"`).
- [ ] `MachineSlot`: add `BdVa(BdVa)` variant + match arms on `new`, `id`,
      `set_macros`, `trigger`, `retune`, `reset`, `is_active`, `tick` (8
      match sites, all mechanical — see `machines/mod.rs:874-1023`).
- [ ] `MACHINE_INFO`: one new `MachineInfo` row at index 12 with the macro
      defaults tabled above. Slot order is part of the binary ABI — the
      firmware CC map depends on it; BD VA is appended, not inserted.
- [ ] Renderer `MachineArg` updated; `sweep bd-va <macro>` works for free
      once the enum is in.
- [ ] MIDI note map: BD VA shares the kick row (notes 35/36) — selection
      via `SLOT_MACHINE` already quantises across `MachineId::ALL`, so no
      firmware change.

#### Bench

- [ ] Add `8 + BD VA` scenario to `firmware/src/bin/bench.rs`: eight tracks,
      three of them BD VA worst-case (high Q, long decay, full sweep) to
      exercise the per-sample retune path. Goal: stay under ~70% of budget,
      the same ceiling Phase 5 hit. If over, switch to Option B and re-measure.

#### Follow-on machines (later, same phase scaffolding)

Once `BridgedT` is a `dsp` primitive, these become one-machine additions
under the same plumbing pattern — none requires new DSP, only macro
mappings + a new module:

- [ ] **VA Tom** — identical topology to BD VA, different tuning range and
      a longer pitch sweep. Reuses `BridgedT`, two `DecayEnv`s. Distinct
      from the existing sine-sweep `Tom` (cleaner attack, true resonator
      ring).
- [ ] **VA Snare low half** — pair of `BridgedT`s tuned a 4th apart for the
      body, with `Noise` through `OnePoleHp` layered on top. Combines the
      analog resonator family with the noise path that `sd_natural.rs`
      already uses.
- [ ] **VA Woodblock / Conga** — single `BridgedT`, short decay, high Q.
      Catalogue's tonal-percussion gap; the existing `SyTone` is FM, not
      modal.

None of these needs to ship in Phase 11. The point of the phase is that
porting the primitive once makes them cheap later.

#### Out of scope here

- `fast_inv_sqrt` from the sketch — unused; not added to `dsp/fast.rs`
  unless something asks for it.
- Sample machine (Phase 6) — still its own phase. The BD VA family is
  fully synthesised, no samples.

### Phase 12 — FX machines: sustained gesture voices

Two non-percussive machines that grow the catalogue past one-shots: a
**dub siren** and a **sweep FX**. Both are the Syntakt's "FX track"
idea — a gesture that lasts seconds rather than a hit that rings out.
Catalogue goes 13 → 15, appended (binary-ABI stable, no slot reordering).

**Why a phase, not two ad-hoc machines.** Both raise the same one
architectural question the catalogue has not hit yet: every existing
machine is a `DecayEnv` one-shot, gated by `Track::is_active` for the
budget early-out at `lib.rs:1381`/`lib.rs:1397`. A sustained sweep
lasts seconds, so it would defeat that early-out if modelled as a
gate held open by an external NoteOff (which the engine does not have
— `MidiEvent`/`EngineEvent` carry NoteOn only). Both machines solve
this the same way: a self-timed internal `AhdEnv` owns the gesture
length, the machine reports `is_active` until the AHD idles, and the
per-track early-out is preserved for free.

`AhdEnv` already lives in `dsp/ahd.rs` (Phase 1, used by the per-track
strip amp env). Phase 12 is the first time a *machine* uses it — no
new DSP primitive, just a new consumer.

**Why bench-gated, but lightly.** Sustained machines stay active for
seconds, so they sit inside the "8 sounding worst-case" bench row
rather than the idle row — but per-sample cost is one `SineOsc` + one
LFO for the siren, one `Svf` + one LFO for the sweep, which is cheaper
than `CyMetallic`'s ring-mod path. The real budget question is
whether running a sweep alongside 7 short drums creates a new
sustained-load worst case the bench has not measured. Phase 12 adds
one bench row to find out; the same disposition as Phase 11's
`8 + BD VA`.

#### Machine 14 — Dub Siren

`machines/dub_siren.rs` (`DubSiren`): a sine oscillator whose
frequency is modulated by aninternal LFO, gated by a long self-timed
`AhdEnv`. The classic dub-reggae siren: a slow warble that dives or
rises, played back through long delay throws.

Topology:

```
   internal LFO (tri/saw/sine) ──► pitch offset (octaves)
   AhdEnv (gesture)            ──► amp
   SineOsc (carrier)           ──► out
```

No new DSP — `SineOsc`, a small inline LFO (phase accumulator +
`fast::sin_turns`), and `AhdEnv`. Retune scales the carrier base
frequency only (the LFO depth is in octaves, so it travels with the
note). SHAPE macro quantises the internal LFO waveform; dropping it
and shipping a fixed sine LFO (the classic dub-siren shape) is a
valid fallback if the quantise adds noise.

Macro layout (canonical 4-bank, appended at index 13):

| idx | CC  | name    | range        | maps to |
|-----|-----|---------|--------------|---------|
| 0   | 20  | TUNE    | 100..1000 Hz | osc base frequency |
| 1   | 21  | DEPTH   | 0..3 oct     | LFO pitch-sweep width |
| 2   | 22  | RATE    | 0.1..8 Hz     | siren LFO speed |
| 5   | 25  | MACH    | 0..1          | machine selector |
| 16  | 36  | LEVEL   | 0..1          | output level |
| 17  | 37  | PAN     | 0..1          | (track-routed) |
| 18  | 38  | DEC     | 0.5..6 s      | AHD gesture length (5% atk / 85% hold / 10% dec) |
| 20  | 40  | SHAPE   | 0..1          | LFO waveform: tri ↔ saw ↔ sine (quantised) |
| 22  | 42  | SEND.DLY| 0..1          | |
| 23  | 43  | SEND.RVB| 0..1          | |
| 26  | 46  | OUT     | 0..1          | track routing |

All other slots RESV.

#### Machine 15 — Sweep FX

`machines/sweep_fx.rs` (`SweepFx`): white noise through an SVF whose
cutoff is swept by an internal LFO, gated by a long self-timed
`AhdEnv`. The "filter sweep" riser — the most-used gesture in
electronic FX.

Topology:

```
   internal LFO (tri)  ──► cutoff (octaves, exp2_approx)
   AhdEnv (gesture)    ──► amp
   Noise ──► Svf (LP/BP/HP) ──► out
```

Reuses `dsp::Svf` (already in the per-track strip path) *inside* a
machine for the first time. The first `dsp::Svf` user outside `Strip`.
Resonance lives at `SLOT_LPF` (9) — the family-wide resonance slot,
the same disposition as BD VA's Q. Noise-only → `retune` no-ops (Hat
Classic precedent).

Macro layout (appended at index 14):

| idx | CC  | name    | range         | maps to |
|-----|-----|---------|---------------|---------|
| 0   | 20  | RATE    | 0.1..5 Hz      | sweep LFO speed |
| 1   | 21  | DEPTH   | 0..4 oct       | cutoff sweep width |
| 2   | 22  | START   | 80..8000 Hz    | cutoff centre |
| 5   | 25  | MACH    | 0..1           | machine selector |
| 9   | 29  | RESO    | 0.5..12 Q      | SVF resonance (FILTER bank) |
| 16  | 36  | LEVEL   | 0..1           | output level |
| 17  | 37  | PAN     | 0..1           | (track-routed) |
| 18  | 38  | DEC     | 0.5..6 s       | AHD gesture length |
| 20  | 40  | MODE    | 0..1           | LP/BP/HP, quantised (bandpass default) |
| 22  | 42  | SEND.DLY| 0..1           | |
| 23  | 43  | SEND.RVB| 0..1           | |
| 26  | 46  | OUT     | 0..1           | track routing |

All other slots RESV.

#### Plumbing (per the existing pattern — one machine = one module + match arms)

- [ ] `machines/dub_siren.rs` + `machines/sweep_fx.rs`, same file
      shape as `bd_va.rs`: `new`, `set_macros`, `trigger(velocity)`,
      `reset`, `is_active`, `tick`, `retune(semis)`.
- [ ] `machines/mod.rs`: `pub mod dub_siren;`/`pub mod sweep_fx;` +
      `pub use` of `DubSiren`/`SweepFx`.
- [ ] `MachineId`: add `DubSiren` (index 13) and `SweepFx` (14);
      `COUNT` 13 → 15; two new arms each in `ALL`, `name()`
      (`"dub-siren"`, `"sweep-fx"`), `label()` (`"Dub Siren"`,
      `"Sweep FX"`).
- [ ] `MachineSlot`: add `DubSiren(DubSiren)` and `SweepFx(SweepFx)`
      variants + match arms on `new`, `id`, `set_macros`, `trigger`,
      `retune`, `reset`, `is_active`, `tick` (16 match sites,
      mechanical).
- [ ] `MACHINE_INFO`: two new `MachineInfo` rows appended at indices
      13 and 14, slot order per the tables above. Appended not
      inserted — the firmware CC map depends on absolute index.
- [ ] Renderer `MachineArg` updated; `sweep dub-siren <macro>` and
      `sweep sweep-fx <macro>` work for free once the enum lands.
- [ ] Machine tests, ported from `bd_va.rs:185` template:
      silent-until-struck, velocity-scales-output, decays-to-silence
      (with a longer window — 7 s — to cover the longest DEC),
      retune-survives-recompute (dub-siren only; sweep-fx is
      noise-only and `retune` no-ops), extreme-macros-do-not-produce-
      nans.
- [ ] Sustained-gesture test (new shape, applies to both): a gesture
      at default DEC is still `is_active()` past 1 s, and reaches
      `!is_active()` before 7 s. Pins the AHD-as-gesture contract
      that the budget argument depends on.

#### Bench

- [x] Add `8 + sweep fx sustained` scenario to
      `firmware/src/bin/bench.rs`: the existing 8-track sounding
      pattern, with one track loaded as       `SweepFx` sustaining a 2 s
      gesture (sweep depth at max, resonance high — the worst-case
      SVF path). Goal: stay under ~70% of budget, the same ceiling
      Phase 5/11 hit. The siren path is cheaper, so it is not benched
      separately. First run: 83.4% — the per-sample `Svf::recalc`
      (`libm::sinf`) blew the budget, so the Option B split (line 866)
      was triggered: `Svf::set_cutoff` refreshes only the moving `c1`
      via `fast::sin_turns`, the SweepFx analog of `BridgedT::set_hz`.
      Re-measure after the split: 72.1% — within ~2 points of the
      baseline `8+FX+CC` row, still over the nominal 70% ceiling
      (the same 2-point overshoot every FX row carries); parked for a
      later optimisation pass.
- [x] Note the sustained-load caveat in the bench output: a
      sustained machine defeats the per-track idle early-out for its
      whole gesture, so a kit with a sweep-FX track idles at "7 idle
      + 1 sounding" rather than "8 idle". This is the new fact
      Phase 12 introduces to the budget model.

#### Out of scope here

- Real-time NoteOff / gate-release (the V2 sustained model). The
  engine has no NoteOff path; Phase 12 commits to the self-timed
  AHD model and pins it with the sustained-gesture test. External
  gating is a separate, larger change that would touch
  `MidiEvent`/`EngineEvent` and the `is_active` budget contract.
- A `dub-siren` SHAPE macro beyond the tri/saw/sine quantise. A
  dedicated "warble" curve (e.g. shapeable cubic) is sound design
  work, not architecture — keep it for a later macro-polish phase.
- Routing the sweeps' own output back into the engine's send-FX
  loop. They are tracks like any other; their `SEND.DLY`/`SEND.RVB`
  macros work the same as every other machine's, which is the
  load-bearing property. No new send routing.

### Phase 13 — Device framework + MI drum device

*Goal:* restructure the repo so it can host multiple devices on a shared
framework, then build a second device whose machines wrap Mutable Instruments
open-source C++ DSP.

#### Target tree

```text
drumsynth/
├── Cargo.toml            workspace: core, mi-dsp, devices/*, render
├── core/                 `device-core` — Rust-only, no_std, no alloc
│   └── src/  dsp/, macros.rs, slot.rs, track.rs, midi.rs, grid.rs, engine.rs
├── mi-dsp/               vendored MI C++ + Rust FFI wrappers
│   ├── vendor/           stmlib, plaits/dsp, peaks/dsp (+ LICENSEs)
│   ├── include/          extern "C" shims over C++ classes
│   ├── src/              component wrappers + engine wrappers
│   └── build.rs          cc-based build for host and thumbv7em-none-eabihf
├── devices/
│   ├── drum/             existing engine crate, now on core
│   └── mi-drum/          NEW device #2
├── render/               one binary, --device flag, device registry
└── firmware/             one crate (still excluded), shared src/ + bin per device
    └── bin/  drum.rs, mi-drum.rs, bench.rs
```

#### Device contract

A device is **one engine crate** containing: a machine catalog
(`MachineId`/`MachineSlot`/`MACHINE_INFO`), an engine struct over
`core::Track<YourSlot>`, a default kit, and a `DeviceEngine` impl.
Everything else comes free: MIDI router + CC map, grid UX, firmware bin,
render/sweep/device harness, bench scaffolding, and SendFx.

#### Trait surfaces

```rust
// Static dispatch: each device's voice slot enum implements this;
// Track<S, N> monomorphizes. No trait objects, no allocation.
pub trait Slot<const N: usize>: Sized {
    type Id: SlotId<N>;
    fn new(id: Self::Id, macros: &[f32; N]) -> Self;
    fn id(&self) -> Self::Id;
    fn set_macros(&mut self, macros: &[f32; N]);
    fn trigger(&mut self, velocity: f32);
    fn retune(&mut self, semis: f32);
    fn reset(&mut self);
    fn is_active(&self) -> bool;
    fn tick(&mut self) -> f32;
}

pub trait SlotId<const N: usize>: Copy + Eq + Debug {
    fn index(self) -> usize;
    fn from_index(i: usize) -> Option<Self>;
    fn count() -> usize;
    fn default_macros(self) -> [f32; N];
}

// Narrow metadata surface for the grid UI: per-machine macro names/abbrevs
// and a human-readable label.
pub trait DeviceModel<const N: usize>: SlotId<N> {
    fn macro_info(self) -> [MacroInfo; N];
    fn label(self) -> &'static str;
}

pub trait DeviceEngine<const N: usize> {
    type Slot: Slot<N>;
    fn tracks(&self) -> &[Track<Self::Slot, N>];
    fn tracks_mut(&mut self) -> &mut [Track<Self::Slot, N>];
    fn trigger(&mut self, track: usize, velocity: f32);
    fn trigger_note(&mut self, note: u8, velocity: f32) -> Option<usize>;
    fn trigger_channel(&mut self, channel: u8, note: u8, velocity: f32) -> Option<usize>;
    fn set_note(&mut self, note: u8, track: Option<u8>);
    fn panic(&mut self);
    fn schedule_timed(&mut self, offset: usize, ev: EngineEvent) -> bool;
    fn load_sound(&mut self, track: usize, sound: &Sound<Self::Slot, N>);
    fn trigger_with_sound(&mut self, track: usize, velocity: f32, sound: &Sound<Self::Slot, N>);
    fn is_active(&self) -> bool;
    fn process(&mut self, out_l: &mut [f32], out_r: &mut [f32]);
    fn process_dry_wet(
        &mut self,
        master_l: &mut [f32; BLOCK],
        master_r: &mut [f32; BLOCK],
        aux: &mut [[f32; BLOCK]; 6],
        wet_l: &mut [f32; BLOCK],
        wet_r: &mut [f32; BLOCK],
    );
    fn master_gain(&self) -> f32;
    fn set_master_gain(&mut self, value: f32);
    fn fx_drive(&self) -> f32;
    fn set_fx_drive(&mut self, value: f32);
    fn load_kit(&mut self, kit: &[<Self::Slot as Slot<N>>::Id]);
}
```

`Track`, `Strip`, `ModState`, `TimedQueue`, `Sound<Id>`, `OutPair`, the
macro/CC system, `SendFx`, and `dsp/` move to core.

#### Phase 13.0 — Baseline capture

Record pre-refactor `cargo test` state and a release `out.wav` byte hash.
Existing bench numbers in this plan are the perf baseline.

#### Phase 13.1 — Extract `core` crate (mechanical)

Create workspace member `core/` (`device-core` crate). Move verbatim:
`dsp/`, macro system (slot consts, `Macro`, `MacroInfo`, banks), MIDI
*parser*, consts, `TimedQueue`/`EngineEvent`/`TimedEvent`, `OutPair`.

The `drum-engine` crate depends on core and **re-exports** so
firmware/render/tests compile unchanged this phase. CI adds
`-p device-core` to cross-compile and clippy jobs.

*Gate:* all tests green, `out.wav` bit-identical.

#### Phase 13.2 — Genericize track infrastructure

Introduce `MachineSlotSurface`; make `Track`/`Strip`/`Sound` generic.
Drum's `MachineSlot` implements it; `DrumEngine` becomes a thin struct
over `[Track<MachineSlot>; 8]`. Track/engine tests move to core;
machine tests stay with the drum machine modules.

*Gate:* tests green, `out.wav` bit-identical, **bench cycle counts
unchanged** — proves monomorphization preserved the hot path.

#### Phase 13.3 — `DeviceEngine` trait; genericize consumers

MIDI router, grid (via a narrow `DeviceModel` surface), firmware
`audio.rs`/`usb.rs`/main-loop skeleton, render `device.rs` harness and
`--device` flag with a registry. Move `engine/` → `devices/drum/`.
Firmware gets `bin/drum.rs` (thin) + shared `runner`.

*Gate:* hardware behaves identically to pre-refactor.

#### Phase 13.4 — `mi-dsp` crate ✅

Vendor stmlib + `plaits/dsp` + `peaks/dsp` with LICENSEs. Inventory
component families against the core `dsp/` taxonomy (oscillator/, LFO,
envelope, waveshaper, `units.h` param scaling, physical models; Peaks
drum synth; Plaits Engine interface).

`build.rs` uses the `cc` crate for host and
`thumbv7em-none-eabihf` builds via `arm-none-eabi-g++` with
`-fno-exceptions -fno-rtti -ffp-contract=off` (no fast-math). Host builds
add `-DTEST` so the vendored ARM inline asm is disabled and tests/benches
run on x86_64 / arm64.

**FFI rules:**
- Boundary is **block-rate** only (`mi_plaits_voice_render(handle, ...)`);
  no per-sample extern calls.
- No-alloc object ownership: C++ objects are placement-new'd into aligned
  `[u8; PLAITS_VOICE_STORAGE_SIZE]` storage held by the Rust wrapper.
- `unsafe` is confined to `mi-dsp`; core and device crates keep
  `#![deny(unsafe_code)]`.

*Gate:* wrap **one** Plaits engine and bench `8×` worst case on hardware
before any catalog breadth decision.

**Completed:** `mi-dsp` crate added to workspace; `PlaitsVoice` wrapper
exposes block-rate `render`/`render_f32`. Host bench (`cargo bench -p mi-dsp`)
renders 8 voices × 24 samples for 1000 iterations at ~20.8 MS/s (~28.8
estimated CM7 cycles/sample @ 600 MHz). Unit tests verify init, idle
silence, and triggered output. Cross-compile to `thumbv7em-none-eabihf`
requires `arm-none-eabi-g++` or `clang++` (not exercised on this host).

#### Phase 13.5 — Device #2: `mi-drum`

Catalog from `mi-dsp` wrappers — each Plaits model as its own machine
(~16) plus Peaks BD/SD/HH (~3), breadth bench-gated. Macros map to MI
params via `units.h` scaling where it fits. `MiDrumEngine` over core
`Track`; grid/MIDI/firmware/render arrive via Phase 13.3 traits.

`bin/mi-drum.rs`, bench scenario, `--device mi-drum` in render.
Determinism: within-platform bit-identity tests; cross-platform
bit-identity documented as best-effort for C++ float paths.

*Gate:* device playable over MIDI+grid on Teensy, bench under ~70%,
tests green.

#### Phase 13.6 — CI + docs

CI tests all crates, cross-compiles core + drum + mi-drum + mi-dsp
(adds `g++-arm-none-eabi` install step), and enables the firmware bench
build job now that the imxrt-hal 0.6 blocker is resolved. README becomes
a device-family doc with the "add a device" checklist.

#### Risks and mitigations

| Risk | Mitigation |
|---|---|
| C++ cross-toolchain (macOS + CI) | Document install; CI apt step; fallback clang `--target=` |
| Per-sample FFI cost kills budget | Block-rate FFI boundary is a stated rule; Phase 13.4 bench gate |
| Genericization regresses hot path | Phase 13.2 bench-comparison gate |
| RAM: 8 Plaits engines + SendFx | Fits 512 KB on paper; assert sizes in bench |
| Host/target bit-identity in C++ paths | `-ffp-contract=off`, within-platform tests, documented caveat |
| Slot-index / CC-map ABI drift | Structural refactor only; WAV byte-compare guards it |
| Polyphonic future | `Machine` stays per-voice; poly device adds voice allocation above machines |

#### Untouched by design

Slot indices and the CC map (binary ABI), the firmware workspace
exclusion, the bench-gating culture, `deny(unsafe_code)` in core +
device crates, and the strict determinism contract for the drum device.

## Design guidance received (2026-08-10)

A design-guidance review (`drum-machine-design.md`, untracked) landed after
Phase 7: outside advice on macro philosophy, envelopes, event scheduling,
Deluge integration, performance, and testing. This section records what the
advice says, what the engine already does, and what we are taking from it.
The verdicts feed Phases 8–10 and the settled-decisions table below.

| Advice | State of the work | Verdict |
|---|---|---|
| Macros are tuned paths, not renamed params — no dead zones, monotonic, loudness-compensated | Normalized 0..1 macros with per-machine `set_macros`; the "tuned path" idea is the engine's core | **Adopt as a rule**, but it is currently unmeasured: no loudness compensation, nothing asserts monotonicity |
| Macro mappings are **data, not code** (breakpoint `Curve` + `MacroTarget` + `loudness_trim`, host hot-reload) | Mappings are hand-written `set_macros` in each machine module | **Defer the data model.** Code mappings are compile-checked and the generic sweep already works off them; the win the advice targets (table-edit tuning) is mostly delivered by the host tool. Adopt the *tuning workflow* instead |
| A macro should drive several params (`PUNCH` = depth + decay + click; `DIRT` gain-compensated) | Macros map mostly 1:1 to internal params | **Take a first one** — PUNCH-style macro on BD Classic (Phase 10). Gain-compensated DIRT needs the RMS tooling first |
| Host tuning workflow: sweep + RMS/LUFS log, dead-zone detection, automated loudness trim | Generic `sweep <machine> <macro>` exists; no measurement, no logging | **Add RMS logging + monotonicity checks** (Phase 10). The renderer is already the right place |
| AHD with per-segment **curve control** (linear ↔ one-pole blend) | `AhdEnv` has linear attack + exponential decay, no blend knob | **Take the guidance's blend verbatim** — ~2 lines, the missing knob (Phase 10) |
| Run envelopes per-sample | Done — `AhdEnv::tick` and all machine envelopes run at audio rate | Already satisfied |
| Pitch envelope independent of amp envelope | Per-machine sweep envelopes are separate from the track AHD | Already satisfied |
| Sample-accurate triggers **inside** the block (`TimedEvent.offset`) | Triggers fire at block start. Block is already 32 (the guidance's recommended 32–64), so the floor is 667 µs — better than the 2.67 ms the advice warns about, but not sample-accurate | **Built** — `TimedQueue` drains in `process()` and fires at sample offsets; `midi::schedule_midi` queues NoteOns with the arrival sample. Firmware sample-counter wiring is Phase 8-M |
| USB MIDI *device*, Deluge as host | Not built. Firmware has `bench` + `TODO(sai)` audio gap | **In scope — the M3 headline and the point of the box** (Phase 8) |
| CC → macro smoothing (one-pole, 5–10 ms) | `apply_cc` steps macros 0..1 in 1/127 increments with a full coefficient recompute per message | **Built** — `Track::set_macro_target` slews at block rate (~7.5 ms) inside `control()`; `apply_cc` routes through it. Kills zipper, caps the `expf` churn (Phase 8-M) |
| Fixed CC map documented and committed | `CC_TRACK_BASE` = 20, table-driven, documented in `midi.rs` and firmware | Already satisfied |
| MIDI channel per voice (or note-number routing) | Both: one channel per track *and* a note map | Already satisfied |
| Gate input circuit + firmware capture | Deferred by the advice itself; hardware-only | Parked — revisit only to close the last millisecond |
| Set the FZ bit in FPSCR | Not set. The engine clamps via `DENORMAL_FLOOR` instead | **Take it** — one line in firmware init, protects the long FX tails too (Phase 8-M) |
| Audio callback in ITCM, hot buffers in DTCM | Not done (ISR in flash/OCRAM, hot buffers in DTCM `.bss`) | Defer to Phase 8-C — measure first (DWT on the ISR body) |
| Hard ceiling ~60% at max polyphony | Measured worst case (Phase 5) is 68.1% — above the advice's number, already flagged | Noted. Budget work concentrates on the delay buffer and `set_macro` (`expf`) |
| PSRAM only if reverb/delay get long | Matches the plan's stance | Already satisfied |
| Golden WAV snapshots against committed references | Not present | **Take it** — cheap, pins the sound across refactors (Phase 10) |
| Property tests (no NaN, reaches zero, within ±1, silent before trigger) | Mostly present (NaN at extremes, output ≤ 1, decay-to-silence) | Top up — explicit "reaches zero" for all machines |
| Macro monotonicity test | Not present | **Take it** — RMS-based, the automated form of "no dead zones" (Phase 10) |
| Host `criterion` benches | Firmware bench (DWT) is the real measure | Defer — host numbers are a regression tripwire we already get from CI + the bench |

Milestones M1/M2 from the guidance map to Phases 1–2. M3 (USB MIDI
enumeration) is the outstanding item and becomes Phase 8. M4 splits: choke +
velocity→macro are done; preset save/load to SD is still open (Phase 10).

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
| teensy4-bsp version | bumped to 0.6 (Phase 8-A) — brings `imxrt_hal::sai`; imxrt-hal 0.6, imxrt-ral 0.6.2, imxrt-usbd 0.4.2, usb-device 0.3.2, teensy4-pins 0.4.0 |
| Macro mapping storage | code (`set_macros` per machine); curve-as-data model deferred, tuning workflow adopted (Phase 10) |
| Sample-accurate trigger offsets | built — `TimedQueue` + `schedule_midi` (Phase 9); firmware sample-counter wiring in Phase 8-M |
| Gate input | deferred (advice-consistent); `TimedEvent` plumbing built first (Phase 9) |
| Transport | USB MIDI device on the Deluge's host port (Phase 8); DIN/gates never a requirement |
| VA machine family | in scope as Phase 11; `BridgedT` lives in `dsp/` as a primitive, BD VA is the first machine, others (Tom, Snare low, Woodblock) follow under the same phase |
| Per-sample biquad retune | bench-gated; Option A (every-sample) is the default, Option B (split static/moving coeff) is the fallback if the bench says over budget. Applied to SweepFx's SVF: the bench's `8+FX+SWFX` row hit 83.4% on Option A, so Option B shipped as `Svf::set_cutoff` (one `sin_turns` lookup per sample, `k` cached from the setup-rate `recalc`); re-measure 72.1% |
| Multi-output | in scope as Phase 8-C, optional and hardware-gated; **8 channels total = 4× PCM5102A** (fits SAI1's 4 TX data lines, one clock domain). Ch 0/1 are the master/wet stereo pair (fixed); ch 2..7 are a per-track-routable pool. Each `Track` has an `Output` destination — `Master` (default, existing behaviour + FX sends), `Channel(n)` (dry mono into one ch), or `Pair(a,b)` (dry stereo into a pair) — an individual-out track is removed from the master sum and has its FX sends forced to 0 (Syntakt/Rytm convention). `OutputMode { MasterOnly, Multi }` on `DrumEngine`; host renders stay `MasterOnly` and bit-identical |
| Individual-out tap point | post-fader, post-strip, **pre-send** — pan summed to mono on `Channel`, preserved on `Pair`/`Master`; no new DSP, the same tap the sends use, reused |
| FX machines (sustained gestures) | in scope as Phase 12; **Dub Siren** (sine + internal pitch LFO, index 13) and **Sweep FX** (noise → SVF, cutoff swept by internal LFO, index 14) appended to the catalogue. Self-timed internal `AhdEnv` owns the gesture length — no NoteOff / external gate path added (engine carries NoteOn only; the per-track `is_active` budget early-out is preserved for the gesture's duration). Bench-gated against a new `8 + sweep fx sustained` row; sustained machines defeat the per-track idle early-out for their whole gesture, which is the new fact in the budget model |
| Machine envelope for sustained voices | `AhdEnv` (Phase 1, used by the per-track strip amp env) becomes a per-machine DSP for the first time in Phase 12 — no new primitive; `DecayEnv` stays the one-shot default, `AhdEnv` is the gesture default |
| Device crate layout | core crate + per-device engine crates |
| Firmware crate layout | one firmware crate, one bin per device; shared audio/usb/runner |
| Render tool layout | one binary, `--device` flag with a device registry |
| Mutable Instruments source | vendored into `mi-dsp/vendor/`, not submodules |
| MI FFI boundary | block-rate only; no per-sample extern calls |
| MI catalog growth | bench-gated; wrap one engine, measure `8×` before breadth |

## Hardware

- Teensy 4.1 (600MHz Cortex-M7, 1MB RAM, microSD on SDIO)
- PCM5102A I2S breakout (for audio output only — not needed for bench). For
  the optional multi-output stage (Phase 8-C): **up to 4 boards = 8 channels**,
  one per SAI1 TX data line, sharing BCLK + LRCLK (one clock domain, no SAI2);
  ch 0/1 master/wet, ch 2..7 per-track-routable individual dry outs.
- The analog/digital track split on Syntakt is about physical circuits;
  meaningless here. Any machine loads on any track.

## Optimisation pass (2026-09-11)

Closed-loop harness: `tools/benchloop.py` builds, flashes, captures and diffs
in one command, with no button press — the bench is built `--features
autoboot`, prints `=== BENCH END ===`, and executes `bkpt #251`, which the
Teensy 4's MKL02 bootloader chip answers by entering HalfKay.
`tools/checkasm.sh` is the instruction census. Run-to-run variance is **4
cycles out of ~350,000**, so anything above ~50 cycles is signal.

Worst case (`8+FX+SWFX`) went **88.2% -> 35.6%** of the 400,000-cycle budget:

| step | 8+FX+SWFX | of budget | delta |
|---|---|---|---|
| baseline (this tree) | 352,767 | 88.2% | — |
| `-C target-cpu=cortex-m7` | 348,934 | 87.2% | −1.1% |
| engine `.uninit` OCRAM -> `.bss` DTCM | 157,035 | 39.3% | **−55.0%** |
| `fast::QUARTER` -> DTCM | 142,237 | 35.6% | −9.4% |

**The engine was memory-bound, not compute-bound** — and the root cause turned
out to be simpler than the placement fix implies. OCRAM is reached over the
AXI bus, and *nothing was enabling the L1 caches*: not `teensy4-bsp`, not
`cortex-m-rt`, though Teensyduino does by default on this same chip. So every
delay-line and reverb-tank access was an uncached bus transaction. Enabling
the caches recovers 98% of what the DTCM move bought:

| configuration | 8+FX+SWFX | of budget |
|---|---|---|
| engine in OCRAM, caches off | 348,934 | 87.2% |
| engine in OCRAM, caches **on** | 143,978 | 36.0% |
| engine in DTCM (cache irrelevant) | 142,237 | 35.6% |

TCM bypasses the caches, so DTCM placement and cache state are independent —
confirmed by a control run (caches on, everything already in DTCM: no change
within the 4-cycle noise floor). That is the whole story of the drift toward the ceiling: the FX
buffers grew past what OCRAM latency could sustain. Note the 72.1% recorded at
Phase 12 is stale — an A/B against `c801324` shows `8 sounding` was already at
84.4% *before* the Phase 13 refactor, which itself costs only ~1.8%.

Measured and rejected:

| change | result |
|---|---|
| `opt-level = 2` | +3.1% to +11.7%. Worse everywhere. |
| `-C llvm-args=-inline-threshold=500` | +1.0% to +1.4%, and ITCM +56% (101 KB -> 158 KB). |

Do not re-try either. Nor look for `vfma` as evidence that `target-cpu` took
effect: LLVM will not fuse `a*b+c` without fast-math, because fusing changes
rounding. The gain there is the scheduling model, invisible in an instruction
census.

Still on the table, not done because the budget goal was met without them:

- Phase 4 micro-optimisations — three real divides in inner loops
  (`hh_basic.rs` `sum / NUM_OSCS`, `track.rs` choke fade, `lfo.rs`), the
  `libm::floorf` inside `exp2_approx`, `.fill(0.0)` for the bus zeroing,
  hoisting `is_active` out of the sample loop, and hoisting the 15-arm
  `MachineSlot::tick` match to once per track per block.
- `bin/mi-drum.rs` stays in OCRAM, and that is now fine. `MiDrumEngine` is
  343,296 bytes against 320 KB of DTCM so it can never fit, but with the
  caches on it pays roughly 1.2% for being there rather than the 51 points it
  used to. The FlexRAM rebalance this file previously called for (ITCM 4 banks
  / DTCM 12, via `imxrt-rt`'s `RuntimeBuilder`) is **not needed**, and neither
  is splitting `SendFx` out of `Engine`.
- DTCM is at 91.6% because the drum engine lives there for a 0.4-point gain.
  If anything else ever needs DTCM, moving the engine back to OCRAM is a
  one-line change costing a measured 1.2%.

### Placement rule

What matters is **accesses per sample x latency**, not size:

| data | placement |
|---|---|
| not touched per sample (`MACHINE_INFO`, string tables, note map) | OCRAM, always fine |
| per sample, data-dependent addressing (lookup tables, voice state) | DTCM — `QUARTER` is 2 KB and cost 9.9% of budget in uncached OCRAM |
| per sample, sequential (delay lines, reverb combs) | cached OCRAM is fine — a walk up an incrementing index is the best case a cache line gets |

With the caches off, every row above collapses to "must be DTCM", which is
what made this look like a placement problem in the first place.
