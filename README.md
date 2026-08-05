# drumsynth

A headless drum synth for Teensy 4.1, structured so that almost all of the
work happens before any hardware is involved.

## The shape of it

```
drumsynth/
├── engine/     no_std, no alloc, no hardware. All the DSP.
├── render/     host binary. WAV output, parameter sweeps, live playback.
└── firmware/   Teensy 4.1 target. Thin.
```

The engine crate knows nothing about peripherals. It takes triggers and
parameters, and fills buffers. That single decision is what lets you develop
the interesting part in a normal edit-run loop, with `cargo test`, at native
speed, with no flashing and no probe.

Both consumers link it unmodified:

```
   render/  (host)          firmware/  (Teensy 4.1)
       │                          │
       └──────────┬───────────────┘
                  ▼
            drum-engine
```

## Start here

```bash
cargo test                              # engine tests, host speed
cargo run -p render -- render           # writes out.wav
cargo run -p render -- sweep kick-decay --from 0.1 --to 0.8 --steps 12
```

The sweep mode is the one that earns its keep. Twelve kicks with different
decay times, rendered faster than real time, ready to flip between in an
editor. Much faster than turning a knob.

With `--features live` you also get real-time playback through your sound
card.

## Answering "am I writing this within the hardware's limits"

Two mechanisms, neither of which needs a working audio path.

**Compile-time.** The engine is `#![no_std]` with no `alloc`, so `Vec`, `Box`
and `String` do not exist inside it — you cannot accidentally allocate in the
audio callback because the option is absent. Everything is `f32`; block size
is a compile-time constant; buffers are owned by the engine struct. CI
cross-compiles for `thumbv7em-none-eabihf` on every push, which catches `std`
leaking in through a dependency on the commit that introduced it.

**Run-time.** `firmware/src/bin/bench.rs` flashes to a bare Teensy with
nothing attached — no DAC, no SD card, no jack — and reports what
`engine.process()` actually costs:

```
voices=3 idle     avg=  1042 cy  peak=  1108 cy   0.3% of budget
voices=3 sounding avg= 21883 cy  peak= 23104 cy   5.5% of budget
```

The budget at 600MHz, 48kHz, 32-frame blocks is 400,000 cycles per block, or
12,500 per output frame. Size against *peak*, not average: the callback has to
make its deadline every time, and a mean that fits while the peak does not is
a click you will hear.

## Hardware

| | |
|---|---|
| Board | Teensy 4.1 (600MHz Cortex-M7, 1MB RAM, microSD on SDIO) |
| DAC | PCM5102A or PCM5100 I2S breakout |
| MIDI | USB device, or DIN on LPUART6 |

**Use a PCM5102A rather than the Teensy Audio Shield.** Those parts are
hardware-strapped — no I2C, no register writes, no codec driver to find or
write. The Audio Shield's SGTL5000 needs an I2C driver that does not currently
exist in Rust, and for an output-only instrument there is no reason to take
that on.

Wiring:

```
Teensy 4.1              PCM5102A
──────────              ────────
pin 21 (BCLK)   ──────► BCK
pin 20 (LRCLK)  ──────► LCK
pin 7  (OUT1A)  ──────► DIN
3.3V            ──────► VIN
GND             ──────► GND
```

Most breakouts want SCK tied low for internal PLL mode; check your silkscreen.

## Status

| | |
|---|---|
| Engine, DSP, voices | written, tested |
| MIDI parser | written, tested (running status, real-time interleaving) |
| Host renderer | written |
| Cycle bench | written, builds for target, **unverified on hardware** |
| Audio output | **not implemented** — see `TODO(sai)` in `firmware/src/main.rs` |

### The SAI situation

`imxrt-hal` gained a SAI driver in 0.6.0, released 26 July 2026. That was the
blocker: without it there was no audio transport at all on this chip in Rust,
and you would have been writing the DMA'd I2S engine from the reference manual.

But `teensy4-bsp` bundles its own `imxrt-hal`, and it may not have bumped yet.
Check before planning any audio work:

```bash
cd firmware && cargo tree | grep imxrt-hal
```

If it is below 0.6, your options are to wait, to open a PR bumping it, or to
depend on `imxrt-hal` and `imxrt-ral` directly and use `teensy4-bsp` only for
the runtime and pin definitions.

None of this blocks the bench binary, which needs no audio at all.

### Compilation status

The engine and host crates build clean, and the firmware's `bench` binary
compiles for `thumbv7em-none-eabihf` (needed one dep fix — `teensy4-panic`
is 0.3, not 0.10 — and three small API-drift fixes against `cortex-m` 0.7 /
`imxrt-hal` 0.5). The audio binary (`main.rs`) still carries the `TODO(sai)`
gap and is not expected to do anything useful until that is filled.

## Where to go next

1. `cargo test && cargo run -p render -- render`, listen to `out.wav`
2. Flash `bench`, find out what you actually have to spend
3. Design voices against that number
4. Fill in `TODO(sai)` when you want to hear it out of a jack

Samples for texture, when you get there: the Teensy's microSD is on SDIO
rather than SPI, so it is fast. Load a kit into RAM at boot rather than
streaming — drum one-shots are short, 1MB holds a sensible kit, and you skip
the ring buffers and the card-speed dependency entirely.

## Licence

MIT OR Apache-2.0.
