//! Generate this firmware's linker script, overriding the BSP's memory map.
//!
//! `teensy4-bsp` 0.6.0 hardcodes a FlexRAM split of **ITCM 6 / DTCM 10** banks
//! in its own `build.rs`, and emits `t4link.x`. That split suits a device
//! whose engine lives in DTCM, which is what the drum device was. It is the
//! wrong shape for mi-drum:
//!
//! ```text
//!                   capacity      used     
//!   ITCM  6 banks     196,608   171,748    87.4%   <- binding
//!   DTCM 10 banks     327,680   165,048    50.4%   <- 160 KB idle
//! ```
//!
//! `MiDrumEngine` is ~343 KB and so cannot live in DTCM at all (320 KB), which
//! leaves half of DTCM unused while the vendored Plaits C++ — 74 KB of `.text`,
//! 43% of the total — runs ITCM out of room. Phase 14 adds more MI stage code
//! on top of that, so ITCM, not the cycle budget, was going to be what stopped
//! it.
//!
//! This script moves two banks the other way, to **ITCM 8 / DTCM 8**:
//!
//! ```text
//!   ITCM  8 banks     262,144   171,748    65.5%
//!   DTCM  8 banks     262,144   165,048    63.0%
//! ```
//!
//! Everything else is copied verbatim from the BSP's build script, so the only
//! deliberate difference from a stock `teensy4-bsp` image is the bank split.
//! Check `teensy4-bsp-0.6.0/build.rs` when bumping the BSP: if it changes
//! anything but the banks, mirror it here.
//!
//! # This constrains the drum device
//!
//! One linker script serves every binary in this crate. `DrumEngine` is
//! ~269 KB and *was* in DTCM, at 304,300 of 327,680 used — 92.9%, with only
//! 23 KB spare. It does not fit in 8 banks, so it moves to `.uninit` OCRAM
//! (see `ENGINE_BUF` in `bin/bench.rs`). With the L1 caches on that costs a
//! measured ~1.2% of the cycle budget, which the optimisation pass in BENCHMARKS.md
//! established and which is affordable at 33.7%. With the caches *off* it
//! would be catastrophic — so if you ever disable the `cache` feature, this
//! trade stops being a good one.

use imxrt_rt::{Family, FlexRamBanks, Memory, RuntimeBuilder};

fn main() {
    RuntimeBuilder::from_flexspi(Family::Imxrt1060, 1984 * 1024)
        .flexram_banks(FlexRamBanks {
            ocram: 0,
            // The one deliberate deviation from the BSP: 6/10 becomes 8/8.
            itcm: 8,
            dtcm: 8,
        })
        .heap(Memory::Ocram)
        .heap_size(16 * 1024)
        .heap_size_env_override("TEENSY4_HEAP_SIZE")
        .stack(Memory::Dtcm)
        .stack_size(16 * 1024)
        .stack_size_env_override("TEENSY4_STACK_SIZE")
        .vectors(Memory::Dtcm)
        .text(Memory::Itcm)
        .data(Memory::Dtcm)
        .bss(Memory::Dtcm)
        .uninit(Memory::Ocram)
        // A distinct name from the BSP's `t4link.x`. Both scripts are
        // generated and both directories end up on the link search path, so
        // the name is what selects ours — see `.cargo/config.toml`.
        .linker_script_name("drumsynth-link.x")
        .build()
        .unwrap();
}
