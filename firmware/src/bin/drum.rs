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
/// buffers) — in `.uninit` OCRAM, *not* the stack.
///
/// This has been in both places, and the history matters because the reason
/// changed underneath it:
///
/// 1. Originally OCRAM. That was slow — but not because OCRAM is slow.
///    Nothing in this firmware or in teensy4-bsp enabled the L1 data cache,
///    so every delay-line and reverb-tank access was an uncached bus
///    transaction.
/// 2. Moved to DTCM, cutting the worst case from 87.2% of budget to 39.3%.
///    That read as a placement win; it was mostly a caching win.
/// 3. The `cache` feature then enabled the L1 caches, which recovered 98% of
///    what the DTCM move bought. DTCM placement was now worth ~1.2%.
/// 4. Back to OCRAM here, to pay that 1.2%. Phase 14 rebalanced FlexRAM to
///    ITCM 8 / DTCM 8 (see `build.rs`) because the vendored Plaits C++ makes
///    ITCM the binding constraint on the mi-drum device, and 269 KB does not
///    fit in 8 DTCM banks.
///
/// So this placement is only affordable *because* the caches are on. If you
/// disable the `cache` feature, expect the 87.2% number back.
#[link_section = ".uninit"]
static mut ENGINE_BUF: core::mem::MaybeUninit<DrumEngine> = core::mem::MaybeUninit::uninit();

#[teensy4_bsp::rt::entry]
fn main() -> ! {
    // SAFETY: `ENGINE_BUF` is `.uninit` OCRAM, written exactly once, here,
    // before interrupts are enabled — single-threaded init, same
    // requirement `new_in_place` documents.
    #[allow(unsafe_code)]
    let engine: &'static mut DrumEngine = unsafe {
        let p: *mut DrumEngine = core::ptr::addr_of_mut!(ENGINE_BUF).cast();
        DrumEngine::new_in_place(p)
    };

    runner::run(engine)
}
