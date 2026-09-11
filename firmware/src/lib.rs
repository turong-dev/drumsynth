//! Shared firmware support for synth devices.
//!
//! This crate is `no_std`. It provides the audio ISR, USB MIDI transport, and
//! a generic [`runner`] that wires a [`DeviceEngine`](drum_engine::engine::DeviceEngine)
//! implementation to the hardware. Device-specific binaries in `src/bin/`
//! create their engine and call [`runner::run`].

#![no_std]
#![warn(missing_docs)]

pub mod audio;
pub mod runner;
pub mod usb;

/// Enable the Cortex-M7 L1 instruction and data caches.
///
/// Neither `teensy4-bsp` nor `cortex-m-rt` does this; Teensyduino does, by
/// default, on the same chip. It is worth a great deal: with the caches off,
/// the drum engine in OCRAM costs 87.2% of the block budget, and with them on
/// it costs 36.0%. TCM is unaffected either way — ITCM and DTCM sit on the TCM
/// ports and bypass the caches — so this only changes accesses to OCRAM
/// (0x20200000) and flash.
///
/// Call it before anything touches bulk data, and before the audio interrupt
/// is unmasked.
///
/// # DMA
///
/// Enabling the D-cache makes coherency the caller's problem for any memory a
/// bus master writes behind the core's back. Today nothing here is exposed:
/// the SAI path is FIFO-interrupt driven rather than DMA, and the USB endpoint
/// memory is a plain `static`, so it lands in `.bss` — DTCM — which is never
/// cached. Adding SDIO or SAI DMA means auditing that first, and either
/// placing those buffers in DTCM or adding explicit clean/invalidate.
///
/// Takes the peripherals by reference rather than calling
/// `Peripherals::take()` itself, because `bench` already owns them for the
/// DWT cycle counter and `take()` succeeds only once.
#[cfg(feature = "cache")]
pub fn enable_caches(
    scb: &mut cortex_m::peripheral::SCB,
    cpuid: &mut cortex_m::peripheral::CPUID,
) {
    scb.enable_icache();
    scb.enable_dcache(cpuid);
}
