//! Isolated audition signal for the ADAA waveshaper stage.
//!
//! This is deliberately *not* in the engine path: it generates a steady probe
//! tone on the host, runs it through a single [`Shaper`], and writes the
//! result. It therefore uses `std::f32::sin` for the probe oscillator — the
//! engine's `fast::sin_turns` constraint applies to the per-sample firmware
//! audio path, not to host-side test signals.

use drum_engine::dsp::Shaper;
use drum_engine::{BLOCK, SAMPLE_RATE};
use std::f32::consts::TAU;

/// Probe tone: low enough that the transfer curve is most of what you hear,
/// high enough that a cell is dozens of steady-state cycles.
const CARRIER_HZ: f32 = 55.0;

/// 3:2 against [`CARRIER_HZ`], so ring positions land on musical tones: the
/// sum and difference are 137.5 and 27.5 Hz rather than an arbitrary beat.
const MOD_HZ: f32 = 82.5;

/// The shipped `WARP` default: [`render_shaper_sine`] sweeps two knobs, not
/// three.
const TIMBRE: f32 = 0.5;

/// Input amplitude. Below the clipper's knee at unity gain, deep inside it at
/// full drive — and the reason no normalisation is needed, since every
/// algorithm's output stays bounded by the saturators in front of it.
const AMP: f32 = 0.5;

/// Length of one grid cell: ~66 cycles of the carrier, so a cell is clearly at
/// steady state before it ends.
const CELL_S: f32 = 1.2;

/// Frames a cell for flipping between it and its neighbours, and resets the
/// ADAA states to a common starting point (see [`SineProbe::gap`]).
const GAP_S: f32 = 0.2;

/// One continuous pass of a knob, slow enough to follow by ear.
const SWEEP_S: f32 = 8.0;

/// Total file length estimate used for `Vec::with_capacity`.
///
/// 16 cells × (cell + gap) + two sweeps ≈ 38.8 s, so 40 s leaves a small
/// margin without over-allocating.
const MAX_FILE_S: f32 = 40.0;

/// The strip's remap of the drive macro onto `Shaper::set_parameters`' own
/// `drive` argument — `warps_drive_from_macro`, minus its bypass detent.
///
/// `set_parameters` turns that into `1 + 7·m` of input gain, and the raw
/// argument's bottom half is flat: everything at or below `drive = 0.5` is
/// unity gain, because the formula is `((drive - 0.5) * 2).clamp(0, 1)`.
/// Sweeping the raw argument from 0 would therefore spend half the file on a
/// constant, which is why everything below is indexed by macro position and
/// remapped here.
fn drive_from_macro_position(m: f32) -> f32 {
    0.5 + 0.5 * m
}

/// The probe signal for [`render_shaper_sine`]: a fixed sine pair driven
/// through a [`Shaper`], with every cell opening on the same waveform.
///
/// [`Self::gap`] returns both phases to zero between cells, so the tone
/// resumes on a zero crossing and the only thing that differs between two
/// cells is the knob position.
struct SineProbe {
    shaper: Shaper,
    /// In turns, kept inside `[0, 1)` by subtraction rather than left to
    /// accumulate. [`Self::gap`] also returns it to zero between sections, so
    /// the longest run is a single ~8 s sweep; without either of those, a
    /// ~40 s file would carry ~2,200 turns, where one `f32` ulp is a fifth of
    /// the per-sample increment and the tone jitters audibly.
    carrier_phase: f32,
    mod_phase: f32,
    carrier: [f32; BLOCK],
    modulator: [f32; BLOCK],
    aux: [f32; BLOCK],
}

impl SineProbe {
    fn new() -> Self {
        Self {
            shaper: Shaper::new(SAMPLE_RATE),
            carrier_phase: 0.0,
            mod_phase: 0.0,
            carrier: [0.0; BLOCK],
            modulator: [0.0; BLOCK],
            aux: [0.0; BLOCK],
        }
    }

    /// Fill one block of tone, advancing both phases.
    ///
    /// Returns whether the carrier phase wrapped during the block — the
    /// sample where the tone crosses zero. `cut` stops the tone there and
    /// leaves the rest of the block silent, which is how a tone section ends
    /// on the same crossing that [`Self::gap`] resumes on.
    fn fill(&mut self, cut: bool) -> bool {
        let mut c = self.carrier_phase;
        let mut m = self.mod_phase;
        let dc = CARRIER_HZ / SAMPLE_RATE;
        let dm = MOD_HZ / SAMPLE_RATE;
        let mut wrapped = false;
        for i in 0..BLOCK {
            self.carrier[i] = AMP * (c * TAU).sin();
            self.modulator[i] = AMP * (m * TAU).sin();
            c += dc;
            if c >= 1.0 {
                c -= 1.0;
                wrapped = true;
                if cut {
                    // The sine is within one step of zero at the wrap, so
                    // the input runs into the gap without stepping.
                    for v in self.carrier.iter_mut().skip(i + 1) {
                        *v = 0.0;
                    }
                    for v in self.modulator.iter_mut().skip(i + 1) {
                        *v = 0.0;
                    }
                    break;
                }
            }
            m += dm;
            if m >= 1.0 {
                m -= 1.0;
            }
        }
        self.carrier_phase = c;
        self.mod_phase = m;
        wrapped
    }

    /// One block of tone through the stage, appended to `out` as interleaved
    /// stereo. The stage is mono, so both channels carry the same signal.
    fn tone(&mut self, out: &mut Vec<f32>) {
        self.fill(false);
        self.run(out);
    }

    /// `blocks` of tone, then run on to the carrier's next zero crossing.
    ///
    /// The extension is what lets a section *end* cleanly: a tone that stops
    /// at an arbitrary phase steps its input to zero, and the stage answers a
    /// one-sample step with a one-sample click — sixteen of them, one per
    /// cell, in a file whose whole job is to let you hear whether the stage
    /// clicks. Reaching the crossing by rendering up to it is bounded by one
    /// carrier period (~18 ms); the alternative, waiting for the phase to land
    /// *on* zero inside a tolerance, is unbounded, because the phase moves
    /// 0.0367 turns per block — far wider than any tolerance tight enough to
    /// call zero — and that wait was measured at up to 5.9 s per gap.
    fn tone_section(&mut self, out: &mut Vec<f32>, blocks: usize) {
        for _ in 0..blocks {
            self.tone(out);
        }
        self.cut(out);
    }

    /// Render tone until the block that contains the carrier's zero crossing,
    /// cut there. For the end of a section whose parameter sweep is already
    /// finished, where [`Self::tone_section`] does not apply.
    fn cut(&mut self, out: &mut Vec<f32>) {
        loop {
            let wrapped = self.fill(true);
            self.run(out);
            if wrapped {
                break;
            }
        }
    }

    /// One block of silence at the input, with the stage still running.
    ///
    /// Three things this buys. It frames the cell, so flipping between a cell
    /// and its neighbours is a jump between two known points in time; the
    /// stage still runs on the zeros, so every ADAA state settles onto its
    /// zero-input value — `x_prev = 0` and the cached `F` consistent with it —
    /// which is what makes each cell start from the same state as every other
    /// rather than inheriting the last one's; and the phases are returned to
    /// zero at the end, which is what keeps the boundary itself from
    /// ticking.
    fn gap(&mut self, out: &mut Vec<f32>, min_s: f32) {
        let min_blocks = (min_s * SAMPLE_RATE / BLOCK as f32) as usize;
        for _ in 0..min_blocks {
            self.silent_block(out);
        }
        // Resume on a zero crossing, by construction rather than by waiting
        // for one.
        //
        // Resuming at an arbitrary phase steps the input from `0` to as much
        // as `AMP` in one sample — a tick at every cell boundary, in a file
        // whose whole job is to let you hear whether the stage clicks. Both
        // sines are `0` at phase `0`, so the input picks up exactly where
        // the silence left off, and the zeroed ADAA states difference the
        // first sample against `0`, which is where they already are.
        //
        // Snapping rather than waiting out the phase is what makes this
        // cheap: the phase advances 0.0367 turns per block, which is wider
        // than any tolerance tight enough to call "zero", so waiting for the
        // orbit to land inside one is unbounded — measured at up to 5.9 s of
        // silence per gap. Zero is also the better experiment: every cell
        // then begins from the same waveform, so the only thing that differs
        // between two cells is the knob position.
        self.carrier_phase = 0.0;
        self.mod_phase = 0.0;
    }

    fn silent_block(&mut self, out: &mut Vec<f32>) {
        self.carrier.fill(0.0);
        self.modulator.fill(0.0);
        self.run(out);
    }

    fn run(&mut self, out: &mut Vec<f32>) {
        self.shaper
            .process_dual(&mut self.carrier, &self.modulator, &mut self.aux);
        for k in 0..BLOCK {
            out.push(self.carrier[k]);
            out.push(self.carrier[k]);
        }
    }
}

/// The stage in isolation: one steady low sine through it, every algorithm at
/// every drive, then each knob swept on its own.
///
/// # Why this file exists when `shaper-alias.wav` already sweeps algorithms
///
/// The other two files under `Command::Shaper` both go through the whole
/// mi-drum strip — master gain, `WARP.MIX`, the output limiter — and the
/// alias test's source is a Plaits `String`, which is dense in partials from
/// the first millisecond. Both are the right question for their own subject
/// (does the stage colour the kit; does the antialiasing hold up), and neither
/// shows the transfer function, because a waveshaper's output is the transfer
/// function *of whatever you feed it*.
///
/// So: nothing but the stage, and a source with one partial in it. 55 Hz puts
/// every harmonic of the shaping within hearing distance of the fundamental
/// and keeps the input slow enough that aliasing is not the subject — this is
/// the character audition, the complement of the alias test's deliberate worst
/// case. It also keeps the ADAA difference quotient honest: consecutive
/// samples differ by ~3.6e-3 at this amplitude, well above `ADAA_EPS`, so
/// cells exercise the quotient rather than the `dx -> 0` midpoint fallback.
///
/// The modulator is *not* silence, which would be the obvious choice. Ring
/// modulation is `c * m_bl * 1.5`, so a silent modulator makes the digital
/// ring silent and leaves the crossfade holding only half its inputs — half
/// the grid would be mute. 82.5 Hz against 55 Hz is a musical 3:2, so the
/// ring positions sound as sum and difference tones (137.5 and 27.5 Hz) rather
/// than as an arbitrary beating.
///
/// `timbre` is pinned at 0.5, the shipped default, on purpose: this file is
/// two-dimensional, and the third parameter is held where the kit runs it.
///
/// # No normalisation, and no headroom fudge
///
/// Every algorithm's output is bounded by the saturators in front of it —
/// `clip3` cannot leave `±2/3`, so the crossfade tops out at ~0.943 (equal
/// power: two weights of 0.707 over two ceilings of 2/3), the fold and both
/// rings at `2/3`. Nothing here can reach full scale — the file's measured
/// peak is 0.9438 — so it is written at the levels the stage actually
/// produces and the drive sweep's loudness is the drive sweep's, not a
/// limiter's. The bound is the same one the level-matching unit test pins at
/// 1.0.
///
/// # What to listen for
///
/// Per cell: the crossfade is two clean tones mixed, the fold thickens into a
/// buzzy spectrum, the diode ring grows grit where its 0.667 dead zone stops
/// conducting, and the digital ring is a frequency relocator rather than a
/// saturator. Across three of the four rows, drive flattens the sine toward a
/// square and lifts RMS with it.
///
/// The fold row does the opposite, and it is the most interesting thing in
/// the file. At `timbre` 0.5 the folder's depth is 3, so the clipper's 2/3
/// ceiling puts its input exactly at 2 — where `fold` is back at zero. Drive
/// the carrier hard and the clipper sits at that ceiling for most of the
/// cycle, so the folder is parked at zero except for a sweep at every zero
/// crossing: peak holds at 0.667 while RMS falls 8.7 dB and the crest factor
/// goes from 3.0 to 11.6 dB. Full drive on the shipped default is not a
/// dirtier fold, it is a pulse train. Whether that is the sound or a problem
/// is what this row is for: the level-matching test pins peaks, which do not
/// move, so nothing else in the suite would notice.
///
/// Across the sweeps: the algorithm sweep crosses three table boundaries with
/// no discontinuity, because all four algorithms are evaluated every sample
/// and the ADAA states never go stale — a click there would be the
/// "moving the knob cannot click" property failing. The drive sweep is
/// monotonic in saturation and, because gain runs 1 -> 8, shows the whole
/// travel from near-clean to fully clipped.
pub fn render_shaper_sine() -> (Vec<f32>, Vec<(f32, String)>) {
    let mut out: Vec<f32> = Vec::with_capacity((MAX_FILE_S * SAMPLE_RATE * 2.0) as usize);
    let mut notes: Vec<(f32, String)> = Vec::new();
    let mut probe = SineProbe::new();
    let blocks = |s: f32| (s * SAMPLE_RATE / BLOCK as f32) as usize;

    let algorithms: [(f32, &str); 4] = [
        (0.0, "crossfade"),
        (1.0 / 3.0, "sine fold"),
        (2.0 / 3.0, "diode ring"),
        (1.0, "digital ring"),
    ];
    // Macro positions rather than `drive` values; `drive_from_macro_position`
    // is the strip's remap, and `1 + 7·m` is the input gain each one lands on.
    let drives = [0.0f32, 1.0 / 3.0, 2.0 / 3.0, 1.0];

    // Algorithm-major, so each character is heard deepening across its row of
    // drives before the next one starts. The gap that follows every cell both
    // frames it for flipping between and resets the ADAA states (see
    // `SineProbe::gap`).
    for (algo, name) in algorithms {
        for m in drives {
            notes.push((
                out.len() as f32 / 2.0 / SAMPLE_RATE,
                format!("{name}, drive {m:.2} (gain {:.2})", 1.0 + 7.0 * m),
            ));
            probe
                .shaper
                .set_parameters(algo, TIMBRE, drive_from_macro_position(m));
            probe.tone_section(&mut out, blocks(CELL_S));
            probe.gap(&mut out, GAP_S);
        }
    }

    // The knob moving with the signal running, at the worst drive for it.
    let sweep_blocks = blocks(SWEEP_S);
    notes.push((
        out.len() as f32 / 2.0 / SAMPLE_RATE,
        "algorithm swept 0 -> 1, drive flat at gain 8.0".to_string(),
    ));
    for b in 0..sweep_blocks {
        let a = b as f32 / (sweep_blocks - 1) as f32;
        probe
            .shaper
            .set_parameters(a, TIMBRE, drive_from_macro_position(1.0));
        probe.tone(&mut out);
    }
    probe.cut(&mut out);
    probe.gap(&mut out, GAP_S);

    notes.push((
        out.len() as f32 / 2.0 / SAMPLE_RATE,
        "drive swept gain 1.0 -> 8.0, algorithm flat on the sine fold".to_string(),
    ));
    for b in 0..sweep_blocks {
        let m = b as f32 / (sweep_blocks - 1) as f32;
        probe
            .shaper
            .set_parameters(1.0 / 3.0, TIMBRE, drive_from_macro_position(m));
        probe.tone(&mut out);
    }
    probe.cut(&mut out);
    probe.gap(&mut out, GAP_S);

    (out, notes)
}
