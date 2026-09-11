//! Per-track strip: everything in the signal chain except the voice slot.
//!
//! Applied in order: drive → filter → amp-envelope × pan × level. The amp
//! envelope defaults to an instant-on gate (attack 0, hold several seconds,
//! decay trailing the slot's own tail), so by default the *slot's* envelope
//! shapes the audible hit and the strip is just the mixer.

use crate::dsp::SvfMode;
use crate::OutPair;

/// Per-track signal chain — everything but the voice slot itself.
#[derive(Clone, Copy)]
#[cfg_attr(feature = "debug-params", derive(Debug))]
pub struct StripParams {
    /// Multimode filter type.
    pub f_mode: SvfMode,
    /// Filter cutoff, Hz.
    pub f_cutoff_hz: f32,
    /// Filter resonance, Q (0.5..~20; 0.707 is Butterworth).
    pub f_reso_q: f32,
    /// Filter envelope attack, seconds. The track amp envelope is reused as
    /// the filter envelope in Phase 1 — a dedicated filter env is Phase 3.
    pub amp_attack_s: f32,
    /// Filter envelope hold, seconds.
    pub amp_hold_s: f32,
    /// Amp envelope decay, seconds. Track-level gate.
    pub amp_decay_s: f32,
    /// Pre-filter drive gain; 1.0 = no drive, higher saturates.
    pub drive: f32,
    /// Pan, -1 (fully left) .. +1 (fully right).
    pub pan: f32,
    /// Track fader, 0..1.
    pub level: f32,
    /// Send level to the delay bus, 0..1. Post-fader, like a mixer aux.
    pub send_delay: f32,
    /// Send level to the reverb bus, 0..1. Post-fader, like a mixer aux.
    pub send_reverb: f32,
    /// Which output pair this track routes to. Tracks on a non-master pair
    /// do not contribute to the master sum.
    pub out: OutPair,
    /// Mask of track indices that this track *chokes* when triggered. Bit `1
    /// << i` set means triggering this track resets track `i` immediately
    /// (cutting its tail) — the OH-cuts-CH relation.
    pub choke_mask: u8,
    /// Mask of track indices that get *layered* on this track's trigger. Bit
    /// `1 << i` set means track `i` is also triggered with the same velocity.
    pub layer_mask: u8,
}

impl Default for StripParams {
    fn default() -> Self {
        Self {
            f_mode: SvfMode::Off,
            f_cutoff_hz: 1000.0,
            f_reso_q: 0.707,
            amp_attack_s: 0.0,
            amp_hold_s: 10.0,
            amp_decay_s: 10.0,
            drive: 1.0,
            pan: 0.0,
            level: 1.0,
            send_delay: 0.0,
            send_reverb: 0.0,
            out: OutPair::Master,
            choke_mask: 0,
            layer_mask: 0,
        }
    }
}
