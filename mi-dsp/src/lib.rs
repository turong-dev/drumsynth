//! Mutable Instruments C++ DSP wrappers for synth devices.
//!
//! This crate bridges the no-alloc Rust device core to the open-source
//! Mutable Instruments (MI) C++ code. The FFI boundary is block-rate only:
//! Rust prepares parameter structs, calls an extern `render_block` once per
//! audio block, and reads back interleaved 16-bit frames.
//!
//! Object ownership is held by Rust: each wrapper is an aligned byte array
//! into which the C++ object is placement-new'd at construction time. No
//! heap allocation occurs on either side of the boundary.
//!
//! # Safety
//!
//! This is the only crate in the workspace that uses `unsafe`. It is confined
//! to the small amount needed for the C FFI calls and the placement-new
//! storage.

#![no_std]
#![warn(missing_docs)]

pub mod plaits;
pub mod sys;
