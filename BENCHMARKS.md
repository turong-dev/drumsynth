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

`mi-bench` has one optional feature, `machine-sweep`, which appends a
per-machine cost table to the run. Off by default — see
[The per-machine sweep](#the-per-machine-sweep-24).

### Scenario naming

- `idle` — loaded kit, every track silent (`is_active` early-out exercised)
- `N sounding` — every track retriggered every block; worst-case voice cost
- `N FX idle` — FX tanks advancing on silence
- `N + FX` — realistic worst case with sends routed
- `<machine> bare` / `<machine> strip` — one machine alone, voice only and
  voice + strip (`machine-sweep` only)
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

## avg or peak? Use both, and know which one is noise

The honest answer depends on the device, and the project got this wrong
for a while. Worst per-scenario spread over **three runs of one
identical binary**, same session, no rebuild:

| | `avg` | `peak` |
|---|---|---|
| drum (`f-nrpn-drum`, `-repeat`, `-run3`) | 0.02% | 0.26% |
| mi-drum (`f-nrpn-mi`, `-repeat`, `-run3`) | **0.68%** | **7.70%** |

The first two drum runs were bit-identical in every scenario; the third
drifted by at most 264 cycles. mi-drum is a different animal: `peak`
swings 2–8% per scenario (`ALG1 fold` 7.7%, `4 sounding` 6.2%,
`6 + RES SND` 5.9%) while `avg` stays inside 0.7% and is usually under
0.3%.

So on mi-drum, `peak` has a noise floor of roughly ±5%, which is larger
than almost any change worth gating. `avg` has one of ±0.5%.

Most of this is now explained — the render-boundary mismatch, measured
below — but the conclusion stands: `max` is an extremum over 512 blocks
drawn from a wide distribution, so it moves even when nothing does.

**This invalidates a conclusion that was recorded as fact.** Issue #1's
finding 1 reported phase B′ as a +4.7% mi-drum regression on
`3 sounding` and +4.3% on `6 sounding`. Both are peak numbers. On `avg`
the same two scenarios moved −0.04% and −0.49%, and the sum over all
sixteen was **−0.23% — B′ made mi-drum slightly faster.** The regression
was peak noise, and a static diff of the two binaries agrees: not one
function on the audio path changed a single instruction.

Practical rule:

- **Gate on `avg`.** It is the statistic that reflects the work done.
- **Watch `peak`, and only believe it over ~8%** on mi-drum. On drum it is
  good to ~0.3% and worth reading directly.
- A `peak` move with no `avg` move is not a regression. Check `avg`
  before you spend a day on it — the JSONs in `bench-results/` carry
  both, so this costs nothing.
- Deadline safety still depends on real peak, so a peak that approaches
  budget matters even when it is noisy. It just cannot be *diffed*.

Most of the tail is now accounted for — see *What the peak/average ratio
is made of* below, which the per-block distribution finally cracked.

## Interpreting results

- Compare `avg` first; see the section above for why, and for when `peak`
  is trustworthy.
- Read the **distribution** block the harness prints under the main table.
  `p99/p50` is how wide the workload is; `max/p99` is how far the single
  worst block sits beyond it. A high `max/p99` with a normal `p99/p50` is
  a contaminated block, not a regression. `over budget` is the only
  number that says whether the deadline is actually missed.
- The `phase` column is the spread between the three positions in the
  voice-render cycle. Flat on `idle`; around 10% when six voices sound.
  A jump there means something changed about render scheduling.
- Compare against the relevant baseline (`baseline.json`, `mi-baseline.json`, or
  the 14.1 skeleton gate for mi-drum strip work).
- Watch the **worst-case scenario** first; a change that helps idle but hurts
  the worst case is not a win.
- Memory pressure matters: a cycle win that costs ITCM or DTCM you do not have
  is not a win.

## Two discontinuities in the saved baselines (#1 phase F)

Both are deliberate and both mean "do not diff across this point".

**`8+FX+CC` became `8+FX+NRPN`.** The drum bench's param-automation
scenario drove `apply_cc` on the flat CC map. That map is gone, so it
drives `apply_nrpn` instead — which resolves a `(group, index)` address
against the machine table before reaching the smoother. That scan is
inside the measured region on purpose: it is per-message work the device
really does now. The label changed with the path, so `benchloop.py`
reports the scenario as missing against a pre-F baseline rather than
quietly comparing two different things.

**mi-bench scenarios moved when the RNG was seeded.** `measure()` now
calls `seed_random(DEFAULT_RANDOM_SEED)` per scenario. `stmlib::Random`
is one process-global LCG shared by every Plaits engine and the firmware
never seeded it, so where a scenario started in the sequence depended on
how many draws the scenarios before it had made — which depends on how
many voices were sounding, which is the thing being measured. That
coupling is real and is now gone, so mi-drum baselines need re-pinning.

It did **not** fix the run-to-run variance it was introduced for. See
the next section; that turned out to be a different problem.

## Gate culture

Bench-gate every significant change:

- New machine or stage: add a worst-case scenario row, measure it, and record
  the delta.
- Refactor: assert the worst-case scenario's `avg` is unchanged within the
  noise floor — ~0.05% on drum, ~0.7% on mi-drum.
- Optimization: measure both drum and mi-drum if the hot path is shared.
- mi-drum strip work: gate **per sub-phase** against the 14.1 skeleton
  baseline, not once at the end.
- Control-plane work: the parameter path is not the audio path, but it is
  not free either — `8+FX+NRPN` is the scenario that sees it. On drum,
  where the bench is exact, one parameter moving every block costs
  **+1,504 cy/block** over `8 + FX`, of which **+187** is NRPN address
  resolution and the rest is the smoother's per-block recompute. Phase B′'s
  route dispatch costs **+8 cy/block** of that, measured as the
  `8+FX+CC` − `8 + FX` delta before and after it.

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
  own has essentially the same ratio as six. The kit's voices do differ, but
  that spread is not what makes the peak.

  > This bullet used to quote "Peaks BD 33.6k, SD 38.1k, HH 24.9k, **FM
  > 71.8k**, Plaits 49.2k each". **The FM figure was wrong by 4.5x and the
  > conclusion it was quoted for is unaffected.** Measured directly (#24),
  > `pk-fm` costs 28,525 cy/block with the strip and **6,330 bare** — the
  > second cheapest machine of the 28, not twice the dearest. The other three
  > Peaks figures were roughly right (28.8k / 29.9k / 29.3k measured). See
  > the per-machine sweep below, and #25, which this retires.
- **Render alignment: fixed, worth 19%.** Every slot used to start at the same
  phase so all six crossed their 24-sample render boundary together. Spreading
  them took peak from 123.9% to 100.3% with the average unchanged. The spacing
  is provably optimal — see `MiSlot::render_phase`.
- **Cold cache after `panic()`: ~12 points.** 32 warm-up passes instead of 1
  drops `6 sounding` peak/avg from 1.35x to 1.19x. Not corrected, for the
  reason in the rejected table above.
- **Mostly the 24-into-32 render mismatch, and it is now measured.**
  `VOICE_BLOCK` (24) does not divide `BLOCK` (32), so the pattern repeats
  every `lcm(24, 32) / 32` = **3 engine blocks**, over which each slot
  renders four times — twice in one of the three, once in each of the
  others. The bench reports mean cost grouped by block index mod 3 (the
  `phase` column), and it is flat to 0.0% on `idle`, which renders
  nothing, and −5%/+2%/+4% on every six-voice scenario.

  The model predicts the sign on the partial kits too, which is what
  makes it convincing rather than suggestive: `1`/`2 sounding` load only
  the first class so phase 0 is dearest; `4 sounding` leaves the third
  empty so phase 2 is cheapest; `3 sounding` splits 2/1/0 and runs
  +3.7/0.0/−3.7.
- **A `load_kit` used to throw the staggering away entirely.** Fixed in
  `7149f20`. Worth 10–25% of peak on every six-voice scenario, and the
  reason no bench saw it is that no bench changes a machine. If you add
  a scenario, consider whether it should.
- **What is left is a handful of genuinely contaminated blocks.** With
  the structural part understood, the residue shows up as `max/p99`
  around 1.4 on one or two scenarios per run, moving between runs, while
  every other scenario sits at 1.02–1.17. That is the rare-and-large
  signature — a few blocks out of 512, not a property of the workload.
  The timed region does not mask interrupts, which is the obvious
  candidate and the obvious next experiment.

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

Clouds is **not** in this table — no bench has measured it yet, and anything
quoting a per-unit figure for it is an estimate. The Peaks voices and the
strip as a whole (Ripples included) *are* measured now, by the per-machine
sweep below:

| unit | cycles | how it was obtained |
|---|---|---|
| one Peaks voice, bare | 6,123–8,624 | `<pk-*> bare`, sweep |
| one Plaits voice, bare | 8,361–29,170 | `<mi-*> bare`, sweep |
| the whole strip, per track | ~19,600 | `strip − bare`, averaged over all 28 |

Those are single-voice figures and the two above are six-track ones; the
caveat under the sweep explains why they do not reconcile by division.

Note what every row above has in common: it is a *delta between two six-track
scenarios*, divided by six. That is the only shape a knob-based decomposition
can take, and it is why the Peaks voices are missing — there is no knob that
removes a voice. The per-machine sweep below exists to fill that gap.

### The per-machine sweep (#24)

`mi-bench --features machine-sweep` measures every one of the 28 machines on
one track with nothing else sounding, twice: `bare` (the voice alone) and
`strip` (the voice plus mi-drum's strip).

```bash
tools/benchloop.py --bin mi-bench --label mi-machine-sweep \
    --features mi-drum,autoboot,machine-sweep
```

It does not go through `engine.process()`. It cannot: no parameter bypasses
Ripples or the four modulation generators, so a knob-based scenario cannot
separate voice from strip. Instead it drives one `MiSlot` directly, which is
where the line already is — `Slot::tick` is "one sample of output, pre-strip"
and `Slot::process_audio_strip` is the strip. Nothing in the engine changed
for it, so the baseline digest is untouched.

What the two columns give you:

- **`bare`** — the first honest per-voice figure for the Peaks models, which
  have only ever been measured with the strip attached. This is the number
  #25 needs: if a bare `pk-fm` is nowhere near 71.8k, that cost is in the
  strip or the FFI shim, not in `FmDrum::Process`.
- **`strip − bare`** — the strip's per-track cost, and the first measurement
  of the modulation generators at all, since they are inside it and no param
  gates them.

**Two caveats, and they matter more than the numbers.**

- **One voice alone has the 32 KB D-cache to itself; six do not.** One slot
  is 33 KB and six are ~200 KB, and this document already measures per-track
  cost rising 26% from three tracks to six for identical work (see #27). A
  machine measured alone is the **floor** of what it costs in a kit. Rank
  machines against each other with these figures; do not add them up and
  expect `6 sounding`.
- **`bare` is biased low by roughly 32 stores per block.** `tick` appends to
  the aux capture while `aux_filled < BLOCK`, and only `process_audio_strip`
  resets that counter — so with the strip off the capture saturates after the
  first block. Far under this device's ~4% noise floor, but it is a bias and
  not noise.

The sweep is off by default because it is 56 extra scenarios and +8 KB of
ITCM in a binary already at 86.8% of it. It runs after every existing
scenario and perturbs none of them, so a sweep run still diffs cleanly
against a baseline captured without it.

#### Results

`bench-results/mi-machine-sweep.json`, 2026-10-18. **avg** cycles per block,
which is the right statistic here — see the warning about `p50` below.

| machine | bare | strip | strip − bare |
|---|---|---|---|
| `pk-bd` | 6,123 | 28,813 | 22,690 |
| `pk-fm` | 6,330 | 28,525 | 22,195 |
| `mi-speech` | 8,361 | 29,169 | 20,808 |
| `pk-hh` | 8,497 | 29,281 | 20,784 |
| `pk-sd` | 8,624 | 29,911 | 21,287 |
| `mi-chiptune` | 11,302 | 31,362 | 20,060 |
| `mi-sixop-3` | 15,070 | 36,134 | 21,064 |
| `mi-sixop-2` | 15,159 | 36,113 | 20,954 |
| `mi-sixop-1` | 15,455 | 37,011 | 21,556 |
| `mi-waveshape` | 16,598 | 35,048 | 18,450 |
| `mi-sd` | 20,087 | 38,269 | 18,182 |
| `mi-noise` | 20,287 | 38,911 | 18,624 |
| `mi-hh` | 20,624 | 38,981 | 18,357 |
| `mi-fm` | 21,017 | 39,707 | 18,690 |
| `mi-va` | 21,694 | 40,303 | 18,609 |
| `mi-bd` | 21,860 | 40,440 | 18,580 |
| `mi-string-mach` | 22,118 | 40,275 | 18,157 |
| `mi-terrain` | 22,249 | 40,879 | 18,630 |
| `mi-wavetable` | 22,323 | 42,730 | 20,407 |
| `mi-chord` | 22,595 | 41,272 | 18,677 |
| `mi-swarm` | 22,892 | 41,244 | 18,352 |
| `mi-string` | 23,738 | 42,273 | 18,535 |
| `mi-va-vcf` | 24,855 | 44,737 | 19,882 |
| `mi-additive` | 25,803 | 44,052 | 18,249 |
| `mi-grain` | 25,850 | 44,343 | 18,493 |
| `mi-phase` | 26,167 | 44,994 | 18,827 |
| `mi-particle` | 26,658 | 47,884 | 21,226 |
| `mi-modal` | 29,170 | 47,885 | 18,715 |

**The strip costs 18,157–22,690 cycles per track and barely cares which
machine it is wrapped around.** Mean 19,608, total spread 25%, against a 4.8x
spread in the voices underneath it. On the four Peaks models it is 71–79% of
the track's whole cost: the voice is the cheap part.

**Peaks voices are the cheapest things in the catalogue, not the dearest.**
All four sit between 6,123 and 8,624 bare. `mi-modal`, the most expensive
machine, is 4.8x `pk-bd`.

**The method checks out against the engine.** `pk-bd strip` (28,813) plus
`idle` (30,029) is 58,842, or 92.5% of the `1 sounding` scenario's 63,609 —
and `1 sounding` *is* a Peaks bass drum, track 0 of `DEFAULT_KIT`, measured
the ordinary way through `engine.process()`. The missing 7.5% is the
track-level mixing and per-track engine scaffolding the sweep omits by
design. Two independent paths to the same voice agree.

**Read `avg`, not `p50`, on these rows.** One voice renders 24 samples into
32-sample blocks, so over three blocks it renders four times — two renders in
one block, one in each of the others. `p50` is therefore a one-render block
and understates the real per-block mean by ~25%, which is exactly what the
~90% `phase` spread on every `bare` row is reporting. In a six-voice kit the
staggering smooths this to ~9% and the distinction stops mattering; alone it
does not.

**Unexplained, do not quote yet: the three `mi-sixop` rows.** They are the
only machines where `avg` falls *below* `p50` (15,455 against 18,264 on
`mi-sixop-1`) and the only ones with a `phase` spread near 290% rather than
~90%. Something about their render is not the 2/1/1 pattern every other
machine shows. The ranking is probably fine — all three agree with each other
to 2.5% — but the shape of their distribution has not been accounted for.

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
