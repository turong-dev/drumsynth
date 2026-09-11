//! Drum-device firmware binary.
//!
//! Thin wrapper: create a [`DrumEngine`] in DTCM and hand it to the shared
//! [`firmware::runner`].

#![no_std]
#![no_main]

use drum_engine::DrumEngine;
use firmware::runner;
use teensy4_panic as _;

/// The engine itself (~266 KB, almost all of it the send-FX delay/reverb
/// buffers) — in DTCM via a plain `.bss` static, *not* the stack.
///
/// This used to be `#[link_section = ".uninit"]`, which put it in OCRAM.
/// OCRAM is reached over the AXI bus and nothing in this firmware or in
/// teensy4-bsp ever enables the L1 data cache, so every delay-line and
/// reverb-tank access was an uncached bus transaction. Moving it to DTCM,
/// which `t4link.x` aliases `REGION_BSS` to and which is zero-wait-state,
/// cut the worst-case bench scenario from 87.2% of budget to 39.3%.
///
/// It fits with about 29 KB to spare. The linker enforces that, so a future
/// growth in engine size fails the build rather than misbehaving on the
/// bench.
static mut ENGINE_BUF: core::mem::MaybeUninit<DrumEngine> = core::mem::MaybeUninit::uninit();

#[teensy4_bsp::rt::entry]
fn main() -> ! {
    // SAFETY: `ENGINE_BUF` is DTCM `.bss`, written exactly once, here,
    // before interrupts are enabled — single-threaded init, same
    // requirement `new_in_place` documents.
    #[allow(unsafe_code)]
    let engine: &'static mut DrumEngine = unsafe {
        let p: *mut DrumEngine = core::ptr::addr_of_mut!(ENGINE_BUF).cast();
        DrumEngine::new_in_place(p)
    };

    runner::run(engine)
}
