//! Measurement for the tuning workflow (Phase 10).
//!
//! The engine is deterministic — same machine + macros renders the same
//! samples on any host — so a measurement taken here is a stable number you
//! can compare across edits, not a spot reading. That is the entire point:
//! the trim pass and the monotonicity checks below all reduce a rendered hit
//! to a handful of numbers.
//!
//! Two loudness measures:
//!
//! - **RMS (dBFS)**: the integrated energy of the whole hit. Cheap, exact,
//!   and the right thing to trim drum macros against — a macro's "travel" is
//!   mostly about energy and tail, and dBFS is what a trim pass can act on
//!   directly.
//! - **LUFS**: K-weighted loudness (ITU-R BS.1770-4) over the whole hit,
//!   closer to perceived level than plain RMS. Optional — it costs two
//!   biquads per sample — and useful when a macro moves a *spectral* knob
//!   (filter cutoff) where raw energy understates how loud it gets.

use drum_engine::{DrumEngine, BLOCK, SAMPLE_RATE};

/// A one-shot hit reduced to measurable numbers.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct Measurement {
    /// Integrated RMS over the whole hit, linear amplitude 0..1.
    pub rms: f32,
    /// K-weighted loudness in LUFS, valid only when measured with
    /// `want_lufs = true` (otherwise 0.0).
    pub lufs: f32,
    /// Samples from trigger until the signal last exceeded -60 dBFS — the
    /// "length" of the hit. Clamped by the render window.
    pub length_samples: usize,
    /// Samples from trigger until a 10 ms-release peak envelope first fell
    /// to -40 dB below its own peak. Amplitude-independent: this is the
    /// hit's *decay time*, not a level-crossing, so a pitch knob that shifts
    /// overall level does not disturb it.
    pub decay_samples: usize,
}

impl Measurement {
    /// RMS in dBFS, floored at -120 so a silent hit does not log as -inf.
    pub fn rms_db(self) -> f32 {
        let v = self.rms.max(1e-6);
        20.0 * v.log10()
    }

    /// Tail length in milliseconds.
    pub fn length_ms(self) -> f32 {
        self.length_samples as f32 * 1000.0 / SAMPLE_RATE
    }

    /// Envelope decay time in milliseconds (see [`Self::decay_samples`]).
    pub fn decay_ms(self) -> f32 {
        self.decay_samples as f32 * 1000.0 / SAMPLE_RATE
    }
}

/// A rendered hit plus the mono-sum waveform it produced, for waveform-level
/// checks (the dead-zone detector).
pub struct OneShot {
    pub m: Measurement,
    /// Mono-sum of L and R, one sample per frame, up to `wave_cap_seconds`.
    pub mono: Vec<f32>,
}

/// Render one hit on `track` and measure it, stopping when it goes quiet.
///
/// Unlike [`measure_buffer`] this renders *until silence* rather than a fixed
/// window, so a long cymbal tail is captured in full and a short kick does
/// not waste cycles rendering silence. `max_seconds` is a safety valve.
/// `wave_cap_seconds` caps how much waveform is kept — the dead-zone metric
/// only needs the body of the hit.
pub fn measure_oneshot(
    engine: &mut DrumEngine,
    track: usize,
    max_seconds: f32,
    want_lufs: bool,
    wave_cap_seconds: f32,
) -> OneShot {
    engine.trigger(track, 1.0);

    let max_blocks = (max_seconds * SAMPLE_RATE / BLOCK as f32) as usize;
    let wave_cap = (wave_cap_seconds * SAMPLE_RATE) as usize;
    let mut l = [0.0f32; BLOCK];
    let mut r = [0.0f32; BLOCK];
    let mut m = Measurement::default();
    let mut kw = KWeight::new();
    let mut sum_sq = 0.0f64;
    let mut rendered = 0usize;
    let mut env_term = 0u32;
    let mut mono = Vec::new();

    // -60 dBFS in linear.
    const FLOOR: f32 = 0.001;

    // Peak-hold envelope with a ~10 ms release, for the decay measurement.
    // 0.99 per sample: -40 dB in ln(0.01)/ln(0.99) ≈ 458 samples ≈ 9.5 ms.
    let mut env = 0.0f32;
    let mut env_peak = 0.0f32;
    let mut env_dropped = false;

    'outer: for _ in 0..max_blocks {
        engine.process(&mut l, &mut r);
        for i in 0..BLOCK {
            let sl = l[i];
            let sr = r[i];
            let s = (sl + sr) * 0.5;
            sum_sq += ((sl * sl + sr * sr) * 0.5) as f64;
            if sl.abs() >= FLOOR || sr.abs() >= FLOOR {
                m.length_samples = rendered + i + 1;
            }
            if mono.len() < wave_cap {
                mono.push(s);
            }
            if want_lufs {
                kw.push(sl);
                kw.push(sr);
            }

            // Envelope decays through the peak; decay time is when it first
            // falls 40 dB below its own peak. Amplitude-independent.
            let a = sl.abs().max(sr.abs());
            if a > env {
                env = a;
            } else {
                env *= ENV_RELEASE;
            }
            if env > env_peak {
                env_peak = env;
            }
            if !env_dropped && env_peak > 1e-4 && env < env_peak * 0.01 {
                m.decay_samples = rendered + i + 1;
                env_dropped = true;
            }
        }
        rendered += BLOCK;

        // Termination: the peak-hold envelope has settled to -80 dB below the
        // hit's own peak and stayed there for a few blocks. Adapting to the
        // signal's *own* level makes this robust to bursty envelopes — a
        // clap's ~6 ms crunch gaps dip the envelope to only ~6% of peak,
        // nowhere near the -80 dB line, so a burst never looks like the end
        // of the tail. An all-silent hit (env_peak never rises) settles
        // immediately.
        let settled = if env_peak > 1e-6 {
            env < env_peak * ENV_TERM_RATIO
        } else {
            true
        };
        if settled {
            env_term += 1;
            if env_term >= ENV_TERM_BLOCKS {
                break 'outer;
            }
        } else {
            env_term = 0;
        }
    }

    // A hit that never dropped 40 dB before the window ended reports its full
    // length (the best information we have).
    if m.decay_samples == 0 {
        m.decay_samples = m.length_samples;
    }

    let n = (rendered as f64).max(1.0);
    m.rms = ((sum_sq / n) as f32).sqrt();
    if want_lufs {
        m.lufs = kw.lufs();
    }
    OneShot { m, mono }
}

/// Peak-hold envelope release per sample: falls 40 dB in ~10 ms at 48 kHz.
const ENV_RELEASE: f32 = 0.99;
/// Termination threshold: the envelope is "settled" once it falls this far
/// below the hit's own peak. -80 dB is far below audibility, and far below
/// any legitimately-bursty crunch gap (see [`measure_oneshot`]).
const ENV_TERM_RATIO: f32 = 0.0001;
/// Sustained settled blocks before we call the tail done.
const ENV_TERM_BLOCKS: u32 = 4;

/// Measure an already-rendered interleaved stereo buffer.
///
/// Use this when the samples are being written to a WAV anyway (sweep, trim,
/// golden) — it costs nothing extra and measures exactly what gets written.
pub fn measure_buffer(interleaved: &[f32], want_lufs: bool) -> Measurement {
    let mut m = Measurement::default();
    let mut kw = KWeight::new();
    let mut sum_sq = 0.0f64;
    let n = interleaved.len();
    let mut env = 0.0f32;
    let mut env_peak = 0.0f32;
    let mut env_dropped = false;

    for (i, &s) in interleaved.iter().enumerate() {
        sum_sq += (s * s) as f64;
        if want_lufs {
            kw.push(s);
        }
        if s.abs() >= 0.001 {
            m.length_samples = i + 1;
        }

        let a = s.abs();
        if a > env {
            env = a;
        } else {
            env *= ENV_RELEASE;
        }
        if env > env_peak {
            env_peak = env;
        }
        if !env_dropped && env_peak > 1e-4 && env < env_peak * 0.01 {
            m.decay_samples = i + 1;
            env_dropped = true;
        }
    }
    // sum_sq spans all n samples across both channels; the per-channel mean
    // square is sum_sq / n, which is also the mono-sum power.
    m.rms = ((sum_sq / n.max(1) as f64) as f32).sqrt();
    if want_lufs {
        m.lufs = kw.lufs();
    }
    if m.decay_samples == 0 {
        m.decay_samples = m.length_samples;
    }
    m
}

/// ITU-R BS.1770-4 K-weighting, as a pair of cascaded biquads.
///
/// Standard pre-filter for loudness measurement: a +4 dB high-shelf at
/// 1.5 kHz (split into the published two-stage coefficients) followed by a
/// 38 Hz high-pass. The output is the per-sample mean square, integrated
/// over whatever window the caller feeds it.
struct KWeight {
    // Pre-filter state (high shelf, then high pass).
    s1: [f32; 2],
    s2: [f32; 2],
    sum_sq: f64,
    count: f64,
}

impl KWeight {
    fn new() -> Self {
        Self {
            s1: [0.0; 2],
            s2: [0.0; 2],
            sum_sq: 0.0,
            count: 0.0,
        }
    }

    /// Filter one sample and accumulate its square. Call on both channels.
    #[inline]
    fn push(&mut self, x: f32) {
        // Stage 1: +4 dB high-shelf at 1.5 kHz (EBU Tech 3341 coefficients).
        let b0 = 1.535_124_9;
        let b1 = -2.691_696_2;
        let b2 = 1.198_392_8;
        let a1 = -1.690_659_3;
        let a2 = 0.732_480_8;
        let y1 = b0 * x + self.s1[0];
        self.s1[0] = b1 * x - a1 * y1 + self.s1[1];
        self.s1[1] = b2 * x - a2 * y1;

        // Stage 2: 38 Hz high-pass.
        let b0 = 1.0;
        let b1 = -2.0;
        let b2 = 1.0;
        let a1 = -1.990_047_5;
        let a2 = 0.990_072_25;
        let y2 = b0 * y1 + self.s2[0];
        self.s2[0] = b1 * y1 - a1 * y2 + self.s2[1];
        self.s2[1] = b2 * y1 - a2 * y2;

        self.sum_sq += (y2 * y2) as f64;
        self.count += 1.0;
    }

    /// Integrated loudness in LUFS over everything pushed so far.
    fn lufs(&self) -> f32 {
        if self.count == 0.0 {
            return -70.0;
        }
        let mean = self.sum_sq / self.count;
        // The -0.691 dB K-weighting constant from BS.1770.
        (-0.691 + 10.0 * mean.log10()) as f32
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rms_of_sine_is_minus_3db() {
        let mut buf = Vec::new();
        let freq = 1000.0;
        for i in 0..(SAMPLE_RATE as usize) {
            let s = (2.0 * std::f32::consts::PI * freq * i as f32 / SAMPLE_RATE).sin();
            buf.push(s);
            buf.push(s);
        }
        let m = measure_buffer(&buf, true);
        assert!(
            (m.rms_db() - (-3.0103)).abs() < 0.1,
            "full-scale sine RMS should be -3.01 dBFS, got {}",
            m.rms_db()
        );
        assert_eq!(m.length_samples, buf.len());
    }

    #[test]
    fn k_weighting_is_roughly_unity_at_1khz() {
        // A full-scale 1 kHz sine reads ≈ -3.0 dBFS RMS; with K-weighting
        // (roughly 0 dB at 1 kHz) that is ≈ -3.7 LUFS. A wide bound keeps
        // the test robust to the shelf's exact response.
        let mut buf = Vec::new();
        let freq = 1000.0;
        for i in 0..(SAMPLE_RATE as usize) {
            let s = (2.0 * std::f32::consts::PI * freq * i as f32 / SAMPLE_RATE).sin();
            buf.push(s);
            buf.push(s);
        }
        let m = measure_buffer(&buf, true);
        assert!(
            m.lufs > -6.0 && m.lufs < -1.0,
            "1 kHz full-scale sine should read near -3.7 LUFS, got {}",
            m.lufs
        );
    }

    #[test]
    fn silence_measures_as_floor() {
        let m = measure_buffer(&[0.0; 64], false);
        assert_eq!(m.rms_db(), -120.0);
        assert_eq!(m.length_samples, 0);
    }

    #[test]
    fn measure_oneshot_stops_when_quiet() {
        let mut e = DrumEngine::new();
        e.tracks[0].load_machine(drum_engine::machines::MachineId::BdClassic);
        let hit = measure_oneshot(&mut e, 0, 4.0, false, 2.0);
        // A default BD Classic kick: a body of a few hundred ms, nowhere near
        // the 4 s ceiling, and loud.
        assert!(
            hit.m.length_ms() > 50.0 && hit.m.length_ms() < 3_000.0,
            "kick tail in a sane range, got {} ms",
            hit.m.length_ms()
        );
        assert!(hit.m.rms > 0.05, "a kick has energy");
        assert!(hit.m.decay_ms() > 30.0, "decay is meaningful");
        assert!(!hit.mono.is_empty(), "waveform capture works");
    }

    #[test]
    fn decay_is_amplitude_independent() {
        // The decay metric must not move when the hit is simply made quieter:
        // LEVEL scales amplitude, not the envelope shape.
        let mut e = DrumEngine::new();
        e.tracks[0].load_machine(drum_engine::machines::MachineId::BdClassic);
        let full = measure_oneshot(&mut e, 0, 4.0, false, 1.0);
        let mut e2 = DrumEngine::new();
        e2.tracks[0].load_machine(drum_engine::machines::MachineId::BdClassic);
        e2.tracks[0].set_macro(drum_engine::machines::SLOT_LEVEL, 0.3);
        let quiet = measure_oneshot(&mut e2, 0, 4.0, false, 1.0);
        let spread = (full.m.decay_ms() - quiet.m.decay_ms()).abs();
        assert!(
            spread < 20.0,
            "decay must ignore level: full {} ms vs quiet {} ms",
            full.m.decay_ms(),
            quiet.m.decay_ms()
        );
    }
}
