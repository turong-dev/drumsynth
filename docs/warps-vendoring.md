# Warps vendoring, and what it costs

Phase 14.2 put `warps::Modulator` in the mi-drum strip as a per-track shaping
stage. This note records what that cost when it was finally measured, and why.

> **Status: removed.** `MiSlot` now runs `core::dsp::shaper::Shaper`. The
> decision was to stop trying to make Warps fit, take the parts of it that are
> cheap, and revisit the shaper once the engine works end to end. The vendored
> tree is still in `mi-dsp/vendor/warps` — `warps::Oscillator` is still used as
> the strip's modulator source — so the tuning levers below are kept against
> that later pass, not deleted.

Measured before and after, six tracks sounding, avg cycles per block:

| | Warps | house shaper | |
|---|---|---|---|
| `6 sounding` | 818,952 (204.7%) | **418,563 (104.6%)** | 1.96x |
| `6 + MOD on` (worst case) | 1,078,046 (269.5%) | **489,771 (122.4%)** | 2.20x |
| the stage itself, per track | 79,855 | **13,448** | 5.9x |
| ITCM | 89.8% | 85.2% | |
| OCRAM | 93.9% | 89.2% | |

The four `stages::SegmentGenerator`s went the same way immediately after, for
the same reason — see "The modulation bus" below. Together:

| | Warps + Stages | house shaper + house bus | |
|---|---|---|---|
| `6 sounding` | 818,952 (204.7%) | **327,369 (81.8%)** | 2.50x |
| `6 + MOD on` | 1,078,046 (269.5%) | **396,931 (99.2%)** | 2.72x |
| ITCM | 89.8% | **81.1%** | -22,768 B |
| OCRAM | 93.9% | **70.2%** | -124,272 B |

What remains over budget is the voices and the `powf` in the Ripples cutoff
loop, which is still per sample, per track and ungated.

## The modulation bus

The strip held four `stages::SegmentGenerator`s — 4,184 bytes each, 16,736 per
track, ~98 KB across the kit — and this is everything it ever asked of them:

```rust
lfo1.set_parameters(rate, 0.5);          // one looping ramp, shape fixed at 0.5
lfo2.set_parameters(rate + 0.12, 0.5);
env1.configure_ad(attack, decay);        // two ramps, shape fixed at 0.5
env2.configure_ad(attack, decay);
```

Two free-running LFOs and two AD envelopes, with the shape parameter hardcoded
at every call site. `core::dsp::lfo::Lfo` and `core::dsp::ahd::AhdEnv` are
**48 bytes each** and offer more than was reachable: six waveshapes against the
one, and a real decay curve through `set_decay_coeff`.

Two things worth knowing about the swap:

- **`Lfo` is block-rate.** Writing its value flat across a block steps the
  modulation once every 32 samples, which at the top of the rate range (~19 Hz,
  1.3% of a cycle per block) zippers audibly on a resonant cutoff. The buffers
  ramp between block values instead — one add per sample.
- **`AD.ATK` had to be remapped.** Stages took these as normalised *segment
  parameters* and applied its own curve, so the strip's linear
  `0.001 + 0.999 * macro` was a passthrough, not a time map. Fed straight to
  `AhdEnv` as seconds it put the shipped default at 51 ms — slower than the
  transient it is supposed to shape, which
  `ad_filter_depth_needs_the_filter_closed_first` caught immediately. Now
  exponential over 0.5 ms .. 1 s, default ~0.7 ms.

Everything below is measured on hardware via `tools/benchloop.py --bin
mi-bench`, 600 MHz, 32-sample block, 400,000 cycles of budget. Figures marked
*structural* are read off the vendored source rather than timed.

## What it costs

| scenario | cy/track/block | cy/sample | six tracks |
|---|---|---|---|
| Warps, shipped default algorithm | 82,183 | 2,568 | 204.7% of budget |
| Warps, cheapest algorithm | 43,952 | 1,373 | ~110% |
| ADAA shaper (`core::dsp::shaper`) | ~7,400 | ~230 | 96% total engine |

`PLAN.md` estimated the whole stage at **2,000-5,000 cycles per track**. The
*irreducible floor* — the part no algorithm choice can reach — is 43,952, nine
times the top of that range.

The estimate was not wrong so much as inherited. `2,000-5,000` brackets the
September spike measurements of `plaits::LPGEnvelope`+`LowPassGate` (2,109) and
`plaits::Overdrive` (2,548) almost exactly. When 14.2 swapped the
implementation, the row kept the old numbers and nobody re-derived them for a
module that oversamples 6x. It was never an estimate of Warps.

## Why it is expensive

Three reasons, all consequences of Warps being a *module* rather than a strip
stage.

**1. It oversamples 6x, unconditionally.** `kOversampling = 6`, with two
upsamplers and one downsampler at 48 taps. *Structural:* each converter is
`filter_size` multiply-accumulates per input sample regardless of ratio, so
that is `3 x 48 = 144 MACs/sample` before any cross-modulation runs. It
executes identically for `ALGORITHM_XFADE`, which is linear and generates
nothing to alias, and for `ALGORITHM_NOP`, which returns its input.

**2. It is a 16-bit module.** `Modulator::Process` takes and returns
`ShortFrame`, so `mi_warps_shim.cc:54-62` round-trips `f32 -> int16 -> f32` per
sample per channel. That is cycles spent and a 32-bit signal path quantised to
16 bits, to model an ADC and a DAC that are not there. It is also why
`6 no WARPS` — Warps bypassed, `Modulator::Process` short-circuited — is still
*more expensive* than running the ADAA shaper for real.

**3. It assumes a whole CPU.** One stereo pair at 96 kHz on an STM32F4 with
nothing else running, against six instances at 48 kHz sharing a core with MIDI,
USB, the grid and the sequencer.

## The default is the second most expensive algorithm

Measured across all six entries of `xmod_table_`, as Warps cost above the
bypassed floor:

| entry | pair | cy/track |
|---|---|---|
| ALG1 | FOLD + analog ring | 91,162 |
| **ALG0** | **XFADE + FOLD** — the shipped default | **82,183** |
| ALG2 | analog + digital ring | 52,761 |
| ALG4 | XOR + comparator | 47,263 |
| ALG3 | digital ring + XOR | 44,045 |
| ALG5 | comparator + NOP | 43,952 |

Each entry computes **both** its algorithms and interpolates, so the pair is
what matters. ALG0 is expensive because `XFADE` does two `Interpolate` table
lookups and `FOLD` one — three per oversampled sample, the most of any entry,
against ALG5's zero. The arithmetic closes: the 205,355-cycle gap across six
tracks over `192 oversampled samples x 6 tracks x 3 lookups` is 59 cycles per
lookup, which is right for a table read with a float-to-int convert and a lerp.

Moving `WARP.ALG` off 0.0 is worth **47% of Warps' cost** and nothing else.

## Tuning levers, if Warps stays

Two independent constants in `vendor/warps/dsp/modulator.h`, both plain
literals, in a file that already carries local reductions of this exact kind
(`kNumBands`, `kDelayLineSize`, `kMaxFilterBankBlockSize`, all "Reduced from
...; vocoder consumes too much memory"). The same move, now for cycles.

```cpp
const size_t kOversampling = 6;
SampleRateConverter<SRC_UP,   kOversampling, 48> src_up_[2];
SampleRateConverter<SRC_DOWN, kOversampling, 48> src_down_;
```

- **`filter_size` (48)** sets the SRC cost — `3 x filter_size` MACs/sample.
- **`kOversampling` (6)** sets how many times the cross-modulation runs.

`SRC_FIR` is specialised per `(ratio, filter_size)`, so the coefficients have
to exist. **They already do.** `sample_rate_conversion_filters.h` carries three
matched up/down pairs:

| specialisation | used by | status |
|---|---|---|
| `<_, 6, 48>` | `Modulator` | live |
| `<_, 4, 48>` | `FilterBank` | dead — compiled, never linked |
| `<_, 3, 36>` | `FilterBank` | dead — compiled, never linked |

`FilterBank` does not survive the link: `Modulator::vocoder_` is commented out
at `modulator.h:199` and `rust-objdump -d` on `mi-bench` finds zero `FilterBank`
symbols. So **6x/48 -> 3x/36 is a drop-in**, using upstream's own filter design,
no new coefficients:

- SRC: `3 x 48 = 144` -> `3 x 36 = 108` MACs/sample (-25%)
- cross-modulation: `size x 6` -> `size x 3` (-50%)

The cost is headroom: the fold point drops from 144 kHz to 72 kHz and 36 taps
rejects less than 48. For drum transients through a stage mixed at
`WARP.MIX = 0.35` that is a far easier trade than for a sustained tone.
`cargo run -p render -- shaper` is the A/B.

## What is dead in this vendoring

Worth knowing before tuning anything, because none of it is reachable:

- **`FilterBank` / the vocoder.** `Modulator::vocoder_` commented out; no
  symbol survives the link. `kSampleMemorySize` evaluates to 0 as a result,
  which is why `filter_bank.h` carries a `+ 1` local fix — the zero-length
  array was a legal-code problem, not a behavioural one.
- **The internal carrier.** `3378389` pinned `Carrier::External` permanently,
  which also left `parameters_.note` unread.
- **`ProcessEasterEgg`.** Linked but never enabled. An earlier draft of
  `PLAN.md` claimed Warps' drive doubled as a dry/wet mix citing a line inside
  it; the line never runs here. The *live* `channel_drive` does behave that way
  (`wet_dry = 1 - channel_drive`), which is why driving Warps hard also pulls
  it back toward dry — and why the ADAA shaper, which has no such coupling,
  reads ~4 dB louder at the same macro value.

## The in-house alternative

`core::dsp::shaper::Shaper` covers the same interface — `set_bypass`,
`set_parameters(algorithm, timbre, drive)`, `process_dual` — using first-order
antiderivative antialiasing in place of oversampling. **48 bytes against
Warps' 4,112**, and roughly an order of magnitude cheaper.

Measured side by side in one render (`cargo run -p render -- shaper`), the
shipped kit at default macros: Warps and ADAA both land at **-22.7 dBFS**, and
the two differ from each other by **-17.4 dB** while each differs from the
unshaped reference by -13.7 dB and -11.2 dB respectively. The two
implementations are closer to each other than either is to no shaping — the
same kind of effect, not a cheap substitute.

What is *not* covered: three of Warps' six algorithms — `XOR`, `COMPARATOR`,
and the diode `analog ring` as distinct from the digital one. The first two are
broadband by construction and are exactly the ones that genuinely need
oversampling, so they are not cheaply replicable by this route; a diode ring is
if it is wanted.

**A/B at `algorithm = 0.0` only.** Warps dispatches across six table entries and
`Shaper` across three, so the same `WARP.ALG` selects different things in the
two: at 0.5, Warps runs `XOR + COMPARATOR` while `Shaper` runs the sine fold.
Comparing at a fixed macro value compares two different effects. Until a common
algorithm map exists, 0.0 is the only honest comparison point — both sit at the
crossfade end of their tables there.
