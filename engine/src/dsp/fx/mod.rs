//! Send-FX bus: stereo delay and Dattorro-plate-ish reverb.
//!
//! Owned by [`crate::DrumEngine`] as a single [`SendFx`] struct and run at
//! block rate from [`crate::DrumEngine::process`], after the dry sample loop
//! and before master-clip. Per-track send levels live on
//! [`crate::StripParams`]; the engine accumulates sends into per-block buses
//! and feeds them here.
//!
//! Why both FX are *send-only* at this stage:
//!
//! - a delay as a send can be tapped, sweep'd, and A/B'd by muting the send
//!   on one track — exactly the workflow Syntakt gives
//! - a reverb on the master would be global — staying as a shared send
//!   avoids the memory cost of an insert per track, and parallel-sends is
//!   the conventional mixer topology anyway
//!
//! See [`delay`] for delays and [`reverb`] for the plate.

pub mod delay;
pub mod reverb;

pub use delay::Delay;
pub use reverb::Reverb;

/// Container for all send effects, owned by [`crate::DrumEngine`].
///
/// The two wet buses (delay bus + reverb bus) feed the master bus after the
/// dry sum and before master clip — see
/// [`DrumEngine::process`](crate::DrumEngine::process) for the wiring.
#[derive(Copy)]
pub struct SendFx {
    /// Stereo delay line.
    pub delay: Delay,
    /// Dattorro-plate-ish reverb.
    pub reverb: Reverb,
    /// FX-bus overdrive — drives the wet sum (delay + reverb tail combined)
    /// through `soft_clip` before it is added to the master. 1.0 = unity
    /// (transparent), higher saturates the wet bus.
    pub drive: f32,
}

impl Clone for SendFx {
    fn clone(&self) -> Self {
        *self
    }
}

impl Default for SendFx {
    fn default() -> Self {
        Self::new()
    }
}

impl SendFx {
    /// Default FX: delay 333 ms / 0.40 fb / 0.35 mix, reverb 22 ms predelay /
    /// 0.84 feedback / 4 kHz damp / 0.40 mix, FX-drive = 1.0.
    pub fn new() -> Self {
        Self {
            delay: Delay::new(),
            reverb: Reverb::new(),
            drive: 1.0,
        }
    }

    /// Initialize a `SendFx` in place at `dst` — see
    /// [`Delay::new_in_place`] and [`Reverb::new_in_place`], which this
    /// delegates to. `SendFx` is ~256 KB (`Delay` ~188 KB + `Reverb` ~69
    /// KB); `SendFx::new()` would build that as one stack local before the
    /// move out, which is the thing this function exists to avoid.
    ///
    /// # Safety
    ///
    /// `dst` must point to writable, properly-aligned memory for a
    /// `SendFx`, valid for writes of `size_of::<SendFx>()` bytes. The
    /// memory need not be initialized beforehand.
    #[allow(unsafe_code)]
    pub unsafe fn new_in_place(dst: *mut SendFx) {
        Delay::new_in_place(core::ptr::addr_of_mut!((*dst).delay));
        Reverb::new_in_place(core::ptr::addr_of_mut!((*dst).reverb));
        core::ptr::addr_of_mut!((*dst).drive).write(1.0);
    }

    /// Flush all FX state — call on engine `panic()`.
    pub fn reset(&mut self) {
        self.delay.reset();
        self.reverb.reset();
    }
}
