//! Drum-device firmware binary.
//!
//! Thin wrapper: create a [`DrumEngine`] in `.uninit` OCRAM and hand it to
//! the shared [`firmware::runner`].

#![no_std]
#![no_main]

use drum_engine::DrumEngine;
use firmware::runner;
use teensy4_panic as _;

/// The engine itself (~266 KB, almost all of it the send-FX delay/reverb
/// buffers) — in OCRAM via a `.uninit` static, *not* the stack.
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
