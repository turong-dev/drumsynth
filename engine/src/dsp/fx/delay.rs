//! Static-buffer stereo delay, no tempo sync (v1).
//!
//! Owned by [`crate::DrumEngine`] and run at block rate from
//! [`crate::DrumEngine::process`]. A single circular delay line per channel
//! with feedback, a one-pole lowpass in the feedback path (the "tone" knob —
//! dulls repeats so they recede rather than pile up), and a wet/dry mix.
//!
//! # Sizing
//!
//! Half a second at 48kHz = 24,000 samples per channel → 192 KB total. Lives
//! in [`DrumEngine`] on the Teensy's 1MB; nothing on the hot path but the
//! per-sample read/write/feedback multiply-add.
//!
//! # Determinism
//!
//! There is no random state anywhere in here — the same input + parameter set
//! produces a bit-identical output on host and target, which is the contract
//! the rest of the engine lives by.

use crate::dsp::filter::{cutoff_coeff, OnePoleLp};
use crate::SAMPLE_RATE;

/// Maximum delay time, in samples. Half a second at 48kHz.
pub const MAX_DELAY_SAMPLES: usize = 24_000;

/// Maximum delay time, in seconds — exposed so callers can clamp user input.
pub const MAX_DELAY_S: f32 = MAX_DELAY_SAMPLES as f32 / SAMPLE_RATE;

/// Static-buffer stereo delay.
///
/// All parameter changes go through [`set_params`](Self::set_params); the
/// per-sample path reads only precomputed coefficients and indices.
#[derive(Copy)]
pub struct Delay {
    buf_l: [f32; MAX_DELAY_SAMPLES],
    buf_r: [f32; MAX_DELAY_SAMPLES],
    /// Write cursor; the read cursor is `write - delay_samples` (mod N).
    write_idx: usize,
    /// Per-channel integer delay in samples. Stored as `usize` because the
    /// read index computation is a plain subtract; fractional delay would
    /// need a lerp between two reads — added later if a continuous time
    /// knob is ever worth the cycles.
    delay_samples: usize,
    /// Linear feedback gain, 0..0.98. Clamped on set so even a runaway
    /// macro cannot make the line self-oscillate hard enough to blow up.
    fb: f32,
    /// Wet/dry mix, 0 (dry only) .. 1 (wet only). Linear crossfade.
    mix: f32,
    /// One-pole lowpass in the feedback path for the left channel — high
    /// frequencies die faster on each repeat, which is what stops a delay
    /// from piling up into a wall of clicks.
    tone_l: OnePoleLp,
    /// Same filter for the right channel; the per-channel `z` state is what
    /// keeps the L/R modulation phase from coupling through the feedback
    /// path (a shared filter would smear the stereo image on every repeat).
    tone_r: OnePoleLp,
}

impl Clone for Delay {
    fn clone(&self) -> Self {
        *self
    }
}

impl Default for Delay {
    fn default() -> Self {
        Self::new()
    }
}

impl Delay {
    /// Default delay: 333 ms, 0.40 feedback, ~3.3 kHz tone, mix 0.35.
    pub fn new() -> Self {
        let tone_coeff = cutoff_coeff(3_300.0, SAMPLE_RATE);
        Self {
            buf_l: [0.0; MAX_DELAY_SAMPLES],
            buf_r: [0.0; MAX_DELAY_SAMPLES],
            write_idx: 0,
            delay_samples: Self::time_to_samples(0.333),
            fb: 0.40,
            mix: 0.35,
            tone_l: OnePoleLp::new(tone_coeff),
            tone_r: OnePoleLp::new(tone_coeff),
        }
    }

    /// Initialize a `Delay` in place at `dst`, without ever holding the
    /// ~192 KB value (`2 × MAX_DELAY_SAMPLES` `f32`s) as a stack local.
    ///
    /// `Delay::new()` builds its return value as an ordinary local before
    /// moving it out; whether that move becomes a direct in-place write or
    /// a temporary-plus-copy is up to the optimizer, and on a 16 KB stack
    /// only the former is survivable. This function is unconditionally
    /// correct: it memsets the buffers straight into `dst` (never
    /// constructing them as a value anywhere) and then patches in the
    /// handful of non-zero scalar defaults.
    ///
    /// # Safety
    ///
    /// `dst` must point to writable, properly-aligned memory for a `Delay`,
    /// valid for writes of `size_of::<Delay>()` bytes. The memory need not
    /// be initialized beforehand.
    #[allow(unsafe_code)]
    pub unsafe fn new_in_place(dst: *mut Delay) {
        // All-zero is a valid bit pattern for every field here (f32s,
        // usizes, and `OnePoleLp`'s own f32s) — this is the same value
        // `buf_l: [0.0; N], buf_r: [0.0; N], write_idx: 0, ...` would be,
        // just written directly instead of built up and moved.
        core::ptr::write_bytes(dst, 0, 1);

        // Patch in the defaults that aren't zero. Safe to go through a
        // `&mut Delay` from here on — the zero-fill above already makes
        // every field a valid value of its type.
        let d = &mut *dst;
        let tone_coeff = cutoff_coeff(3_300.0, SAMPLE_RATE);
        d.delay_samples = Self::time_to_samples(0.333);
        d.fb = 0.40;
        d.mix = 0.35;
        d.tone_l = OnePoleLp::new(tone_coeff);
        d.tone_r = OnePoleLp::new(tone_coeff);
    }

    fn time_to_samples(time_s: f32) -> usize {
        let t = time_s.clamp(0.0, MAX_DELAY_S);
        (t * SAMPLE_RATE) as usize
    }

    /// Configure all delay parameters. Setup rate.
    ///
    /// `time_s` is clamped to [`MAX_DELAY_S`]. `feedback` is clamped to
    /// `0.98` so a sustained input cannot grow without bound. `tone_hz` is
    /// the feedback one-pole cutoff, so a higher value passes more high
    /// frequencies through the feedback path (brighter repeats).
    pub fn set_params(&mut self, time_s: f32, feedback: f32, tone_hz: f32, mix: f32) {
        self.delay_samples = Self::time_to_samples(time_s);
        self.fb = feedback.clamp(0.0, 0.98);
        self.mix = mix.clamp(0.0, 1.0);
        self.tone_l
            .set_coeff(cutoff_coeff(tone_hz.max(50.0), SAMPLE_RATE));
        self.tone_r
            .set_coeff(cutoff_coeff(tone_hz.max(50.0), SAMPLE_RATE));
    }

    /// Wet/dry mix, 0..1. Separate setter so LFOs can sweep the wetness
    /// alone (sweeping time has clicks without a smoothing crossfade).
    pub fn set_mix(&mut self, mix: f32) {
        self.mix = mix.clamp(0.0, 1.0);
    }

    /// Current delay time in seconds.
    pub fn time_s(&self) -> f32 {
        self.delay_samples as f32 / SAMPLE_RATE
    }

    /// Current feedback.
    pub fn feedback(&self) -> f32 {
        self.fb
    }

    /// Current wet/dry mix.
    pub fn mix(&self) -> f32 {
        self.mix
    }

    /// Flush the delay line. Call on panic / kit reload.
    pub fn reset(&mut self) {
        self.buf_l.fill(0.0);
        self.buf_r.fill(0.0);
        self.write_idx = 0;
        self.tone_l.reset();
        self.tone_r.reset();
    }

    /// Process one block of stereo send. Inputs are the *send* bus (post
    /// fader, post-strip); outputs are the *wet* bus, which the caller
    /// crossfades against the dry bus with [`mix`](Self::mix).
    ///
    /// `in_l`/`in_r` may be silence (no sends active); the delay line
    /// continues to ring out its own tail into `out_l`/`out_r`.
    pub fn process_block(
        &mut self,
        in_l: &[f32],
        in_r: &[f32],
        out_l: &mut [f32],
        out_r: &mut [f32],
    ) {
        let n = in_l.len().min(in_r.len()).min(out_l.len()).min(out_r.len());
        let n_buf = MAX_DELAY_SAMPLES;
        let d = self.delay_samples;
        let fb = self.fb;
        let mix = self.mix;
        // mix^2 wart: use a linear crossfade (wet * mix + dry * (1-mix)).
        // The delay here is *send-only*, so the dry signal isn't passed in
        // — the caller mixes the wet bus against the master dry bus.
        // What `out_*` carries is therefore pure wet, scaled by mix.
        let wet = mix;

        let mut write = self.write_idx;
        // Read cursor: `write - delay`, wrapping forward into the buffer.
        // `wrapping_sub` + `%` does NOT yield `(write - d) mod N` for usize
        // because the intermediate value isn't a true modular negative —
        // instead use the always-non-negative `write + N - (d mod N)`, then
        // take `mod N` to fold it back. `d` is bounded to `N`, so the inner
        // `mod N` is a no-op except at exactly `d == N`.
        let d_eff = d.min(n_buf) % n_buf;
        let mut read = (write + n_buf - d_eff) % n_buf;

        for i in 0..n {
            let dl = self.buf_l[read];
            let dr = self.buf_r[read];

            // Tone: dampen the *delayed* signal before feedback so each
            // repeat is duller than the last. Two independent one-poles — one
            // per channel — keep the stereo image from coupling through a
            // shared `z` state.
            let tl = self.tone_l.tick(dl);
            let tr = self.tone_r.tick(dr);

            out_l[i] += tl * wet;
            out_r[i] += tr * wet;

            // Feedback: input send + delayed/damped → back into the line.
            let inl_in = in_l[i] + tl * fb;
            let inr_in = in_r[i] + tr * fb;
            self.buf_l[write] = inl_in;
            self.buf_r[write] = inr_in;

            // Advance cursors with a manual wrap (the wrapping-add form
            // `(... + 1) % N` compiles to a multiply that costs more than a
            // branch-predicted compare; keep with conditional add).
            write += 1;
            if write >= n_buf {
                write = 0;
            }
            read += 1;
            if read >= n_buf {
                read = 0;
            }
        }
        self.write_idx = write;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::BLOCK;

    #[test]
    fn delay_roundtrips_an_impulse() {
        let mut d = Delay::new();
        d.set_params(0.005, 0.0, 20_000.0, 1.0); // 5 ms, no feedback, all wet
        d.reset();

        let mut in_l = [0.0f32; BLOCK];
        let mut in_r = [0.0f32; BLOCK];
        let mut out_l = [0.0f32; BLOCK];
        let mut out_r = [0.0f32; BLOCK];
        in_l[0] = 1.0;
        in_r[0] = 0.5;

        // 5 ms @ 48k = 240 samples = 7.5 blocks. Process enough blocks that
        // the impulse has come back out.
        let blocks = (Delay::time_to_samples(0.005) / BLOCK) + 2;
        let mut peak_l = 0.0f32;
        let mut peak_r = 0.0f32;
        for b in 0..blocks {
            out_l.fill(0.0);
            out_r.fill(0.0);
            // After the first block the input is silent — only the delayed
            // signal lingers.
            if b > 0 {
                in_l.fill(0.0);
                in_r.fill(0.0);
            }
            d.process_block(&in_l, &in_r, &mut out_l, &mut out_r);
            for &s in out_l.iter() {
                peak_l = peak_l.max(s.abs());
            }
            for &s in out_r.iter() {
                peak_r = peak_r.max(s.abs());
            }
        }

        // The impulse should come back at ~unity on L (tone is open at 20k,
        // feedback is 0) and ~0.5 on R.
        assert!(peak_l > 0.9, "delay didn't pass L through: {peak_l}");
        assert!(peak_r > 0.4 && peak_r < 0.6, "delay R not ~0.5: {peak_r}");
    }

    #[test]
    fn feedback_decays_to_silence() {
        let mut d = Delay::new();
        d.set_params(0.020, 0.5, 20_000.0, 1.0); // 20 ms, fb=0.5
        d.reset();

        let mut in_l = [0.0f32; BLOCK];
        in_l[0] = 1.0;
        let in_r = [0.0f32; BLOCK];
        let mut out_l = [0.0f32; BLOCK];
        let mut out_r = [0.0f32; BLOCK];

        // 64-block run: capture the peak per block. The impulse round trips
        // every `delay_samples` = 960 samples ≈ 30 blocks. So peaks land at
        // roughly block 30 and block 60; comparing the two quantifies the
        // feedback decay.
        let mut peaks: [f32; 64] = [0.0; 64];
        for (b, slot) in peaks.iter_mut().enumerate() {
            if b > 0 {
                in_l.fill(0.0);
            }
            out_l.fill(0.0);
            out_r.fill(0.0);
            d.process_block(&in_l, &in_r, &mut out_l, &mut out_r);
            let mut peak = 0.0f32;
            for &s in out_l.iter() {
                peak = peak.max(s.abs());
            }
            *slot = peak;
        }

        let first_trip = peaks[28..35].iter().copied().fold(0.0f32, f32::max);
        let _second_trip = peaks[58..64].iter().copied().fold(0.0f32, f32::max);

        // First round-trip must be non-zero — else feedback is broken.
        assert!(
            first_trip > 0.05,
            "delay produced no detectable feedback: first_trip={first_trip}"
        );
        // After the first round trip, the tone LP + feedback together must
        // take the energy down. The exact number isn't the assertion — just
        // that it monotonically recedes.
        let all_remaining = peaks[40..].iter().copied().fold(0.0f32, f32::max);
        assert!(
            all_remaining < first_trip,
            "decay stalled: max after block 40 ({all_remaining}) > first round trip ({first_trip})"
        );
    }

    #[test]
    fn tone_attenuates_high_frequencies_in_repeats() {
        let mut bright = Delay::new();
        bright.set_params(0.005, 0.9, 20_000.0, 1.0);
        bright.reset();

        let mut dull = Delay::new();
        dull.set_params(0.005, 0.9, 500.0, 1.0);
        dull.reset();

        // Drive a 5 kHz burst for several blocks, then let it ring out.
        let freq = 5_000.0f32;
        let mut phase = 0.0f32;
        let inc = freq / SAMPLE_RATE;
        let mut in_l = [0.0f32; BLOCK];
        let in_r = [0.0f32; BLOCK];
        let mut bl = [0.0f32; BLOCK];
        let mut br = [0.0f32; BLOCK];

        for slot in in_l.iter_mut() {
            *slot = libm::sinf(phase * core::f32::consts::TAU) * 0.5;
            phase += inc;
        }

        // 20 blocks of drive.
        for _ in 0..20 {
            bl.fill(0.0);
            br.fill(0.0);
            bright.process_block(&in_l, &in_r, &mut bl, &mut br);
            dull.process_block(&in_l, &in_r, &mut bl, &mut br);
        }

        // Then 50 blocks of passive ring under each, separately.
        let mut bright_peak = 0.0f32;
        let mut dull_peak = 0.0f32;
        in_l.fill(0.0);
        for _ in 0..50 {
            bl.fill(0.0);
            br.fill(0.0);
            bright.process_block(&in_l, &in_r, &mut bl, &mut br);
            for &s in bl.iter() {
                bright_peak = bright_peak.max(s.abs());
            }
        }
        for _ in 0..50 {
            bl.fill(0.0);
            br.fill(0.0);
            dull.process_block(&in_l, &in_r, &mut bl, &mut br);
            for &s in bl.iter() {
                dull_peak = dull_peak.max(s.abs());
            }
        }

        // The dull version's tail should be quieter for high-frequency content
        // because the tone LP is killing the very thing that's recirculating.
        assert!(
            dull_peak < bright_peak * 0.9,
            "tone didn't attenuate highs: dull={dull_peak}, bright={bright_peak}"
        );
    }

    #[test]
    fn no_nans_at_extreme_params() {
        let mut d = Delay::new();
        // Extreme but legal parameters.
        d.set_params(MAX_DELAY_S, 0.98, 50.0, 1.0);
        d.reset();

        let mut in_l = [0.0f32; BLOCK];
        in_l[0] = 1.0;
        let in_r = [0.0f32; BLOCK];
        let mut out_l = [0.0f32; BLOCK];
        let mut out_r = [0.0f32; BLOCK];

        for _ in 0..200 {
            out_l.fill(0.0);
            out_r.fill(0.0);
            d.process_block(&in_l, &in_r, &mut out_l, &mut out_r);
            for &s in out_l.iter().chain(out_r.iter()) {
                assert!(s.is_finite(), "delay NaN at extreme params: {s}");
            }
            in_l.fill(0.0);
        }
    }
}
