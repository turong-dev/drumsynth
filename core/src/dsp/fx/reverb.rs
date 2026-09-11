//! Dattorro-plate-inspired stereo reverb.
//!
//! Owned by [`crate::DrumEngine`] and run at block rate from
//! [`crate::DrumEngine::process`]. The structure is the classic Freeverb /
//! Dattorro hybrid that producers know the sound of:
//!
//! ```text
//!   input → predelay → [diffusor allpasses] ──┬→ 6× L comb-tank → L out
//!                                            └→ 6× R comb-tank → R out
//! ```
//!
//! Each comb is a feedback delay with a one-pole damping lowpass in the loop
//! (Freeverb structure). Tank sizes for L and R are deliberately different so
//! the stereo image spreads rather than two monos cross-talking. There is no
//! cross-feedback between L and R tanks in this v1 — that is the next thing
//! you would add to grow from "good" to "expensive".
//!
//! # Sizing
//!
//! Six combs per side (~16 KB), two input diffusor allpasses (~3 KB), one
//! predelay line (~6 KB) → ~42 KB total, comfortably under the 80 KB reverb
//! slot the plan reserves in case we ever add the cross-coupled tank path.
//!
//! # Determinism
//!
//! No random state. Identical input + parameters produce bit-identical output
//! on host and target. The "thickness" of a real plate cannot come from noise
//! here — it comes from the comb-length incommensurability, which is what
//! gives this design its reputation.

use crate::dsp::filter::cutoff_coeff;
use crate::SAMPLE_RATE;

/// Maximum predelay in samples (~30 ms at 48kHz).
const PREDELAY_MAX: usize = 1_500;

/// Comb filter with damping lowpass in the feedback path.
///
/// The damping one-pole state is owned (not shared between L and R) so the
/// stereo image doesn't smear. The fibreglass-tuned length is a `const`
/// generic so each comb gets its own stack-backed buffer at the right size
/// — no `alloc`, no resizing.
#[derive(Copy)]
struct Comb<const N: usize> {
    buf: [f32; N],
    /// Write cursor; reads lag by `N` samples.
    idx: usize,
    /// Linear feedback gain, 0..~0.99. Clamped on set.
    feedback: f32,
    /// Damping one-pole LP coefficient (`cutoff_coeff`-shaped, i.e. closer
    /// to 1 = darker, closer to 0 = brighter).
    damp: f32,
    /// Internal LP state.
    lp: f32,
}

impl<const N: usize> Comb<N> {
    const fn new() -> Self {
        Self {
            buf: [0.0; N],
            idx: 0,
            feedback: 0.84,
            damp: 0.2,
            lp: 0.0,
        }
    }

    fn reset(&mut self) {
        self.buf.fill(0.0);
        self.idx = 0;
        self.lp = 0.0;
    }

    /// Process one sample through the comb and return the tank output.
    ///
    /// `out` (the oldest sample) is what the read hears; the input becomes
    /// `input + damp(lp) * feedback` written back to the buffer. Matches the
    /// Freeverb track exactly, so a future port to Freeverb is one rename.
    #[inline(always)]
    fn process(&mut self, input: f32) -> f32 {
        let out = self.buf[self.idx];
        // One-pole damping on the recirculating signal.
        self.lp = out + self.damp * (self.lp - out);
        let v = input + self.lp * self.feedback;
        self.buf[self.idx] = v;
        self.idx += 1;
        if self.idx >= N {
            self.idx = 0;
        }
        out
    }
}

impl<const N: usize> Clone for Comb<N> {
    fn clone(&self) -> Self {
        *self
    }
}

/// Schroeder allpass — `y = -x + delay_out`, used as a diffusor.
///
/// Same buffer/idx pattern as [`Comb`] but the output is the delayed signal
/// summed with the negative input; the input back into the line is `input +
/// c * delay_out` so the response is phase-flat at low frequencies and
/// phase-scrambled high up. Produces the diffuse reverberant wash rather
/// than ringing.
#[derive(Copy)]
struct Allpass<const N: usize> {
    buf: [f32; N],
    idx: usize,
    /// Allpass feedback coefficient, 0.5..0.7 typical. Beyond 0.7 the
    /// diffusion gets metallic.
    c: f32,
}

impl<const N: usize> Allpass<N> {
    const fn new() -> Self {
        Self {
            buf: [0.0; N],
            idx: 0,
            c: 0.6,
        }
    }

    fn reset(&mut self) {
        self.buf.fill(0.0);
        self.idx = 0;
    }

    #[inline(always)]
    fn process(&mut self, x: f32) -> f32 {
        let delayed = self.buf[self.idx];
        let y = -x + delayed;
        let v = x + delayed * self.c;
        self.buf[self.idx] = v;
        self.idx += 1;
        if self.idx >= N {
            self.idx = 0;
        }
        y
    }
}

impl<const N: usize> Clone for Allpass<N> {
    fn clone(&self) -> Self {
        *self
    }
}

// Tank sizes — chosen as coprime-ish to spread the modal density across
// frequencies. Tuned by ear (Freeverb's standard set scaled to 48kHz), then
// shrunk to fit the 80 KB reverb-budget slot the plan reserves.
//
// Named individually because `ARRAY[0]` in a `const N` generic position
// is still unstable (adt_const_params); bare `const` scalars work.
const L_C0: usize = 800;
const L_C1: usize = 1000;
const L_C2: usize = 1195;
const L_C3: usize = 1357;
const L_C4: usize = 1527;
const L_C5: usize = 1679;

const R_C0: usize = 840;
const R_C1: usize = 1070;
const R_C2: usize = 1223;
const R_C3: usize = 1393;
const R_C4: usize = 1557;
const R_C5: usize = 1683;

const AP_A: usize = 216;
const AP_B: usize = 532;

/// Stereo Dattorro-plate-ish reverb.
///
/// Six combs per side, two input diffusor allpasses, one predelay line. See
/// the [module docs](self) for topology and sizing.
#[derive(Copy)]
pub struct Reverb {
    // Predelay — a plain ring buffer used as a fixed-length delay.
    predelay: [f32; PREDELAY_MAX],
    predelay_idx: usize,
    predelay_samples: usize,

    // Input diffusors — single allpass shared by both channels (mono sum
    // pre-tank is the convention; tank spreads it back to stereo).
    diffusor_a: Allpass<AP_A>,
    diffusor_b: Allpass<AP_B>,

    // Tanks — six combs per side.
    combs_l: (
        Comb<L_C0>,
        Comb<L_C1>,
        Comb<L_C2>,
        Comb<L_C3>,
        Comb<L_C4>,
        Comb<L_C5>,
    ),
    combs_r: (
        Comb<R_C0>,
        Comb<R_C1>,
        Comb<R_C2>,
        Comb<R_C3>,
        Comb<R_C4>,
        Comb<R_C5>,
    ),

    // Parameters (setup rate).
    /// Tank feedback, ~0.7 (short) .. ~0.92 (long hall).
    feedback: f32,
    /// Damping LP cutoff in Hz — lower means high frequencies die young.
    damp_hz: f32,
    /// Wet/dry mix.
    mix: f32,
}

impl Clone for Reverb {
    fn clone(&self) -> Self {
        *self
    }
}

impl Default for Reverb {
    fn default() -> Self {
        Self::new()
    }
}

impl Reverb {
    /// Default reverb: medium hall (`feedback=0.84`, `damp=4 kHz`, predelay
    /// 22 ms, mix 0.4).
    pub fn new() -> Self {
        let mut r = Self {
            predelay: [0.0; PREDELAY_MAX],
            predelay_idx: 0,
            predelay_samples: Self::predelay_to_samples(0.022),
            diffusor_a: Allpass::new(),
            diffusor_b: Allpass::new(),
            combs_l: (
                Comb::new(),
                Comb::new(),
                Comb::new(),
                Comb::new(),
                Comb::new(),
                Comb::new(),
            ),
            combs_r: (
                Comb::new(),
                Comb::new(),
                Comb::new(),
                Comb::new(),
                Comb::new(),
                Comb::new(),
            ),
            feedback: 0.84,
            damp_hz: 4_000.0,
            mix: 0.40,
        };
        // Push factory coefficients into every tank comb. `new()` left damping
        // at the Freeverb default of 0.2; we want it set from the user-facing
        // `damp_hz` so future `set_params` calls stay consistent.
        r.apply_damp_to_tanks();
        r.apply_feedback_to_tanks(0.84);
        r
    }

    /// Initialize a `Reverb` in place at `dst`, without ever holding the
    /// ~69 KB value as a stack local — see
    /// [`Delay::new_in_place`](super::Delay::new_in_place) for why that
    /// matters on a 16 KB stack. Same technique: memset to all-zero (valid
    /// for every field here — plain `f32`/`usize` throughout, no niches),
    /// then patch in the non-zero defaults through an ordinary `&mut
    /// Reverb`, reusing the same tank helpers `new()` uses.
    ///
    /// # Safety
    ///
    /// `dst` must point to writable, properly-aligned memory for a
    /// `Reverb`, valid for writes of `size_of::<Reverb>()` bytes. The
    /// memory need not be initialized beforehand.
    #[allow(unsafe_code)]
    pub unsafe fn new_in_place(dst: *mut Reverb) {
        core::ptr::write_bytes(dst, 0, 1);

        let r = &mut *dst;
        r.predelay_samples = Self::predelay_to_samples(0.022);
        r.feedback = 0.84;
        r.damp_hz = 4_000.0;
        r.mix = 0.40;
        // Allpass `c` defaults to 0.6, not zero — the zero-fill above left
        // it at 0.0, so it needs an explicit patch (unlike the combs, whose
        // `feedback`/`damp` get overwritten by the two `apply_*` calls
        // below regardless).
        r.diffusor_a.c = 0.6;
        r.diffusor_b.c = 0.6;
        r.apply_damp_to_tanks();
        r.apply_feedback_to_tanks(0.84);
    }

    fn predelay_to_samples(seconds: f32) -> usize {
        let s = seconds.clamp(0.0, PREDELAY_MAX as f32 / SAMPLE_RATE);
        (s * SAMPLE_RATE) as usize
    }

    /// Push the current `damp_hz` into every tank comb's `damp` field.
    fn apply_damp_to_tanks(&mut self) {
        // Convert cutoff_hz into the OnePoleLp coeff shape that the comb uses.
        // A low cutoff → coeff near 1 → duller. Map straight through.
        let coeff = cutoff_coeff(self.damp_hz.max(50.0), SAMPLE_RATE);
        let (c0, c1, c2, c3, c4, c5) = &mut self.combs_l;
        c0.damp = coeff;
        c1.damp = coeff;
        c2.damp = coeff;
        c3.damp = coeff;
        c4.damp = coeff;
        c5.damp = coeff;
        let (c0, c1, c2, c3, c4, c5) = &mut self.combs_r;
        c0.damp = coeff;
        c1.damp = coeff;
        c2.damp = coeff;
        c3.damp = coeff;
        c4.damp = coeff;
        c5.damp = coeff;
    }

    /// Push a feedback value into every tank comb.
    fn apply_feedback_to_tanks(&mut self, fb: f32) {
        let (c0, c1, c2, c3, c4, c5) = &mut self.combs_l;
        c0.feedback = fb;
        c1.feedback = fb;
        c2.feedback = fb;
        c3.feedback = fb;
        c4.feedback = fb;
        c5.feedback = fb;
        let (c0, c1, c2, c3, c4, c5) = &mut self.combs_r;
        c0.feedback = fb;
        c1.feedback = fb;
        c2.feedback = fb;
        c3.feedback = fb;
        c4.feedback = fb;
        c5.feedback = fb;
    }

    /// Configure reverb parameters. Setup rate.
    pub fn set_params(&mut self, predelay_s: f32, feedback: f32, damp_hz: f32, mix: f32) {
        self.predelay_samples = Self::predelay_to_samples(predelay_s);
        let fb = feedback.clamp(0.0, 0.97);
        self.feedback = fb;
        self.damp_hz = damp_hz.clamp(50.0, 20_000.0);
        self.mix = mix.clamp(0.0, 1.0);
        self.apply_damp_to_tanks();
        self.apply_feedback_to_tanks(fb);
    }

    /// Wet/dry mix, 0..1.
    pub fn set_mix(&mut self, mix: f32) {
        self.mix = mix.clamp(0.0, 1.0);
    }

    /// Current wet/dry mix.
    pub fn mix(&self) -> f32 {
        self.mix
    }

    /// Flush all tank and predelay state.
    pub fn reset(&mut self) {
        self.predelay.fill(0.0);
        self.predelay_idx = 0;
        self.diffusor_a.reset();
        self.diffusor_b.reset();
        let (c0, c1, c2, c3, c4, c5) = &mut self.combs_l;
        c0.reset();
        c1.reset();
        c2.reset();
        c3.reset();
        c4.reset();
        c5.reset();
        let (c0, c1, c2, c3, c4, c5) = &mut self.combs_r;
        c0.reset();
        c1.reset();
        c2.reset();
        c3.reset();
        c4.reset();
        c5.reset();
    }

    /// Process one block of stereo send input and accumulate wet output into
    /// `out_l`/`out_r`. The two inputs are summed to mono pre-predelay (the
    /// tank spreads it back to stereo by running on the *same* mono signal
    /// with different comb sizes for L and R, which is what gives the stereo
    /// image even with no cross-feedback).
    pub fn process_block(
        &mut self,
        in_l: &[f32],
        in_r: &[f32],
        out_l: &mut [f32],
        out_r: &mut [f32],
    ) {
        let n = in_l.len().min(in_r.len()).min(out_l.len()).min(out_r.len());
        let pd_samples = self.predelay_samples.min(PREDELAY_MAX) % PREDELAY_MAX;
        // Always-non-negative modular subtract — `wrapping_sub` + `%` is
        // wrong (the intermediate isn't a true modular negative).
        let mut read = (self.predelay_idx + PREDELAY_MAX - pd_samples) % PREDELAY_MAX;
        let mix = self.mix;

        for i in 0..n {
            // Mono-sum the send.
            let mono = (in_l[i] + in_r[i]) * 0.5;

            // Predelay ring buffer write-through. Bypass the read when the
            // delay is zero — the ring buffer with a zero-tap read lags by
            // `PREDELAY_MAX` samples because the value at the current index
            // is the one written `PREDELAY_MAX` samples ago, not the just-
            // written value. Special-casing keeps the off-by-one clean.
            self.predelay[self.predelay_idx] = mono;
            self.predelay_idx += 1;
            if self.predelay_idx >= PREDELAY_MAX {
                self.predelay_idx = 0;
            }

            let pd_out = if pd_samples == 0 {
                mono
            } else {
                let v = self.predelay[read];
                read += 1;
                if read >= PREDELAY_MAX {
                    read = 0;
                }
                v
            };

            // Diffusors — series allpass链.
            let d1 = self.diffusor_a.process(pd_out);
            let d2 = self.diffusor_b.process(d1);

            // Tank — run each of the 6 L combs and 6 R combs on the diffused
            // signal, summing their outputs. Six tapped combs recirculate
            // slowly; the 1/6 scale keeps the tank bounded at high feedback
            // (six parallel combs out of phase sum to ~6× the input, so the
            // per-comb input is divided by 6 to keep the total energy in),
            // and the 0.3 wet gain keeps the output unity-ish at unity send.
            let tank_in = d2 * (1.0 / 6.0);
            let (c0, c1, c2, c3, c4, c5) = &mut self.combs_l;
            let mut l_sum = c0.process(tank_in);
            l_sum += c1.process(tank_in);
            l_sum += c2.process(tank_in);
            l_sum += c3.process(tank_in);
            l_sum += c4.process(tank_in);
            l_sum += c5.process(tank_in);

            let (c0, c1, c2, c3, c4, c5) = &mut self.combs_r;
            let mut r_sum = c0.process(tank_in);
            r_sum += c1.process(tank_in);
            r_sum += c2.process(tank_in);
            r_sum += c3.process(tank_in);
            r_sum += c4.process(tank_in);
            r_sum += c5.process(tank_in);

            out_l[i] += l_sum * mix * 0.3;
            out_r[i] += r_sum * mix * 0.3;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::BLOCK;

    #[test]
    fn impulse_produces_decaying_tail() {
        let mut r = Reverb::new();
        // Lower feedback so the tank rings down within the 200-block test
        // window. With fb=0.4 each comb has a ~1.6-sample decay time
        // constant of `1/(1-fb) ≈ 1.67` cycles, with N≈1,250 samples per
        // cycle → per-comb ring to ~50 dB in 200 blocks.
        r.set_params(0.0, 0.4, 4_000.0, 1.0);
        r.reset();

        let mut in_l = [0.0f32; BLOCK];
        let mut in_r = [0.0f32; BLOCK];
        let mut out_l = [0.0f32; BLOCK];
        let mut out_r = [0.0f32; BLOCK];
        in_l[0] = 1.0;
        in_r[0] = 1.0;

        let mut peaks: [f32; 200] = [0.0; 200];
        for (b, slot) in peaks.iter_mut().enumerate() {
            if b > 0 {
                in_l.fill(0.0);
                in_r.fill(0.0);
            }
            out_l.fill(0.0);
            out_r.fill(0.0);
            r.process_block(&in_l, &in_r, &mut out_l, &mut out_r);
            let mut peak = 0.0f32;
            for &s in out_l.iter().chain(out_r.iter()) {
                peak = peak.max(s.abs());
            }
            *slot = peak;
        }

        let max_peak = peaks.iter().copied().fold(0.0f32, f32::max);
        assert!(max_peak > 1e-3, "reverb produced no output: {max_peak}");

        // Tail must decay to substantially quieter than the peak by the end.
        let late = peaks[150..].iter().copied().fold(0.0f32, f32::max);
        assert!(
            late < max_peak * 0.1,
            "reverb didn't decay enough: late={late}, max={max_peak}"
        );
    }

    #[test]
    fn stereo_from_mono_input() {
        let mut r = Reverb::new();
        // Zero predelay so a single block of input immediately reaches the
        // tank. Non-zero predelay would push the response past the block
        // window used here.
        r.set_params(0.0, 0.86, 6_000.0, 1.0);
        r.reset();

        let mut in_l = [0.0f32; BLOCK];
        let in_r = [0.0f32; BLOCK];
        let mut out_l = [0.0f32; BLOCK];
        let mut out_r = [0.0f32; BLOCK];
        for slot in in_l.iter_mut().take(BLOCK.min(8)) {
            *slot = 0.1; // short burst
        }
        r.process_block(&in_l, &in_r, &mut out_l, &mut out_r);
        in_l.fill(0.0);

        // Ring out across enough blocks that all combs (including the
        // smallest, N=800 samples ≈ 25 blocks) have filled and started
        // contributing to the L and R buses.
        let mut l_any = 0.0f32;
        let mut r_any = 0.0f32;
        for _ in 0..80 {
            out_l.fill(0.0);
            out_r.fill(0.0);
            r.process_block(&in_l, &in_r, &mut out_l, &mut out_r);
            for &s in out_l.iter() {
                l_any = l_any.max(s.abs());
            }
            for &s in out_r.iter() {
                r_any = r_any.max(s.abs());
            }
        }

        assert!(l_any > 0.0, "L tank silent");
        assert!(r_any > 0.0, "R tank silent — stereo image broken");
    }

    #[test]
    fn no_runaway_for_sustained_input() {
        let mut r = Reverb::new();
        r.set_params(0.005, 0.97, 4_000.0, 1.0); // max feedback
        r.reset();

        let in_l = [0.5f32; BLOCK];
        let in_r = [0.5f32; BLOCK];
        let mut out_l = [0.0f32; BLOCK];
        let mut out_r = [0.0f32; BLOCK];

        for _ in 0..1_000 {
            out_l.fill(0.0);
            out_r.fill(0.0);
            r.process_block(&in_l, &in_r, &mut out_l, &mut out_r);
            for &s in out_l.iter().chain(out_r.iter()) {
                assert!(s.is_finite(), "reverb ran away: {s}");
                assert!(s.abs() < 10.0, "reverb runaway: {s}");
            }
        }
    }

    #[test]
    fn no_nans_at_extreme_params() {
        let mut r = Reverb::new();
        r.set_params(0.0, 0.0, 50.0, 1.0);
        r.reset();
        let in_l = [1.0f32; BLOCK];
        let in_r = [1.0f32; BLOCK];
        let mut out_l = [0.0f32; BLOCK];
        let mut out_r = [0.0f32; BLOCK];
        for _ in 0..200 {
            out_l.fill(0.0);
            out_r.fill(0.0);
            r.process_block(&in_l, &in_r, &mut out_l, &mut out_r);
            for &s in out_l.iter().chain(out_r.iter()) {
                assert!(s.is_finite(), "reverb NaN at extremes: {s}");
            }
        }
    }
}
