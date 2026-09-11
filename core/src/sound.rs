//! A serialisable sound preset: voice slot + macros + strip.
//!
//! `Sound` is deliberately `Copy` and small enough to be handed around at
//! trigger time as a Syntakt-style *sound lock*.

use crate::slot::{Slot, SlotId};
use crate::strip::StripParams;

/// A complete sound: slot identifier + macros + strip.
///
/// The Syntakt's "sound pool" is a fixed-size array of these. Load one onto
/// a track at trigger time to get per-trig sound locks: the engine re-loads
/// the slot and applies the macros + strip in `trigger_with_sound`, all at
/// control rate so there's no allocation or coefficient recompute in the
/// audio callback.
#[derive(Clone, Copy)]
#[cfg_attr(feature = "debug-params", derive(Debug))]
pub struct Sound<S, const N: usize>
where
    S: Slot<N>,
{
    /// Which slot to load.
    pub id: S::Id,
    /// Factory macro values for this sound.
    pub macros: [f32; N],
    /// Strip configuration (filter, amp env, drive, pan, level, choke, layer).
    pub strip: StripParams,
}

impl<S, const N: usize> Sound<S, N>
where
    S: Slot<N>,
{
    /// Build a sound for the given slot with its default macros + default
    /// strip. A one-liner starting point for sound design.
    pub fn from_defaults(id: S::Id) -> Self {
        Self {
            macros: S::Id::default_macros(id),
            strip: StripParams::default(),
            id,
        }
    }
}
