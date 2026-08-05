//! Drum voices.
//!
//! Each voice follows the same shape:
//!
//! - a `Params` struct of plain, human-meaningful values (seconds, Hz, 0..1)
//! - a voice struct holding DSP state plus coefficients derived from params
//! - `set_params` doing the expensive maths, `tick` doing none of it
//!
//! Adding a voice means adding a module here, a field on
//! [`crate::Params`], and an arm on [`crate::VoiceId`]. Nothing else changes.

mod hat;
mod kick;
mod snare;

pub use hat::{Hat, HatParams};
pub use kick::{Kick, KickParams};
pub use snare::{Snare, SnareParams};
