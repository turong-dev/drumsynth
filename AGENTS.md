# Agent instructions

How to work on the drum engine without breaking it.

## Project layout

```text
drumsynth/
├── core/                 `device-core` crate: DSP, track, macros, MIDI parser
├── devices/
│   ├── drum/             `drum-engine` crate: Rust drum machines
│   └── mi-drum/          `mi-drum-engine` crate: Peaks + Plaits MI device
├── mi-dsp/               vendored Mutable Instruments C++ + FFI wrappers
├── render/               host binary: WAV render, sweeps, device registry
├── firmware/             Teensy 4.1 firmware (excluded from workspace)
│   ├── src/bin/bench.rs       drum cycle bench
│   ├── src/bin/mi-bench.rs    mi-drum cycle bench
│   ├── src/bin/drum.rs        drum firmware
│   └── src/bin/mi-drum.rs     mi-drum firmware
├── tools/
│   ├── benchloop.py      closed-loop build/flash/capture/diff harness
│   └── checkasm.sh       instruction census
├── BENCHMARKS.md         how to measure and gate changes
├── DESIGN.md             long-lived architecture decisions
└── PLAN.md               active Phase 14 work only
```

## Build and test

```bash
# host tests — run these first
cargo test

# host render
cargo run -p render -- render
cargo run -p render -- sweep <machine> <macro> --from 0.1 --to 0.8 --steps 12

# mi-drum baseline test (digest gate)
cargo test -p render mi_drum_baseline_is_unchanged

# cross-compile (catches std/alloc leakage)
cargo build -p device-core --target thumbv7em-none-eabihf
cargo build -p drum-engine --target thumbv7em-none-eabihf
cargo build -p mi-drum-engine --target thumbv7em-none-eabihf

# firmware benches (requires Teensy and toolchain)
cd firmware
cargo build --release --bin bench --features autoboot
cargo build --release --bin mi-bench --features mi-drum,autoboot
```

`firmware/` is excluded from the workspace. Always build it from inside
`firmware/`, not with `--manifest-path` from the root, or it will build for the
host and fail in `bsp::rt`.

## Hard constraints

- Engine crates (`core`, `drum-engine`, `mi-drum-engine`) are `#![no_std]` and
  have no `alloc`. No `Vec`, `Box`, `String`, or panicking allocations in the
  audio path.
- Everything in the engine is `f32`; block size is compile-time constant.
- No `unsafe` in `core` or device crates. `unsafe` is allowed only inside
  `mi-dsp`.
- Do not change existing macro slot indices or the CC-map layout. These are a
  binary ABI shared with firmware and the Deluge.
- Do not reintroduce `libm::sinf` into the per-sample audio path. Use
  `fast::sin_turns`.

## Before changing anything

1. Read `DESIGN.md` for the decision this touches.
2. Read `BENCHMARKS.md` if the change affects the hot path, adds a machine, or
   adds an MI stage.
3. Run `cargo test`.
4. For mi-drum changes, run the baseline test.

## Bit-identity gates

- `drum-engine`: host renders should stay bit-identical to `out.baseline.wav`
  for unchanged code paths.
- `mi-drum`: `mi_drum_baseline_is_unchanged` in `render` tests a committed
  FNV-1a digest. If a change is intentional, re-pin the digest and commit the
  new value; do not silently move it.

## Adding a machine (drum device)

1. Add module under `devices/drum/src/machines/`.
2. Add variant to `MachineId` and `MachineSlot` in `devices/drum/src/machines/mod.rs`.
3. Add `MachineInfo` row to `MACHINE_INFO` (append, do not insert).
4. Add tests: silent-until-struck, velocity-scales, decays-to-silence,
   extreme-macros-do-not-produce-nans.
5. Update renderer `MachineArg` if needed.
6. Add a bench scenario if the voice is heavy or unusual.

## Adding an MI stage (Phase 14)

See `PLAN.md`. Gate per sub-phase against the 14.0 baseline. Do not touch the
core strip or the drum device's determinism contract.

## Common commands

```bash
# clippy
cargo clippy --all-targets

# format
cargo fmt

# render mi-drum to listen to the baseline
cargo run -p render -- mi-drum

# cycle bench (hardware required)
tools/benchloop.py --bin bench --label my-change --baseline baseline

# instruction census
tools/checkasm.sh
```

## When in doubt

- Keep changes minimal.
- Prefer additive changes; never reorder macro slots or machine enum indices.
- Ask before adding dependencies, especially anything that pulls in `std` or
  `alloc`.
- If a change affects cycles or memory on Teensy, it must be bench-gated before
  merging.
