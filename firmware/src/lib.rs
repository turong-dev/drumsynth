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
