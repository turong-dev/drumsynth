//! The monotonicity harness (Phase 10).
//!
//! The design rule — "macros are tuned paths, not renamed params: no dead
//! zones, monotonic, loudness-compensated" — is an assertion about *every*
//! knob on *every* machine. This sweeps a machine's voice knobs 0..1,
//! measures each step, and flags:
//!
//! - **dead zones**: the waveform never moves across the travel, so the knob
//!   is not a tuned path at all;
//! - **non-monotonic loudness**: a *gain* knob (LEVEL/NLEV/STICK/DRIVE) that
//!   does not get louder as the value rises, or a *spectral* knob (filter /
//!   pitch / detune) whose loudness swings hard in both directions;
//! - **non-monotonic decay**: a *time* knob (DEC/TDEC/NDEC/BDEC/SWP_T) whose
//!   envelope decay regresses as the value rises.
//!
//! Each knob family is checked for the output it is supposed to move, and
//! nothing else: filters legitimately sweep quieter as they brighten, a
//! detuned pair beats, a crossfade dips through the middle, and a stick
//! click is an *addition* on top of a loud body. The families are declared in
//! [`family`].
//!
//! It is deterministic — the engine renders bit-identically on every host,
//! so a step's measurement is an exact number, not a spot reading — which is
//! what makes a 0.5 dB tolerance meaningful rather than wishful.

use drum_engine::machines::MachineId;
use drum_engine::DrumEngine;

/// Aggregated counts from a verification run.
#[derive(Clone, Copy, Default)]
pub struct Totals {
    pub checked: usize,
    pub failed: usize,
}

/// RMS dip between adjacent steps that still counts as monotonic. The engine
/// is deterministic, so no measurement noise hides behind this: a spectral
/// knob (filter cutoff) can legitimately trade energy between bands by a
/// fraction of a dB while the *sound* moves the right way.
const RMS_DIP_DB: f32 = 0.5;
/// Decay-time regression that counts as monotonic, as a fraction of the
/// knob's own decay travel. Pitch/sweep knobs have a natural few-ms wobble
/// in their peak-envelope decay without being non-monotonic, so a fixed
/// absolute tolerance would false-positive them; a fraction of the *travel*
/// (5%) scales with the knob — 12 ms slack for a 230 ms wobble, 40 ms for a
/// 800 ms decay knob.
const DECAY_DIP_FRACTION: f32 = 0.05;
/// ...never below this, even for a barely-travelling knob.
const DECAY_DIP_FLOOR_MS: f32 = 5.0;
/// Below this RMS the hit is effectively silent and dB is meaningless
/// (the RMS floor already clamps at -120).
const RMS_FLOOR_DB: f32 = -55.0;
/// The loudness swing (dB) that counts as a *reversal* for a spectral knob
/// (filter, pitch, detune). Such knobs legitimately trade energy between
/// bands as they sweep — a filter dropping a fraction of a dB per step is
/// physics, and a detuned pair beating is its actual sound. This is generous
/// enough to absorb that and strict enough to catch a knob whose loudness
/// swings hard in *both* directions (a genuinely non-monotonic sweep).
const SPECTRAL_SWING_DB: f32 = 1.5;
/// The largest adjacent waveform difference (normalised) below which a knob
/// counts as dead. A working knob changes the body of the wave by a lot; a
/// dead one reproduces it bit-for-bit (≈0.000).
const DEAD_DISTANCE: f32 = 0.03;
/// Longest single hit we render. A cymbal at full decay needs the room;
/// everything else stops at silence long before.
const MAX_SECONDS: f32 = 12.0;
/// How much waveform is kept for the dead-zone metric. The body of a drum
/// hit lives in the first ~100 ms.
const WAVE_CAP_SECONDS: f32 = 0.1;

/// The voice knobs worth checking: named, non-special controls. Excludes the
/// machine selector, pan (spatial — mono-sum RMS reads it as flat), the send
/// auxes (routing, not voice loudness), and reserved slots.
pub fn knob_slots(id: MachineId) -> impl Iterator<Item = (usize, &'static str)> {
    id.macros()
        .into_iter()
        .enumerate()
        .filter(|(_, m)| {
            m.name != "RESV" && m.name != "MACH" && m.name != "PAN" && !m.name.starts_with("SEND.")
        })
        .map(|(i, m)| (i, m.name))
}

/// What a knob is allowed to move, so the harness checks the right metric.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Family {
    /// Amplitude — must get louder as the value rises. LEVEL, NLEV, STICK, DRIVE.
    Gain,
    /// Time — must ring longer as the value rises. DEC, TDEC, NDEC, BDEC, SWP_T.
    Time,
    /// Pitch / spectral — has no canonical direction; must move the wave, not swing.
    Spectral,
    /// A crossfade — loudness legitimately dips through the middle. NMIX, BAL.
    Mix,
    /// A trigger-behaviour knob with no single-hit sound of its own. RST.
    Behavioral,
}

/// Classify a macro by what its travel is supposed to change.
fn family(name: &str) -> Family {
    match name {
        "LEVEL" | "NLEV" | "STICK" | "DRIVE" => Family::Gain,
        "DEC" | "TDEC" | "NDEC" | "BDEC" | "SWP_T" => Family::Time,
        "NMIX" | "BAL" => Family::Mix,
        "RST" => Family::Behavioral,
        _ => Family::Spectral,
    }
}

/// One step of a sweep.
struct Step {
    rms_db: f32,
    decay_ms: f32,
    length_ms: f32,
    mono: Vec<f32>,
}

/// Sweep one knob across `steps` values and check the travel.
struct KnobReport {
    machine: MachineId,
    macro_name: &'static str,
    rms_min: f32,
    rms_max: f32,
    decay_min: f32,
    decay_max: f32,
    len_max: f32,
    rms_mono: bool,
    decay_mono: bool,
    dead: bool,
}

/// Normalised difference between two waveforms: how much the body of the hit
/// changed, independent of overall level (each wave is scaled by its own mean
/// magnitude). A knob that changes pitch scores high here; a dead knob
/// scores ~0.
fn wave_distance(a: &[f32], b: &[f32]) -> f32 {
    let n = a.len().min(b.len());
    if n == 0 {
        return 0.0;
    }
    let mut den = 0.0f64;
    let mut num = 0.0f64;
    for i in 0..n {
        den += (a[i].abs() + b[i].abs()) as f64;
        num += (a[i] - b[i]).abs() as f64;
    }
    if den < 1e-9 {
        return 0.0;
    }
    (num / den) as f32
}

fn sweep_knob(id: MachineId, slot: usize, steps: usize) -> KnobReport {
    let mut steps_out: Vec<Step> = Vec::with_capacity(steps);

    for i in 0..steps {
        let t = i as f32 / (steps - 1) as f32;
        let mut engine = DrumEngine::new();
        engine.tracks[0].load_machine(id);
        engine.tracks[0].set_macro(slot, t);
        let hit =
            crate::measure::measure_oneshot(&mut engine, 0, MAX_SECONDS, false, WAVE_CAP_SECONDS);
        steps_out.push(Step {
            rms_db: hit.m.rms_db(),
            decay_ms: hit.m.decay_ms(),
            length_ms: hit.m.length_ms(),
            mono: hit.mono,
        });
    }

    let name = id.macros()[slot].name;
    let fam = family(name);

    // Gain knobs must get louder as the value rises. Pairs where both steps
    // sit below the RMS floor are skipped — near silence, dB is quantisation.
    let rms_mono = match fam {
        Family::Gain => steps_out.windows(2).all(|w| {
            if w[0].rms_db < RMS_FLOOR_DB && w[1].rms_db < RMS_FLOOR_DB {
                true
            } else {
                w[1].rms_db >= w[0].rms_db - RMS_DIP_DB
            }
        }),
        // Spectral knobs have no canonical direction; flag only a loudness
        // swing that reverses — hard up *and* hard down across the travel.
        Family::Spectral => {
            let up = steps_out
                .windows(2)
                .any(|w| w[1].rms_db - w[0].rms_db > SPECTRAL_SWING_DB);
            let down = steps_out
                .windows(2)
                .any(|w| w[0].rms_db - w[1].rms_db > SPECTRAL_SWING_DB);
            !(up && down)
        }
        _ => true,
    };

    let decay_max = steps_out
        .iter()
        .map(|s| s.decay_ms)
        .fold(f32::NEG_INFINITY, f32::max);

    // Time knobs must ring longer as the value rises. A pitch/sweep knob has
    // a natural few-ms wobble in its peak-envelope decay without being
    // non-monotonic, so a fixed absolute tolerance would false-positive; a
    // fraction of the *travel* (5%) scales with the knob — 12 ms slack for a
    // 230 ms wobble, 40 ms for an 800 ms decay knob.
    let decay_mono = match fam {
        Family::Time => {
            let decay_dip_ms = (DECAY_DIP_FRACTION * decay_max).max(DECAY_DIP_FLOOR_MS);
            steps_out
                .windows(2)
                .all(|w| w[1].decay_ms >= w[0].decay_ms - decay_dip_ms)
        }
        _ => true,
    };

    // Dead zone: the waveform never moves across the travel. Compare every
    // step to the baseline (value 0) rather than to its neighbour, so an
    // *additive* transient knob — a stick click or noise crack layered over a
    // loud body — counts by its own added energy instead of being buried by
    // the body it adds to.
    let max_move = steps_out
        .iter()
        .skip(1)
        .map(|s| wave_distance(&steps_out[0].mono, &s.mono))
        .fold(0.0f32, f32::max);

    let rms_min = steps_out
        .iter()
        .map(|s| s.rms_db)
        .fold(f32::INFINITY, f32::min);
    let rms_max = steps_out
        .iter()
        .map(|s| s.rms_db)
        .fold(f32::NEG_INFINITY, f32::max);
    let decay_min = steps_out
        .iter()
        .map(|s| s.decay_ms)
        .fold(f32::INFINITY, f32::min);
    let len_max = steps_out.iter().map(|s| s.length_ms).fold(0.0f32, f32::max);

    KnobReport {
        machine: id,
        macro_name: name,
        rms_min,
        rms_max,
        decay_min,
        decay_max,
        len_max,
        rms_mono,
        decay_mono,
        dead: fam != Family::Behavioral && max_move < DEAD_DISTANCE,
    }
}

/// Print one knob's per-step curve — the Phase 10 tuning view: how RMS,
/// decay, and length actually move across the travel, so a failure in
/// [`run`] can be seen (and a curve tuned) rather than just counted.
pub fn dump_knob(id: MachineId, slot: usize, steps: usize) {
    let name = id.macros()[slot].name;
    println!("# {} {}", id.name(), name);
    println!(
        "{:>6}  {:>8}  {:>8}  {:>8}",
        "value", "rms", "decay", "length"
    );
    println!("{:─>6}  {:─>8}  {:─>8}  {:─>8}", "", "", "", "");
    for i in 0..steps {
        let t = i as f32 / (steps - 1) as f32;
        let mut engine = DrumEngine::new();
        engine.tracks[0].load_machine(id);
        engine.tracks[0].set_macro(slot, t);
        let hit =
            crate::measure::measure_oneshot(&mut engine, 0, MAX_SECONDS, false, WAVE_CAP_SECONDS);
        println!(
            "{t:>6.3}  {:>8.2}  {:>8.0}  {:>8.0}",
            hit.m.rms_db(),
            hit.m.decay_ms(),
            hit.m.length_ms(),
        );
    }
    println!();
}

/// Check every machine × voice knob, printing a per-knob table. Returns the
/// aggregate counts; the caller decides the exit code.
pub fn run(ids: &[MachineId], steps: usize) -> Totals {
    println!(
        "{:<12} {:<9} {:>7} {:>7} {:>9} {:>9} {:>9}  verdict",
        "machine", "macro", "rmsmin", "rmsmax", "decmin", "decmax", "lenmax"
    );
    println!("{}", "-".repeat(82));

    let mut totals = Totals::default();
    for &id in ids {
        let knobs: Vec<(usize, &'static str)> = knob_slots(id).collect();
        for (slot, _) in knobs {
            let r = sweep_knob(id, slot, steps);
            totals.checked += 1;

            let mut problems = Vec::new();
            if !r.rms_mono {
                problems.push("rms non-monotonic");
            }
            if !r.decay_mono {
                problems.push("decay non-monotonic");
            }
            if r.dead {
                problems.push("dead zone");
            }
            let verdict = if problems.is_empty() {
                "ok".to_string()
            } else {
                totals.failed += 1;
                format!("FAIL: {}", problems.join(", "))
            };

            println!(
                "{:<12} {:<9} {:>7.1} {:>7.1} {:>9.0} {:>9.0} {:>9.0}  {}",
                r.machine.name(),
                r.macro_name,
                r.rms_min,
                r.rms_max,
                r.decay_min,
                r.decay_max,
                r.len_max,
                verdict,
            );
        }
    }
    totals
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn knob_slots_excludes_special_macros() {
        let id = MachineId::BdClassic;
        let names: Vec<&str> = knob_slots(id).map(|(_, n)| n).collect();
        assert!(!names.contains(&"RESV"));
        assert!(!names.contains(&"MACH"));
        assert!(!names.contains(&"PAN"));
        assert!(!names.contains(&"SEND.DLY"));
        assert!(!names.contains(&"SEND.RVB"));
        assert!(names.contains(&"DEC"), "voice knobs stay: {names:?}");
    }

    #[test]
    fn wave_distance_is_zero_for_identical_waves() {
        let a = vec![0.1f32, -0.2, 0.3, -0.4, 0.5];
        assert_eq!(wave_distance(&a, &a), 0.0);
        let b: Vec<f32> = a.iter().map(|&s| s * 2.0).collect();
        assert!(wave_distance(&a, &b) > 0.2, "level change is a change");
        let c: Vec<f32> = a.iter().map(|&s| -s).collect();
        assert!(wave_distance(&a, &c) > 0.9, "phase flip is a big change");
    }

    #[test]
    fn all_bd_classic_knobs_are_monotonic() {
        // The harness is the dead-zone detector; this pins the one machine
        // we have actually tuned so a regression shows up immediately.
        let totals = run(&[MachineId::BdClassic], 9);
        assert_eq!(
            totals.failed, 0,
            "bd-classic knobs must be monotonic: {} knobs checked, {} failed",
            totals.checked, totals.failed
        );
    }
}
