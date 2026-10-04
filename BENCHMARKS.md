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
an estimate; `PLAN.md` marks which is which.

## Things that were measured and rejected

Do not re-try these:

| change | result |
|---|---|
| `opt-level = 2` | +3.1% to +11.7% worse |
| `-C llvm-args=-inline-threshold=500` | +1.0% to +1.4% cycles, +56% ITCM |

## Recording results

1. Run `tools/benchloop.py --label <name>`.
2. If this is a new baseline, commit `bench-results/<name>.json`.
3. If diffing, note the worst-case delta and any memory change in the PR
   description.
