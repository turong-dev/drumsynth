//! Mi-drum firmware binary.
//!
//! Thin wrapper: create a [`MiDrumEngine`] in `.uninit` OCRAM and hand it to
//! the shared [`firmware::runner`].

#![no_std]
#![no_main]

use firmware::runner;
use mi_drum_engine::MiDrumEngine;
use teensy4_panic as _;

/// The engine itself (343,296 bytes, dominated by the six Plaits voices and
/// the shared send-FX delay/reverb buffers) — in OCRAM via a `.uninit`
/// static, *not* the stack.
///
/// Unlike `bin/drum.rs`, this one cannot move to DTCM, and the reason is
/// worth knowing: DTCM is 320 KB total, so a 343 KB engine does not fit even
/// before the 16 KB stack. That matters, because moving the *drum* engine out
/// of OCRAM cut its worst-case bench scenario from 87.2% of budget to 39.3%
/// — OCRAM is reached over AXI and nothing here enables the L1 data cache, so
/// every FX buffer access is an uncached bus transaction. This device is
/// paying that cost in full.
///
/// The way out is a FlexRAM rebalance. `teensy4-bsp`'s build script hardcodes
/// ITCM 6 banks / DTCM 10, but `.text` uses only ~101 KB of the 192 KB ITCM,
/// so ITCM 4 / DTCM 12 would give 384 KB of DTCM — enough for this engine
/// plus the stack, with roughly 25 KB spare. That needs the firmware to
/// generate its own linker script via `imxrt-rt`'s `RuntimeBuilder` instead
/// of taking the BSP's, so it is left until this device is worked on.
#[link_section = ".uninit"]
static mut ENGINE_BUF: core::mem::MaybeUninit<MiDrumEngine> = core::mem::MaybeUninit::uninit();

#[teensy4_bsp::rt::entry]
fn main() -> ! {
    // SAFETY: `ENGINE_BUF` is `.uninit` OCRAM, written exactly once, here,
    // before interrupts are enabled — single-threaded init, same
    // requirement `new_in_place` documents.
    #[allow(unsafe_code)]
    let engine: &'static mut MiDrumEngine = unsafe {
        let p: *mut MiDrumEngine = core::ptr::addr_of_mut!(ENGINE_BUF).cast();
        MiDrumEngine::new_in_place(p)
    };

    runner::run(engine)
}
