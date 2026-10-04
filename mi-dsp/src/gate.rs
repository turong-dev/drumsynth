//! The gate-flag vocabulary shared across the vendored wrappers.
//!
//! These four values are the wire format the shims convert at their
//! boundaries: `mi_peaks_shim.cc` maps them onto Peaks' own
//! `peaks::GateFlags` bits, which are Peaks-local and not the same encoding
//! (see `docs/peaks-vendoring.md`).
//!
//! They lived in `stages.rs` until the Stages segment generator was retired —
//! the Peaks path had always borrowed them from there, which only ever made
//! sense while both wrappers existed.

/// Gate flag: low.
pub const GATE_LOW: u8 = 0;
/// Gate flag: high.
pub const GATE_HIGH: u8 = 1;
/// Gate flag: rising edge (starts a note).
pub const GATE_RISING: u8 = 2;
/// Gate flag: falling edge (ends one).
pub const GATE_FALLING: u8 = 3;
