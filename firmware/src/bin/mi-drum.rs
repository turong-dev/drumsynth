//! Mi-drum firmware binary.
//!
//! Thin wrapper: create a [`MiDrumEngine`] in `.uninit` OCRAM and hand it to
//! the shared [`firmware::runner`].

#![no_std]
#![no_main]

use firmware::runner;
use mi_drum_engine::MiDrumEngine;
use teensy4_panic as _;

/// The engine itself (~375 KB, dominated by the six Plaits voices and the
/// shared send-FX delay/reverb buffers) — in OCRAM via a `.uninit` static,
/// *not* the stack.
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
