# Benchmarks

Cycle-bench process for the Teensy 4.1 drum engine. Everything here is about
measuring `engine.process()` on real hardware and deciding whether a change is
affordable.

## Budget

- Core clock: 600 MHz Cortex-M7
- Sample rate: 48 kHz
- Block size: 32 frames
- Budget: **400,000 cycles per block** (12,500 cycles per output frame)

Judge against **peak**, not average. A mean that fits while the peak does not is
a click you will hear.

The informal ceiling is **~70% of budget** for the worst-case scenario. The
bench itself warns above 70%.

## Bench binaries

Two cycle-bench binaries live in `firmware/src/bin/`:

| binary | device | features | how to build |
|---|---|---|---|
| `bench` | drum (Rust machines) | `autoboot` | `cd firmware && cargo build --release --bin bench --features autoboot` |
| `mi-bench` | mi-drum (6 tracks: 4 Peaks + 2 Plaits) | `mi-drum,autoboot` | needs `arm-none-eabi-g++` on PATH |

Both print the same report format and end with `=== BENCH END ===`, then reboot
themselves into HalfKay via `bkpt #251` when built with `autoboot`.

### Scenario naming

- `idle` — loaded kit, every track silent (`is_active` early-out exercised)
- `N sounding` — every track retriggered every block; worst-case voice cost
- `N FX idle` — FX tanks advancing on silence
- `N + FX` — realistic worst case with sends routed
- Extra rows are added per phase/sub-phase to gate new work

## Closed-loop harness

`tools/benchloop.py` builds, flashes, captures, parses, and diffs a run in one
command.

```bash
# first time: press the Teensy button when prompted
tools/benchloop.py --bin bench --label my-change

# diff against a saved baseline
tools/benchloop.py --bin bench --label my-change --baseline baseline

# mi-drum bench
tools/benchloop.py --bin mi-bench --label mi-change --baseline mi-baseline
```

The harness:

1. Builds the bench ELF and converts it to HEX with `rust-objcopy`.
2. Flashes with `teensy_loader_cli -w` (waits for HalfKay; autoboot benches
   reboot themselves, so after the first flash no button press is needed).
3. Opens the CDC port as fast as possible after reboot (macOS drops bytes that
   arrive before the character device is open).
4. Captures until `=== BENCH END ===`.
5. Parses rows, computes region sizes from `rust-size -A`, and writes
   `bench-results/<label>.json`.
6. Reports peak cycles, % budget, and memory pressure; flags anything over 70%.

Run-to-run variance is roughly **4 cycles out of ~350,000**, so anything above
~50 cycles is signal.

### Captures that miss the header

If the host lost the opening rows, `benchloop.py` detects it and re-flashes
automatically (3 retries by default). Do not trust a capture whose header is
missing.

### Panic pattern

If the LED blinks S.O.S. (3 short / 3 long / 3 short), `teensy4-panic` caught a
fault. The last printed scenario is the one before the crash. Press the button
to get back to HalfKay.

## Interpreting results

- Compare **peak** cycles, not average.
- Compare against the relevant baseline (`baseline.json`, `mi-baseline.json`, or
  the 14.1 skeleton gate for mi-drum strip work).
- Watch the **worst-case scenario** first; a change that helps idle but hurts
  the worst case is not a win.
- Memory pressure matters: a cycle win that costs ITCM or DTCM you do not have
  is not a win.

## Gate culture

Bench-gate every significant change:

- New machine or stage: add a worst-case scenario row, measure it, and record
  the delta.
- Refactor: assert the worst-case scenario is unchanged within the ~50-cycle
  noise floor.
- Optimization: measure both drum and mi-drum if the hot path is shared.
- mi-drum strip work: gate **per sub-phase** against the 14.1 skeleton
  baseline, not once at the end.

Commit result JSONs to `bench-results/` when they are the new baseline.

## Memory

The firmware overrides the BSP's default FlexRAM split to **ITCM 8 banks /
DTCM 8 banks** (256 KB / 256 KB). OCRAM gets the remainder (~512 KB usable).

| region | capacity | typical binding concern |
|---|---|---|
| ITCM | 256 KB | `.text`; vendored C++ for mi-drum |
| DTCM | 256 KB | `.data`, `.bss`, stack, vector table |
| OCRAM | ~512 KB | `.rodata`, `.uninit`, `.heap`; engine struct, delay lines, reverb tanks |
| FLASH | 1984 KB | `.boot` |

### Placement rule

What matters is **accesses per sample × latency**, not raw size:

| data | placement |
|---|---|
| Not touched per sample (`MACHINE_INFO`, string tables, note map) | OCRAM, always fine |
| Per sample, data-dependent addressing (lookup tables, voice state) | DTCM |
| Per sample, sequential (delay lines, reverb combs) | Cached OCRAM is fine |

With the L1 caches off, every row collapses to "must be DTCM". The caches are
enabled in the current firmware; verify before relying on this.

### mi-drum memory note

`MiDrumEngine` is ~476 KB and cannot fit in DTCM, so it lives in cached OCRAM.
Warps, Ripples, and Stages objects also live in the engine struct.

> **This section used to end "so OCRAM is the binding region for mi-drum, not
> cycles." That was wrong, and it is why Phase 14 shipped at 290% of budget
> without anyone noticing.** The work optimised for *capacity* — does it fit in
> 512 KB — and the thing that actually bit was cycles. Retiring Warps and the
> Stages modulation bus for in-house equivalents took `6 sounding` from 205% of
> budget to 81.8% *and* OCRAM from 93.9% to 70.2%, so the two were never in
> tension — the vendored modules were simply expensive in both. See
> `docs/warps-vendoring.md`.

It is also close to the ceiling: 474,864 bytes measured against a 500 KB test
cap, and 268,560 of that is track-independent overhead — of which `SendFx` is
262,656 (192,000 stereo delay + 70,656 reverb). The send bus, not the voices, is
what makes the engine big, which is why Clouds is planned as a firmware
`.uninit` static outside the engine. `engine_size_fits_ocram_budget` prints the
current size and fails the build if it crosses 500 KB.

## Instruction census

`tools/checkasm.sh` counts instructions in the bench ELF independently of cycle
counts, to prove a flag actually reached the compiler.

```bash
tools/checkasm.sh
```

Look for:

- `vfma/vfms` — should appear once `-C target-cpu=cortex-m7` is active.
- `vdiv` — data-dependent latency; watch this if you replace a `recip`
  approximation with a real divide.
- `bl <sinf>` / `bl <floorf>` / etc. — libm calls surviving in the audio path.
  `sin_turns` should not emit `bl <sinf>`; `exp2_approx` still calls `floorf`
  and is the one known offender.

This counts static occurrences, not executions. Pair it with `benchloop.py`.

## Known measured results worth keeping in mind

- A Plaits voice costs roughly **2.6×** a drum machine voice per block.
- The send-FX tanks advance every block regardless of send levels, so `N + FX`
  and `N FX idle` are usually identical to the cycle.
- `Resonator` is ~10× cheaper as a shared send effect than as six per-track
  instances, because it recomputes all 24 mode coefficients on every call.
- Sustained machines (Dub Siren, Sweep FX) defeat the per-track idle early-out
  for the whole gesture, so a kit with one idles at "N-1 idle + 1 sounding".

### What the peak/average ratio is made of

The block deadline is set by the **peak**, and `6 sounding` runs at 1.35x its
own average while `idle` sits at 1.01x. Partly accounted for:

- **Not uneven track costs.** `1 sounding` shows 1.34x — a single voice on its
  own has essentially the same ratio as six. The kit's voices do differ a lot
  (Peaks BD 33.6k, SD 38.1k, HH 24.9k, **FM 71.8k**, Plaits 49.2k each, avg
  cy/block) but that spread is not what makes the peak.
- **Render alignment: fixed, worth 19%.** Every slot used to start at the same
  phase so all six crossed their 24-sample render boundary together. Spreading
  them took peak from 123.9% to 100.3% with the average unchanged. The spacing
  is provably optimal — see `MiSlot::render_phase`.
- **Cold cache after `panic()`: ~12 points.** 32 warm-up passes instead of 1
  drops `6 sounding` peak/avg from 1.35x to 1.19x. Not corrected, for the
  reason in the rejected table above.
- **Unexplained: the ~1.34x on a single voice.** It does not move with the
  voice block size (16 vs 24) or with warm-up, so it is neither render
  scheduling nor cache warming. Whatever it is, it is the thing to understand
  before cutting more DSP, because it sets the deadline.

### mi-drum per-unit costs

Derived from `mi-baseline.json` and the `mi-stages-chain.json` spike deltas, so
the next estimate does not have to re-derive them. Peak cycles, block 32:

| unit | cycles | how it was obtained |
|---|---|---|
| whole engine, every track silent | ~29,700 | `idle` scenario |
| one Plaits voice | ~38,900 | (`6 sounding` − `idle`) / 6 |
| one Stages segment | **never measured, now removed** | the ~2,100 that used to sit here came from `6 + LPG`, and the spike `Lpg` is **not** a Stages segment — it is a 64 B `plaits::LPGEnvelope`+`LowPassGate` in `mi_dsp::spike_stages`, against a 4,184 B `stages::SegmentGenerator`. Different C++ class, 65x the size. Nothing has measured a real segment. |
| one Overdrive spike stage | ~2,550 | (`6 + DRIVE` − `6 sounding`) / 6 |
| `Resonator` per track | ~32,000 | (`6 + RESON` − `6 sounding`) / 6 |
| `Resonator` as a single send | ~19,200 | `6 + RES SND` − `6 sounding` |

Warps, the Ripples SVF, the Peaks voices and Clouds are **not** in this table —
no bench has measured them yet. Anything quoting a per-unit figure for those is
an estimate, not a measurement.

## Things that were measured and rejected

Do not re-try these:

| change | result |
|---|---|
| `opt-level = 2` | +3.1% to +11.7% worse |
| `VOICE_BLOCK` 24 → 16 | **peak +8 points worse** (100.3% → 108.4%). 16 divides the 32-sample engine block exactly, so every block gets two renders and the 2/1/1 pattern disappears — but 1.5x the render calls costs more than the flattening saves. The average barely moved (74.3% → 73.2%); only the peak got worse. |
| `mi-bench` warm-up 1 → 32 passes | Drops `6 sounding` peak from 100.3% to 88.3% by excluding the cold-cache blocks after `panic()`. **Rejected as flattery, not accuracy**: hardware measures ~131% worst in continuous operation, so the 1-pass figure is already the *optimistic* one and more warm-up moves the bench further from the truth. |
| `-C llvm-args=-inline-threshold=500` | +1.0% to +1.4% cycles, +56% ITCM |

## Recording results

1. Run `tools/benchloop.py --label <name>`.
2. If this is a new baseline, commit `bench-results/<name>.json`.
3. If diffing, note the worst-case delta and any memory change in the PR
   description.
