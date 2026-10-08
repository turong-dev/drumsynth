use drum_engine::machines::{
    SLOT_MACH_0, SLOT_MACH_1, SLOT_MACH_2, SLOT_MACH_3, SLOT_MACH_5, SLOT_MACH_7,
};
use drum_engine::{DrumEngine, BLOCK, SAMPLE_RATE, TRACKS};
use mi_drum_engine::MiDrumEngine;

use crate::wav::append_block;

/// Allocate a `MiDrumEngine` on the heap and construct it in-place.
///
/// The engine is too large for the default test thread stack, so this helper
/// uses `alloc` + `new_in_place` instead of `MiDrumEngine::new()`.
pub(crate) fn mi_drum_in_place() -> Box<MiDrumEngine> {
    use std::alloc::{alloc, Layout};

    unsafe {
        let layout = Layout::new::<MiDrumEngine>();
        let ptr = alloc(layout) as *mut MiDrumEngine;
        assert!(!ptr.is_null(), "failed to allocate MiDrumEngine");
        MiDrumEngine::new_in_place(ptr);
        Box::from_raw(ptr)
    }
}

/// Render a single hit with a tail, for auditioning one track.
///
/// A sustained machine (Dub Siren, Sweep FX) is released partway through the
/// window rather than left to ring out the gate watchdog, so the audition
/// shows the release and the file still terminates when it says it will. A
/// one-shot machine ignores the release, so this changes nothing for it.
pub(crate) fn render_one_shot(engine: &mut DrumEngine, track: usize, seconds: f32) -> Vec<f32> {
    engine.trigger(track, 1.0);

    let total_blocks = (seconds * SAMPLE_RATE / BLOCK as f32) as usize;
    let mut out = Vec::with_capacity(total_blocks * BLOCK * 2);
    let mut l = [0.0f32; BLOCK];
    let mut r = [0.0f32; BLOCK];

    for b in 0..total_blocks {
        if b == total_blocks / 2 {
            engine.release(track);
        }
        engine.process(&mut l, &mut r);
        append_block(&mut out, &l, &r);
    }

    out
}

/// Per-track strip settings for the mi-drum kit pattern.
///
/// `(warps timbre, ripples cutoff, ripples resonance, warps drive, lfo rate,
///  lfo depth, lfo->filter, lfo->warps, ad attack, ad decay, ad->filter,
///  ad->warps, warps oscillator shape)`
///
/// Depths stay moderate: the full-scale end of the filter depth is three
/// octaves, and six tracks all modulating at once drives the master sum into
/// the clipper, which would make the baseline a test of the limiter rather
/// than of the modulation map.
///
/// The last field is the shape of the strip's modulator oscillator, which
/// only sounds when a track's `WARP.IN` selects it. The kit leaves `WARP.IN`
/// at its default, so these are inert here; the oscillator gets a dedicated
/// pass instead.
///
/// Shared by the baseline's bypassed and engaged passes, which have to drive
/// the *same* kit or the A/B is not a comparison.
pub(crate) type MiStripSpec = (
    f32,
    f32,
    f32,
    f32,
    f32,
    f32,
    f32,
    f32,
    f32,
    f32,
    f32,
    f32,
    f32,
);
pub(crate) const MI_KIT_STRIPS: [MiStripSpec; mi_drum_engine::TRACKS] = [
    (
        0.40, 0.55, 0.20, 0.60, 0.45, 0.80, 0.30, 0.20, 0.05, 0.35, 0.25, 0.20, 0.0,
    ),
    (
        0.55, 0.35, 0.40, 0.75, 0.55, 0.60, 0.20, 0.15, 0.10, 0.50, 0.20, 0.15, 0.0,
    ),
    (
        0.30, 0.80, 0.10, 0.50, 0.35, 0.90, 0.35, 0.10, 0.02, 0.25, 0.15, 0.10, 0.0,
    ),
    (
        0.65, 0.45, 0.55, 0.80, 0.60, 0.70, 0.15, 0.30, 0.15, 0.60, 0.30, 0.20, 0.0,
    ),
    (
        0.25, 0.70, 0.30, 0.65, 0.40, 0.75, 0.25, 0.25, 0.08, 0.40, 0.20, 0.25, 0.0,
    ),
    (
        0.50, 0.60, 0.15, 0.55, 0.50, 0.65, 0.20, 0.15, 0.12, 0.30, 0.25, 0.15, 0.0,
    ),
];

/// A 16-step pattern, two bars at 130 BPM. Rows are kick / snare / hat /
/// modal / noise / string against the default kit.
///
/// Shared between the mi-drum baseline (as [`MI_KIT_PATTERN`]) and the
/// Warps/shaper kit comparisons (as [`BACKBEAT`]).
const BACKBEAT_PATTERN: [[bool; 16]; 6] = [
    [
        true, false, false, false, true, false, false, false, true, false, false, true, true,
        false, false, false,
    ],
    [
        false, false, false, false, true, false, false, false, false, false, false, false, true,
        false, true, false,
    ],
    [
        true, false, true, false, true, false, true, false, true, false, true, false, true, false,
        true, true,
    ],
    [
        false, false, true, false, false, false, false, true, false, false, true, false, false,
        false, false, false,
    ],
    [
        false, false, false, true, false, false, false, false, false, true, false, false, false,
        false, true, false,
    ],
    [
        true, false, false, false, false, false, true, false, false, false, false, false, true,
        false, false, false,
    ],
];

/// The mi-drum baseline's view of [`BACKBEAT_PATTERN`].
const MI_KIT_PATTERN: [[bool; 16]; mi_drum_engine::TRACKS] = BACKBEAT_PATTERN;

/// Apply [`MI_KIT_STRIPS`] to the engine's tracks.
///
/// `drive_override` replaces every track's `WARP.DRV`. `Some(0.0)` is the
/// detent that takes Warps out of the path via `Modulator::set_bypass`, which
/// is what the baseline's reference bar uses.
fn apply_mi_kit_strips(
    engine: &mut MiDrumEngine,
    warps_algorithm: f32,
    drive_override: Option<f32>,
) {
    use mi_drum_engine::{
        DeviceEngine, SLOT_AD_ATTACK, SLOT_AD_DECAY, SLOT_AD_FILTER_DEPTH, SLOT_AD_WARPS_DEPTH,
        SLOT_FILT_0, SLOT_FILT_1, SLOT_LFO_DEPTH, SLOT_LFO_FILTER_DEPTH, SLOT_LFO_RATE,
        SLOT_LFO_WARPS_DEPTH, SLOT_STRIP_CUT, SLOT_STRIP_HOLD, SLOT_STRIP_RESO,
        SLOT_WARPS_OSC_SHAPE,
    };

    for (idx, &spec) in MI_KIT_STRIPS.iter().enumerate() {
        let track = &mut engine.tracks_mut()[idx];
        track.set_macro(SLOT_FILT_0, warps_algorithm);
        track.set_macro(SLOT_FILT_1, spec.0);
        track.set_macro(SLOT_STRIP_CUT, spec.1);
        track.set_macro(SLOT_STRIP_RESO, spec.2);
        track.set_macro(SLOT_STRIP_HOLD, drive_override.unwrap_or(spec.3));
        track.set_macro(SLOT_LFO_RATE, spec.4);
        track.set_macro(SLOT_LFO_DEPTH, spec.5);
        track.set_macro(SLOT_LFO_FILTER_DEPTH, spec.6);
        track.set_macro(SLOT_LFO_WARPS_DEPTH, spec.7);
        track.set_macro(SLOT_AD_ATTACK, spec.8);
        track.set_macro(SLOT_AD_DECAY, spec.9);
        track.set_macro(SLOT_AD_FILTER_DEPTH, spec.10);
        track.set_macro(SLOT_AD_WARPS_DEPTH, spec.11);
        track.set_macro(SLOT_WARPS_OSC_SHAPE, spec.12);
    }
}

/// Play [`MI_KIT_PATTERN`] once through, appending to `out`.
///
/// `bars` of 16 steps at 130 BPM, then `tail_s` seconds for the last hit and
/// the send-FX decay.
fn play_mi_kit_pattern(engine: &mut MiDrumEngine, out: &mut Vec<f32>, bars: usize, tail_s: f32) {
    use mi_drum_engine::DeviceEngine;

    let mut l = [0.0f32; BLOCK];
    let mut r = [0.0f32; BLOCK];

    let bpm = 130.0f32;
    let samples_per_step = (SAMPLE_RATE * 60.0 / bpm / 4.0) as usize;
    let total_steps = bars * 16;
    let total_blocks = (total_steps * samples_per_step + (tail_s * SAMPLE_RATE) as usize) / BLOCK;

    let mut next_step = 0usize;
    let mut next_step_at = 0usize;
    for block in 0..total_blocks {
        let block_start = block * BLOCK;
        while next_step_at < block_start + BLOCK && next_step < total_steps {
            let s = next_step % 16;
            for (track, row) in MI_KIT_PATTERN.iter().enumerate() {
                if row[s] {
                    // Velocity varies with the step so the velocity-mod path
                    // is not stuck at full scale for the whole render.
                    let vel = if s.is_multiple_of(4) { 1.0 } else { 0.7 };
                    engine.trigger(track, vel);
                }
            }
            next_step += 1;
            next_step_at += samples_per_step;
        }

        engine.process(&mut l, &mut r);
        append_block(out, &l, &r);
    }
}

/// The mi-drum baseline render, with the offset of each section in seconds.
///
/// The marks exist so the Warps A/B inside the render is findable: it sits
/// 21 seconds in, behind the machine sweep, and a reference you have to hunt
/// for is a reference nobody uses.
pub(crate) fn render_mi_drum_marked(warps_algorithm: f32) -> (Vec<f32>, Vec<(f32, &'static str)>) {
    use mi_drum_engine::{
        DeviceEngine, MiMachineId, SLOT_AD_ATTACK, SLOT_AD_DECAY, SLOT_AD_FILTER_DEPTH,
        SLOT_AD_WARPS_DEPTH, SLOT_FILT_0, SLOT_FILT_1, SLOT_LFO_DEPTH, SLOT_LFO_FILTER_DEPTH,
        SLOT_LFO_RATE, SLOT_LFO_WARPS_DEPTH, SLOT_STRIP_CUT, SLOT_STRIP_HOLD, SLOT_STRIP_RESO,
        SLOT_WARPS_MOD_SRC, SLOT_WARPS_OSC_SHAPE,
    };

    // Every Plaits engine draws noise from one process-global LCG, so the
    // render is only reproducible from a known seed. This does not make two
    // concurrent renders safe — they would interleave their draws — which is
    // why the baseline test below is a single test doing a single render.
    mi_drum_engine::seed_random(mi_drum_engine::DEFAULT_RANDOM_SEED);

    let mut engine = mi_drum_in_place();
    let mut out: Vec<f32> = Vec::new();
    let mut marks: Vec<(f32, &'static str)> = Vec::new();
    let mut l = [0.0f32; BLOCK];
    let mut r = [0.0f32; BLOCK];
    macro_rules! mark {
        ($what:expr) => {
            marks.push((out.len() as f32 / 2.0 / SAMPLE_RATE, $what));
        };
    }

    mark!("machine sweep — one hit per catalogued machine");
    // Half one: one hit per machine on track 0, 0.75 s apart, so every engine
    // in the catalogue contributes to the hash.
    let blocks_per_hit = (0.75 * SAMPLE_RATE / BLOCK as f32) as usize;
    for &id in MiMachineId::ALL.iter() {
        engine.tracks_mut()[0].load_machine(id);
        engine.tracks_mut()[0].set_macro(SLOT_FILT_0, warps_algorithm);
        engine.tracks_mut()[0].set_macro(SLOT_FILT_1, 0.5);
        engine.tracks_mut()[0].set_macro(SLOT_STRIP_CUT, 0.5);
        engine.tracks_mut()[0].set_macro(SLOT_STRIP_RESO, 0.25);
        engine.tracks_mut()[0].set_macro(SLOT_STRIP_HOLD, 0.7);
        // Modulation live, so the Stages path is inside the hash.
        engine.tracks_mut()[0].set_macro(SLOT_LFO_RATE, 0.45);
        engine.tracks_mut()[0].set_macro(SLOT_LFO_DEPTH, 0.8);
        engine.tracks_mut()[0].set_macro(SLOT_AD_ATTACK, 0.05);
        engine.tracks_mut()[0].set_macro(SLOT_AD_DECAY, 0.35);
        engine.tracks_mut()[0].set_macro(SLOT_LFO_FILTER_DEPTH, 0.30);
        engine.tracks_mut()[0].set_macro(SLOT_LFO_WARPS_DEPTH, 0.20);
        engine.tracks_mut()[0].set_macro(SLOT_AD_FILTER_DEPTH, 0.25);
        engine.tracks_mut()[0].set_macro(SLOT_AD_WARPS_DEPTH, 0.20);
        engine.trigger(0, 1.0);
        // Release at two-thirds so a sustained engine's release is in the
        // hash and the window still ends in silence. A one-shot model ignores
        // the release, so this only affects the engines that hold.
        let release_at = blocks_per_hit * 2 / 3;
        for b in 0..blocks_per_hit {
            if b == release_at {
                engine.release(0);
            }
            engine.process(&mut l, &mut r);
            append_block(&mut out, &l, &r);
        }
    }

    // Half two: back to the default kit, with per-track Warps/Ripples settings
    // so the strip is actually exercised.
    engine.load_kit(&mi_drum_engine::DEFAULT_KIT);

    // Half two-a: the kit with Warps bypassed, so the baseline carries its own
    // reference for what the strip is doing. The settings themselves are
    // documented on `MI_KIT_STRIPS`.
    //
    // `WARP.DRV = 0` is a detent onto `Modulator::set_bypass`, not a quiet
    // drive setting. It is not the only uncoloured setting any more —
    // `WARP.MIX = 0` is too — but it is the one that takes the whole stage out
    // rather than mixing it away, which is what makes it the right reference.
    // One bar bypassed and then the same bar as the kit is actually set makes
    // the difference audible instead of a claim in a comment, and if the two
    // bars ever sound the same the strip has stopped working.
    mark!("kit, WARPS BYPASSED (WARP.DRV 0) — the reference");
    apply_mi_kit_strips(&mut engine, warps_algorithm, Some(0.0));
    play_mi_kit_pattern(&mut engine, &mut out, 1, 0.75);

    // Half two-b: the same bars with each track's own `WARP.DRV`, which the
    // kit sets between 0.50 and 0.80 — Warps drive 0.75 to 0.90, three to ten
    // times overdriven.
    mark!("kit, WARPS ENGAGED (WARP.DRV 0.50-0.80) — same bars");
    apply_mi_kit_strips(&mut engine, warps_algorithm, None);
    play_mi_kit_pattern(&mut engine, &mut out, 2, 2.0);

    mark!("gate — held notes and their releases");
    // Half three: the gate itself. A held note and its release, on the two
    // engines that read the Plaits gate as a level rather than an edge. This
    // is the capability the whole note-off path exists for, so it belongs in
    // the baseline rather than only in a unit test: before the gate was real,
    // `SixOp1`/`2`/`3` emitted digital silence for an entire hit and three of
    // the 28 machine windows in half one were exactly -inf.
    for &id in &[
        mi_drum_engine::MiMachineId::SixOp1,
        mi_drum_engine::MiMachineId::SixOp3,
    ] {
        engine.tracks_mut()[0].load_machine(id);
        engine.trigger(0, 1.0);

        // Two seconds held, then two seconds of tail after the release.
        for &do_release in &[false, true] {
            if do_release {
                engine.release(0);
            }
            for _ in 0..(2.0 * SAMPLE_RATE / BLOCK as f32) as usize {
                engine.process(&mut l, &mut r);
                append_block(&mut out, &l, &r);
            }
        }
    }

    mark!("Warps modulator oscillator — five shapes against the kick");
    // Half four: the strip's own oscillator on Warps' modulator input, one
    // shape per hit, on the kick.
    //
    // This pass used to sweep `WARP.CAR`, which put the same five shapes on
    // Warps' *carrier* input — where they replaced the voice rather than
    // modulating it, so the kick became a drone for the length of its own
    // decay and the machine was only an FM index. On the modulator side the
    // kick stays the kick and the oscillator ring-modulates it, which is the
    // thing worth auditioning. A ring-mod algorithm, because that is where a
    // modulator is most plainly audible.
    engine.tracks_mut()[0].load_machine(mi_drum_engine::MiMachineId::PeaksBassDrum);
    engine.tracks_mut()[0].set_macro(SLOT_WARPS_MOD_SRC, 1.0); // oscillator
    engine.tracks_mut()[0].set_macro(SLOT_FILT_0, 0.25); // analog ring mod
    engine.tracks_mut()[0].set_macro(SLOT_STRIP_HOLD, 0.4);
    for step in 0..5 {
        engine.tracks_mut()[0].set_macro(SLOT_WARPS_OSC_SHAPE, step as f32 / 5.0 + 0.05);
        engine.trigger(0, 1.0);
        for b in 0..blocks_per_hit {
            if b == blocks_per_hit * 2 / 3 {
                engine.release(0);
            }
            engine.process(&mut l, &mut r);
            append_block(&mut out, &l, &r);
        }
    }
    engine.tracks_mut()[0].set_macro(SLOT_WARPS_OSC_SHAPE, 0.0);

    (out, marks)
}

/// A 16-step pattern per track. `true` means trigger.
#[derive(Default)]
pub(crate) struct Pattern {
    pub(crate) tracks: [[bool; 16]; TRACKS],
    /// Per-step semitone offsets, applied via `Track::retune` before each
    /// trigger. 0 = the track's macro pitch; anything else transposes that
    /// hit. All-zero (the default) is the pitchless kit. Row `i` only means
    /// anything when `tracks[i][s]` is `true`.
    pub(crate) notes: [[i8; 16]; TRACKS],
}

impl Pattern {
    /// A full 8-track groove: kick, snare, closed + open metallic hat, clap,
    /// tom fill, cowbell accent, and a tonal synth pulse.
    pub(crate) fn demo() -> Self {
        let mut p = Self::default();
        p.set_melody(
            7,
            &[
                (0, 0),
                (4, -3),
                (8, -5),
                (12, -3),
                (16, -2),
                (20, -3),
                (24, 0),
                (28, 5),
            ],
        );

        // 0 — BdClassic (kick): syncopated groove
        p.tracks[0] = [
            true, false, false, false, false, false, true, false, false, false, true, false, false,
            false, false, false,
        ];

        // 1 — SdNatural (snare): backbeat + ghost
        p.tracks[1] = [
            false, false, false, false, true, false, false, false, false, false, false, false,
            true, false, false, true,
        ];

        // 2 — HatClassic (closed hat): 8th notes + flam
        p.tracks[2] = [
            true, false, true, false, true, false, true, false, true, false, true, false, true,
            false, true, true,
        ];

        // 3 — HH Basic (metallic open hat): the "and" of beat 2
        p.tracks[3] = [
            false, false, false, false, false, true, false, false, false, false, false, false,
            false, false, false, false,
        ];

        // 4 — Cp (clap): doubles the second backbeat
        p.tracks[4] = [
            false, false, false, false, false, false, false, false, false, false, false, false,
            true, false, false, false,
        ];

        // 5 — Tom: end-of-bar fill
        p.tracks[5] = [
            false, false, false, false, false, false, false, false, false, false, false, false,
            false, false, true, true,
        ];

        // 6 — CbClassic (cowbell): off-beat accents
        p.tracks[6] = [
            false, false, true, false, false, false, true, false, false, false, true, false, false,
            false, true, false,
        ];

        // 7 — SyTone: bassline pulse on the downbeats + a high note
        p.tracks[7] = [
            true, false, false, false, true, false, false, false, true, false, false, false, true,
            false, false, false,
        ];

        p
    }

    /// Set a melody lane: `(step, semitones)` pairs. Steps run 0..32 across
    /// two bars; offsets are clamped into the 16-step row by step % 16.
    fn set_melody(&mut self, track: usize, notes: &[(usize, i8)]) {
        for &(step, semis) in notes {
            self.notes[track][step % 16] = semis;
        }
    }
}

/// Render a pattern over the 8-track kit from `DrumEngine::new()`, block by
/// block, exactly as the firmware will.
///
/// Note the structure: the sequencer decides what to trigger *between*
/// blocks, never inside `process`. That is the same discipline the firmware
/// needs, so keeping it here means the host and target behave identically —
/// including the up-to-667µs of timing quantisation that block processing
/// imposes on trigger timing.
pub(crate) fn render_pattern(pattern: &Pattern, bpm: f32, bars: usize) -> Vec<f32> {
    let mut engine = DrumEngine::new();
    setup_kit_mix(&mut engine);

    let samples_per_step = (SAMPLE_RATE * 60.0 / bpm / 4.0) as usize;
    let total_steps = bars * 16;
    // A couple of seconds of tail so the last hit is not truncated.
    let total_samples = total_steps * samples_per_step + (2.0 * SAMPLE_RATE) as usize;
    let total_blocks = total_samples / BLOCK;

    let mut out = Vec::with_capacity(total_blocks * BLOCK * 2);
    let mut l = [0.0f32; BLOCK];
    let mut r = [0.0f32; BLOCK];

    let mut next_step = 0usize;
    let mut next_step_at = 0usize;

    for block in 0..total_blocks {
        let block_start = block * BLOCK;

        while next_step_at < block_start + BLOCK && next_step < total_steps {
            let s = next_step % 16;
            // Trigger whichever tracks have a step this row.
            let mut i = 0;
            while i < TRACKS {
                if pattern.tracks[i][s] {
                    let vel = default_velocity_for_track(i, s);
                    // Melodic lane: transpose the track to the step's note
                    // *before* the trigger so the hit lands in pitch.
                    // Absolute, and applied on every hit — `retune` is
                    // sticky, so `0` ("the track's macro pitch") has to be
                    // re-applied too, or the previous hit's transposition
                    // would carry over.
                    engine.tracks[i].retune(pattern.notes[i][s] as f32);
                    engine.trigger(i, vel);
                }
                i += 1;
            }
            next_step += 1;
            next_step_at += samples_per_step;
        }

        engine.process(&mut l, &mut r);
        append_block(&mut out, &l, &r);
    }

    out
}

/// Render a retrigger/choke stress test: dense rolls, quantised to block
/// boundaries like `render_pattern`, that repeatedly land on sounding
/// voices (the "lots of notes quickly" click scenario), including the
/// closed-hat hitting on top of a ringing open hat so the choke fade is
/// exercised every iteration.
pub(crate) fn render_stress(seconds: f32) -> Vec<f32> {
    let mut engine = DrumEngine::new();
    setup_kit_mix(&mut engine);

    let total_samples = (seconds * SAMPLE_RATE) as usize;
    let total_blocks = total_samples / BLOCK;
    let mut out = Vec::with_capacity(total_blocks * BLOCK * 2);
    let mut l = [0.0f32; BLOCK];
    let mut r = [0.0f32; BLOCK];

    // (track, trigger period in samples): kick ~137 Hz roll, snare roll,
    // closed hat ~107 Hz roll (each hit chokes the ringing open hat), open
    // hat re-triggered every ~38 ms so there is always a tail to cut, and a
    // fast SyTone pulse.
    let schedule = [
        (0, 350usize),
        (1, 300usize),
        (2, 450usize),
        (3, 1800usize),
        (7, 400usize),
    ];
    let mut next = [0usize; TRACKS];

    for block in 0..total_blocks {
        let block_start = block * BLOCK;
        for &(track, period) in &schedule {
            while next[track] < block_start + BLOCK {
                engine.trigger(track, 1.0);
                next[track] += period;
            }
        }
        engine.process(&mut l, &mut r);
        append_block(&mut out, &l, &r);
    }

    out
}

/// Configure per-track strip settings (pan, level, filter, drive) and a few
/// macro tweaks so the kit sounds balanced across the stereo field rather
/// than all-centre, all-equal. Preserves the choke/layer masks already set
/// by `DrumEngine::new` (open-hat ↔ closed-hat relation).
pub(crate) fn setup_kit_mix(engine: &mut DrumEngine) {
    use drum_engine::dsp::SvfMode;
    use drum_engine::StripParams;

    // (track, pan, level, filter_mode, cutoff_hz, reso_q, drive)
    let configs: [(usize, f32, f32, SvfMode, f32, f32, f32); TRACKS] = [
        (0, 0.00, 0.95, SvfMode::Off, 1000.0, 0.707, 3.0), // kick — centre
        (1, -0.25, 0.75, SvfMode::Off, 1000.0, 0.707, 1.0), // snare — slightly L
        (2, 0.35, 0.45, SvfMode::Off, 1000.0, 0.707, 1.0), // closed hat — R
        (3, 0.40, 0.40, SvfMode::Off, 1000.0, 0.707, 1.0), // HH Basic metallic — R
        (4, -0.35, 0.65, SvfMode::Off, 1000.0, 0.707, 1.0), // clap — L
        (5, -0.50, 0.75, SvfMode::Lp, 3000.0, 0.707, 1.0), // tom — far L, warm LP
        (6, 0.15, 0.50, SvfMode::Off, 1000.0, 0.707, 1.0), // cowbell — near centre
        (7, -0.10, 0.55, SvfMode::Off, 1000.0, 0.707, 1.2), // SY Tone — near centre, drive
    ];

    for &(track, pan, level, f_mode, f_cutoff_hz, f_reso_q, drive) in &configs {
        let choke = engine.tracks[track].strip.choke_mask;
        let layer = engine.tracks[track].strip.layer_mask;
        let strip = StripParams {
            f_mode,
            f_cutoff_hz,
            f_reso_q,
            drive,
            pan,
            level,
            choke_mask: choke,
            layer_mask: layer,
            ..StripParams::default()
        };
        engine.tracks[track].set_strip(&strip);
    }

    // Macro tweaks — a few beyond the defaults to get more musical results:

    // Kick: Driven with decay
    engine.tracks[0].set_macro(SLOT_MACH_5, 0.75); // DEC
    engine.tracks[0].set_macro(SLOT_MACH_7, 0.5); // DRIVE

    // Clap: slightly more body, less pure noise (BAL default 0.80 noise-heavy)
    engine.tracks[4].set_macro(SLOT_MACH_7, 0.65); // BAL

    // Tom: tune lower (~132 Hz, a mid tom) and add a bit more stick
    engine.tracks[5].set_macro(SLOT_MACH_0, 0.50); // TUNE → ~132 Hz
    engine.tracks[5].set_macro(SLOT_MACH_7, 0.40); // STICK

    // Cowbell: classic 808 tuning — ~540 Hz base, wider detune, short decay
    engine.tracks[6].set_macro(SLOT_MACH_0, 0.34); // TUNE → ~538 Hz
    engine.tracks[6].set_macro(SLOT_MACH_1, 0.86); // DET → ~1.43 ratio (the classic 540/800 pair)
    engine.tracks[6].set_macro(SLOT_MACH_5, 0.15); // DEC → ~85ms

    // SY Tone: pitch it as a bassline — low note, moderate FM
    engine.tracks[7].set_macro(SLOT_MACH_0, 0.20); // TUNE → ~132 Hz (bass range)
    engine.tracks[7].set_macro(SLOT_MACH_3, 0.50); // MOD.AMT
    engine.tracks[7].set_macro(SLOT_MACH_2, 0.30); // FDBK
    engine.tracks[7].set_macro(SLOT_MACH_5, 0.40); // DEC → ~830ms (rings a bit)

    // --- LFO modulation demo ---
    // Snare (track 1): slow sine LFO on a LP filter, adds movement.
    engine.tracks[1].mod_state.lfos[0].set_params(
        0.3, // 0.3 Hz — very slow sweep
        drum_engine::dsp::LfoWave::Sine,
        drum_engine::dsp::LfoMode::Trig,
        0.5, // depth: ±0.5
        drum_engine::dsp::ModDest::FilterCutoff,
        0.0,
    );
    // The snare needs a filter for the LFO to act on. Start from the strip as
    // configured above (keeping its choke/layer masks, level, pan, drive …)
    // rather than rebuilding from `StripParams::default()`, which would
    // silently drop anything the configs loop preserved.
    let mut snare_strip = engine.tracks[1].strip;
    snare_strip.f_mode = SvfMode::Lp;
    snare_strip.f_cutoff_hz = 3000.0;
    snare_strip.f_reso_q = 1.5;
    engine.tracks[1].set_strip(&snare_strip);

    // --- Velocity modulation demo ---
    // Kick (track 0): velocity → drive. Hard hits are gutsier.
    engine.tracks[0].mod_state.vel_mods[0] = drum_engine::VelMod {
        dest: drum_engine::dsp::ModDest::Drive,
        depth: 0.3,
    };

    // --- Send FX demo (Phase 5) ---
    // Snare (track 1): send to reverb for ambient backbeat space.
    engine.tracks[1].strip.send_reverb = 0.5;
    engine.tracks[1].strip.send_delay = 0.25;
    // Clap (track 4): send to delay for a reggae-ish ghost echo.
    engine.tracks[4].strip.send_delay = 0.25;
    // Cowbell (track 6): light reverb send for an "other room" accent.
    engine.tracks[6].strip.send_reverb = 0.5;

    // Refresh the strip caches so the per-block fast path (no mod active for
    // clap/cowbell) sees the new send levels. Copy each strip before calling
    // `set_strip` so we don't simultaneously borrow the track mut and immut.
    for t in [1usize, 4, 6] {
        let strip = engine.tracks[t].strip;
        engine.tracks[t].set_strip(&strip);
    }

    // FX bus configuration: a medium hall reverb and a slap-back delay.
    engine.send_fx.delay.set_params(0.346, 0.45, 5_000.0, 1.0);
    engine.send_fx.reverb.set_params(0.022, 0.86, 4_500.0, 1.0);
    engine.send_fx.drive = 1.0;
}

/// Per-row accent patterns so the kit pattern sounds less robotic on the
/// demo. Maps track index → step → velocity.
pub(crate) fn default_velocity_for_track(track: usize, step: usize) -> f32 {
    match (track, step) {
        // Kick — strong on downbeats, softer on syncopation
        (0, 0) | (0, 8) => 0.95,
        (0, 6) | (0, 10) => 0.82,
        // Snare — backbeat strong, ghost note soft
        (1, 4) | (1, 12) => 0.90,
        (1, 15) => 0.50,
        // Closed hat — accent on even steps (downbeats), lighter off-beats
        (2, _) if step.is_multiple_of(4) => 0.85,
        (2, 15) => 0.70, // flam at the end
        (2, _) => 0.50,
        // HH Basic metallic hat
        (3, _) => 0.75,
        // Clap
        (4, _) => 0.85,
        // Tom fill — crescendo into the next bar
        (5, 14) => 0.70,
        (5, 15) => 0.85,
        // Cowbell — short and punchy
        (6, _) => 0.65,
        // SY Tone — bassline pulse, steady
        (7, _) => 0.80,
        _ => 0.80,
    }
}

/// A one-entry manifest of the gate demo, printed alongside the WAV so the
/// file is navigable without scrubbing.
pub(crate) type GateTimeline = Vec<(f32, String, &'static str)>;

/// Render the gate open/close demo for mi-drum.
///
/// Each engine gets: a short silence, a note-on, a two-second hold, a note-off,
/// then two seconds for the release to run out. The hold is what the gate fix
/// makes possible — before it, the Plaits trigger was a one-block pulse and
/// these voices died 0.5 ms in.
///
/// The three `SixOp` engines lead, because they were the ones emitting digital
/// silence. `chiptune` is last and labelled, because measurement says it is
/// the one Plaits engine that ignores a falling gate entirely, and a demo that
/// hid that would be dishonest about the state of the work.
pub(crate) fn render_gate_demo_mi() -> (Vec<f32>, GateTimeline) {
    use mi_drum_engine::{DeviceEngine, MiMachineId, BLOCK};

    // Every Plaits engine draws from one process-global LCG, so the render is
    // only reproducible from a known seed.
    mi_drum_engine::seed_random(mi_drum_engine::DEFAULT_RANDOM_SEED);

    let hold_s = 2.0f32;
    let release_s = 2.0f32;
    let gap_s = 0.35f32;
    let per_engine = hold_s + release_s + gap_s;

    // (machine, why it is in the list)
    let entries: &[(MiMachineId, &str)] = &[
        (
            MiMachineId::SixOp1,
            "was DIGITAL SILENCE before the gate was real",
        ),
        (
            MiMachineId::SixOp2,
            "was DIGITAL SILENCE before the gate was real",
        ),
        (
            MiMachineId::SixOp3,
            "was DIGITAL SILENCE before the gate was real",
        ),
        (
            MiMachineId::String,
            "sustained engine; the gate hold is the note",
        ),
        (
            MiMachineId::Modal,
            "sustained engine; the gate hold is the note",
        ),
        (
            MiMachineId::VirtualAnalog,
            "outer LPG; release follows MACH 7 decay",
        ),
        (
            MiMachineId::PeaksBassDrum,
            "PEAKS drum: gate is per-sample flags",
        ),
        (
            MiMachineId::PeaksSnareDrum,
            "PEAKS drum: gate is per-sample flags",
        ),
        (
            MiMachineId::BassDrum,
            "one-shot: the note-off must NOT cut it",
        ),
        (MiMachineId::HiHat, "one-shot: the note-off must NOT cut it"),
        (
            MiMachineId::Chiptune,
            "ignores a falling gate - rings on past it",
        ),
    ];

    let mut engine = mi_drum_in_place();
    // Headroom. At the default master gain every voice in this list hits the
    // output clipper, which flattens exactly the envelope shape the demo
    // exists to show. Backing the master off keeps the hold and the release
    // both audible as level rather than as time spent against a limiter.
    engine.set_master_gain(0.35);
    let mut out: Vec<f32> =
        Vec::with_capacity((entries.len() as f32 * per_engine * SAMPLE_RATE * 2.0) as usize);
    let mut l = [0.0f32; BLOCK];
    let mut r = [0.0f32; BLOCK];
    let mut timeline = GateTimeline::new();

    let blocks = |s: f32| (s * SAMPLE_RATE / BLOCK as f32) as usize;

    for (i, (id, why)) in entries.iter().enumerate() {
        // Start each engine on a fresh voice so the previous release cannot
        // bleed into the silence that frames this one.
        engine.tracks_mut()[0].load_machine(*id);
        timeline.push((i as f32 * per_engine, id.name().to_string(), why));

        // Leading silence, so the note-on is an attack you can hear start.
        for _ in 0..blocks(gap_s) {
            engine.process(&mut l, &mut r);
            append_block(&mut out, &l, &r);
        }

        // Note-on, then hold.
        engine.trigger(0, 1.0);
        for _ in 0..blocks(hold_s) {
            engine.process(&mut l, &mut r);
            append_block(&mut out, &l, &r);
        }

        // Note-off, then let the release run.
        engine.release(0);
        for _ in 0..blocks(release_s) {
            engine.process(&mut l, &mut r);
            append_block(&mut out, &l, &r);
        }
    }

    (out, timeline)
}

/// Render the gate open/close demo for the drum device.
///
/// [`DubSiren`](drum_engine::machines::DubSiren) and
/// [`SweepFx`](drum_engine::machines::SweepFx) were the two self-timed gesture
/// machines and are now gated, so they belong in the same demo as the mi-drum
/// voices. The rest of the catalogue is here as a control: a note-off must not
/// shorten a one-shot drum hit.
pub(crate) fn render_gate_demo_drum() -> (Vec<f32>, GateTimeline) {
    use drum_engine::{DeviceEngine, MachineId};

    let hold_s = 2.0f32;
    let release_s = 2.0f32;
    let gap_s = 0.35f32;
    let per_engine = hold_s + release_s + gap_s;

    let entries: &[(MachineId, &str)] = &[
        (
            MachineId::DubSiren,
            "was self-timed; now holds until note-off",
        ),
        (
            MachineId::SweepFx,
            "was self-timed; now holds until note-off",
        ),
        (
            MachineId::BdClassic,
            "one-shot kick: the note-off must NOT cut it",
        ),
        (
            MachineId::SdNatural,
            "one-shot snare: the note-off must NOT cut it",
        ),
        (
            MachineId::CyMetallic,
            "one-shot cymbal: the note-off must NOT cut it",
        ),
    ];

    let mut engine = DrumEngine::new();
    // Headroom, for the same reason as the mi-drum demo.
    engine.set_master_gain(0.7);
    let mut out: Vec<f32> =
        Vec::with_capacity((entries.len() as f32 * per_engine * SAMPLE_RATE * 2.0) as usize);
    let mut l = [0.0f32; BLOCK];
    let mut r = [0.0f32; BLOCK];
    let mut timeline = GateTimeline::new();

    let blocks = |s: f32| (s * SAMPLE_RATE / BLOCK as f32) as usize;

    for (i, (id, why)) in entries.iter().enumerate() {
        engine.tracks[0].load_machine(*id);
        timeline.push((i as f32 * per_engine, id.name().to_string(), why));

        for _ in 0..blocks(gap_s) {
            engine.process(&mut l, &mut r);
            append_block(&mut out, &l, &r);
        }
        engine.trigger(0, 1.0);
        for _ in 0..blocks(hold_s) {
            engine.process(&mut l, &mut r);
            append_block(&mut out, &l, &r);
        }
        engine.release(0);
        for _ in 0..blocks(release_s) {
            engine.process(&mut l, &mut r);
            append_block(&mut out, &l, &r);
        }
    }

    (out, timeline)
}

/// The two-bar backbeat shared by the kit A/B comparisons — same groove, same
/// tempo, so the only variable between passes and files is the strip.
///
/// A reference to [`BACKBEAT_PATTERN`], which is shared with the mi-drum baseline.
const BACKBEAT: [[bool; 16]; 6] = BACKBEAT_PATTERN;

/// Both kit A/B comparisons audition at 130 BPM, two bars of 16ths, with a
/// 2 s tail.
const BACKBEAT_BPM: f32 = 130.0;
const BACKBEAT_STEPS: usize = 16;
const BACKBEAT_BARS: usize = 2;
const BACKBEAT_TAIL_S: f32 = 2.0;

/// Number of blocks in one backbeat pass (two bars + tail).
fn backbeat_blocks() -> usize {
    let step_s = 60.0 / BACKBEAT_BPM / 4.0;
    let total_s = BACKBEAT_STEPS as f32 * step_s * BACKBEAT_BARS as f32 + BACKBEAT_TAIL_S;
    (total_s * SAMPLE_RATE / BLOCK as f32) as usize
}

/// Render one pass of [`BACKBEAT`] through `engine`, appending interleaved
/// stereo to `out`. Triggers land on block boundaries, quantised the same way
/// as `render_pattern`.
fn play_backbeat(engine: &mut MiDrumEngine, out: &mut Vec<f32>, total_blocks: usize) {
    use mi_drum_engine::DeviceEngine;

    let samples_per_step = (SAMPLE_RATE * 60.0 / BACKBEAT_BPM / 4.0) as usize;
    let total_steps = BACKBEAT_STEPS * BACKBEAT_BARS;
    let mut next_step = 0usize;
    let mut next_step_at = 0usize;
    let mut l = [0.0f32; BLOCK];
    let mut r = [0.0f32; BLOCK];

    for b in 0..total_blocks {
        let block_start = b * BLOCK;
        while next_step < total_steps && next_step_at < block_start + BLOCK {
            let s = next_step % BACKBEAT_STEPS;
            for (track, row) in BACKBEAT.iter().enumerate() {
                if row[s] {
                    engine.trigger(track, if s.is_multiple_of(4) { 1.0 } else { 0.7 });
                }
            }
            next_step += 1;
            next_step_at += samples_per_step;
        }
        engine.process(&mut l, &mut r);
        append_block(out, &l, &r);
    }
}

/// Render the default kit twice: as shipped, then with Warps bypassed on the
/// four drum tracks.
///
/// This is the decision the drive sweep sets up but cannot answer on its own.
/// Warps' `drive` doubles as the wet/dry mix, so "a lot of Warps" and "almost no
/// dry signal" are the same setting — there is no in-between where a track is
/// coloured but still mostly itself. On a simple kick that reads as character;
/// on a six-operator FM voice, cross-modulating it with itself turns 19
/// partials into hundreds of inharmonic sum-and-difference products, which is
/// what "gritty" is.
///
/// So the split is per track: drum tracks bypassed, melodic tracks left alone.
pub(crate) fn render_warps_kit_comparison() -> (Vec<f32>, Vec<(f32, String)>) {
    use drum_engine::machines::SLOT_STRIP_HOLD as WARP_DRV_SLOT;
    use mi_drum_engine::{DeviceEngine, SAMPLE_RATE as SR};

    let total_blocks = backbeat_blocks();
    let mut out: Vec<f32> = Vec::with_capacity(total_blocks * BLOCK * 2 * 2);
    let mut notes: Vec<(f32, String)> = Vec::new();

    for bypass_drums in [false, true] {
        // Re-seeded per pass, like `render_shaper_kit_comparison` below:
        // `stmlib::Random` is a process-global generator, so a pass that
        // continued the previous pass's stream would A/B two different
        // performances rather than two strip settings.
        mi_drum_engine::seed_random(mi_drum_engine::DEFAULT_RANDOM_SEED);

        let mut engine = mi_drum_in_place();
        engine.set_master_gain(0.5);
        if bypass_drums {
            // Tracks 0-3 are the Peaks drums in the default kit, 4-5 the Plaits
            // voices. 0.0 is a true bypass, not a zero drive.
            for t in 0..4usize {
                engine.tracks_mut()[t].set_macro(WARP_DRV_SLOT, 0.0);
            }
        }

        notes.push((
            out.len() as f32 / 2.0 / SR,
            if bypass_drums {
                "B - drum tracks BYPASSED, melodic tracks still warped".to_string()
            } else {
                "A - default kit as shipped (all tracks 60% warped)".to_string()
            },
        ));

        play_backbeat(&mut engine, &mut out, total_blocks);
    }

    (out, notes)
}

/// Print a manifest next to its WAV so the file is navigable without scrubbing.
pub(crate) fn print_gate_timeline(path: &str, samples: usize, timeline: &GateTimeline) {
    println!(
        "\nwrote {path} ({:.2}s)",
        samples as f32 / 2.0 / SAMPLE_RATE
    );
    println!("  time    engine          what you are hearing");
    println!("  ------  --------------  ----------------------------------------");
    for (t, name, what) in timeline {
        println!("  {t:>5.1}s  {name:<14}  {what}");
    }
}

/// Render the Warps drive sweep: the kick at each point on the `WARP.DRV` axis,
/// from the clean bypass to full destruction.
///
/// Every setting gets the same eight hits at a steady tempo, so the only thing
/// changing between them is the shaping stage.
///
/// # The labels below are the shaper's, not Warps'
///
/// This function and its `warps` filename predate the substitution. Warps'
/// `drive` was `0.5·drive` blended towards `24·drive⁵`, so its travel used to be
/// described in pre-gain (0.4 / 1.0 / 3.4 / 9.7 / 24) and those numbers were
/// printed here. `core::dsp::shaper` maps the macro onto the input gain of its
/// cubic clipper instead — `1 + 7·macro`, so `1.0` to `8.0` — and the labels
/// quote gain, because gain is what the code actually computes. The
/// `WARP.DRV 0.00` row is unchanged and still the one that matters: it is the
/// only bit-transparent point on the axis.
///
/// The *character* claims are still right, and are unchanged, because a cubic
/// clipper flattens a sine, smears a click and drops the crest factor exactly
/// as Warps' amplifier did.
///
/// # Why a kick and not a tonal voice
///
/// The source has to be *clean* for the drive to be legible. The stage is a
/// saturating waveshaper, so what you hear is the harmonic structure of
/// whatever you feed it — and a six-operator FM voice is dense in partials from
/// the first millisecond. Warping one produces something already complex, and
/// the drive change is masked by the source rather than revealed by it.
///
/// A kick is a pitch-swept sine plus a click: nearly all of its energy sits in
/// one partial, so the shape of the transfer function is most of what you hear.
/// As the drive comes up the sine flattens toward a square, the click smears,
/// and the crest factor falls — three separate symptoms of the same change,
/// which is what makes the sweep readable at a glance.
pub(crate) fn render_warps_drive_demo() -> (Vec<f32>, GateTimeline) {
    use drum_engine::machines::SLOT_LEVEL;
    use drum_engine::machines::SLOT_STRIP_HOLD as WARP_DRV_SLOT;
    use mi_drum_engine::{DeviceEngine, MiMachineId, BLOCK, SAMPLE_RATE as SR};

    mi_drum_engine::seed_random(mi_drum_engine::DEFAULT_RANDOM_SEED);

    let hits = 8usize;
    let step_s = 0.25f32; // 8th notes at 120 BPM
    let gap_s = 0.45f32;
    let per_setting = hits as f32 * step_s + gap_s;

    // (drive macro, what it should sound like)
    let entries: &[(f32, &str)] = &[
        (0.0, "BYPASS - bit transparent, kick uncoloured"),
        (0.1, "light - gain 1.7 into the cubic clipper"),
        (0.25, "unity-ish - gain 2.75, the softest part of the knee"),
        (
            0.5,
            "driven - gain 4.5, most of the kick is inside the clipper",
        ),
        (0.75, "hard - gain 6.25, sine visibly flattening"),
        (1.0, "destroyed - gain 8.0, the clipper is all you hear"),
    ];

    let mut engine = mi_drum_in_place();
    // Headroom. The top of this sweep is loud by design and the point is to hear
    // the drive, not the limiter.
    engine.set_master_gain(0.3);

    // FILT 5 is `WARP.DRV` on mi-drum. Re-exported from `drum_engine` only so
    // this reads next to `SLOT_LEVEL`, which is genuinely per-device.
    {
        let track = &mut engine.tracks_mut()[0];
        track.load_machine(MiMachineId::PeaksBassDrum);
        // The Peaks kick peaks at full scale by design (see
        // docs/peaks-vendoring.md), which would slam Warps' input and hide the
        // transfer curve. Back it off so Warps has somewhere to go.
        track.set_macro(SLOT_LEVEL, 0.5);
    }

    let mut out: Vec<f32> =
        Vec::with_capacity((entries.len() as f32 * per_setting * SR * 2.0) as usize);
    let mut l = [0.0f32; BLOCK];
    let mut r = [0.0f32; BLOCK];
    let mut timeline = GateTimeline::new();
    let blocks = |s: f32| (s * SR / BLOCK as f32) as usize;

    for (i, (drive, why)) in entries.iter().enumerate() {
        engine.tracks_mut()[0].set_macro(WARP_DRV_SLOT, *drive);
        timeline.push((i as f32 * per_setting, format!("WARP.DRV {drive:.2}"), why));

        for hit in 0..hits {
            engine.trigger(0, 1.0);
            // The last hit carries the gap so the settings are separated by
            // silence rather than butting into each other.
            let tail = if hit + 1 == hits {
                step_s + gap_s
            } else {
                step_s
            };
            for _ in 0..blocks(tail) {
                engine.process(&mut l, &mut r);
                append_block(&mut out, &l, &r);
            }
        }
    }

    (out, timeline)
}

/// The shipped kit, twice: with the shaping stage, and with it bypassed.
///
/// Same seed, same pattern, same macros, same two bars, so the only variable
/// is the stage. This used to carry a third pass for `warps::Modulator`; that
/// comparison is in `docs/warps-vendoring.md` and the module is gone.
pub(crate) fn render_shaper_kit_comparison() -> (Vec<f32>, Vec<(f32, String)>) {
    use mi_drum_engine::{DeviceEngine, SAMPLE_RATE as SR};

    let total_blocks = backbeat_blocks();
    let mut out: Vec<f32> = Vec::new();
    let mut notes: Vec<(f32, String)> = Vec::new();

    // Warps is gone; the comparison that remains is the stage against its own
    // bypass, which is the one that says whether it is doing anything.
    //
    // How much to expect from it, because the honest answer is "less than you
    // might hope". `WARP.MIX` is 0.35 on the shipped kit, so a full-scale
    // sample at the strip input moves at most 35% of the way to whatever the
    // stage produced, and the difference between these two passes measures
    // 9.4 dB below pass A — a colouration, not a transformation. Warps at the
    // same macros measured 12.1 dB down, so the replacement is if anything the
    // more present of the two. If you want to hear the stage rather than
    // measure it, `Command::Shaper`'s second file puts it at `WARP.MIX = 1`,
    // full drive, on a pitched sweep.
    let passes: &[(bool, &str)] = &[
        (true, "A - ADAA shaper, kit defaults"),
        (
            false,
            "B - no shaping at all (WARP.DRV 0, bypass), the reference",
        ),
    ];

    for (shaped, label) in passes {
        // Re-seeding per pass rather than once for the file: `stmlib::Random`
        // is a process-global generator, so three passes sharing one seed
        // would have the noise diverge between them and the comparison would
        // be of two different performances.
        mi_drum_engine::seed_random(mi_drum_engine::DEFAULT_RANDOM_SEED);

        // Heap, not stack: `MiDrumEngine` is ~476 KB and returning one by
        // value overflows the main thread's stack. See `mi_drum_in_place`.
        let mut engine = mi_drum_in_place();
        engine.set_master_gain(0.5);
        if !*shaped {
            for t in 0..mi_drum_engine::TRACKS {
                // `SLOT_WARPS_DRIVE`, not `SLOT_STRIP_HOLD`. Same slot index,
                // but the printed label below says `WARP.DRV` and reaching for
                // the generic name next to it reads like the bypass was missed.
                engine.tracks_mut()[t].set_macro(mi_drum_engine::SLOT_WARPS_DRIVE, 0.0);
            }
        }

        notes.push((out.len() as f32 / 2.0 / SR, (*label).to_string()));

        play_backbeat(&mut engine, &mut out, total_blocks);
    }

    (out, notes)
}

/// The stage under the conditions that expose a cheap antialiasing scheme.
///
/// Track 4 of `DEFAULT_KIT` is a Plaits `String` — sustained and pitched,
/// which is the opposite of what drums are and exactly what aliasing shows up
/// on. Driven hard and taken fully wet so nothing is hidden behind
/// `WARP.MIX`, then walked up three octaves a semitone at a time.
///
/// What to listen for: alias partials move *down* as the fundamental moves
/// up, because they are reflections about Nyquist. A stage that stays
/// harmonically coherent across the rise is working; one that grows a
/// descending metallic shadow is not.
pub(crate) fn render_shaper_alias_test() -> (Vec<f32>, Vec<(f32, String)>) {
    use mi_drum_engine::{DeviceEngine, BLOCK, SAMPLE_RATE as SR};

    const MELODIC_TRACK: usize = 4;
    const SEMIS: i32 = 36;
    let note_s = 0.22f32;
    let tail_s = 1.5f32;

    let mut out: Vec<f32> = Vec::new();
    let mut notes: Vec<(f32, String)> = Vec::new();

    // One pass per algorithm. ADAA is first order, so it attenuates aliasing
    // rather than removing it, and the four algorithms do not stress it
    // equally: the crossfade is linear and generates nothing, while the fold
    // is the most harmonically violent thing in the set. If any of them is
    // going to fall apart on a pitched sweep it will be audible here.
    for (algo, label) in [
        (0.0f32, "A - crossfade (linear, nothing to alias)"),
        (1.0 / 3.0, "B - sine fold (the violent one)"),
        (2.0 / 3.0, "C - diode ring (Warps' analog model)"),
        (1.0, "D - digital ring (band-limited modulator)"),
    ] {
        mi_drum_engine::seed_random(mi_drum_engine::DEFAULT_RANDOM_SEED);

        // Heap, not stack: `MiDrumEngine` is ~476 KB and returning one by
        // value overflows the main thread's stack. See `mi_drum_in_place`.
        let mut engine = mi_drum_in_place();
        // Low, because this pass is deliberately driven into the region where
        // both stages are at their most extreme and neither may be allowed to
        // hit the output clipper: a clipped pass generates its own harmonics
        // and the comparison stops being about either shaper. The passes are
        // loudness-matched below instead.
        engine.set_master_gain(0.12);
        {
            let track = &mut engine.tracks_mut()[MELODIC_TRACK];
            // Hard drive, fully wet: the worst case for aliasing.
            track.set_macro(mi_drum_engine::SLOT_STRIP_HOLD, 0.9);
            track.set_macro(mi_drum_engine::SLOT_WARPS_MIX, 1.0);
            track.set_macro(mi_drum_engine::SLOT_FILT_0, algo);
        }
        // Everything else silent: one voice, one variable.
        for t in 0..mi_drum_engine::TRACKS {
            if t != MELODIC_TRACK {
                engine.tracks_mut()[t].set_macro(drum_engine::machines::SLOT_LEVEL, 0.0);
            }
        }

        notes.push((out.len() as f32 / 2.0 / SR, label.to_string()));

        let mut pass: Vec<f32> = Vec::new();
        let mut l = [0.0f32; BLOCK];
        let mut r = [0.0f32; BLOCK];
        let blocks_per_note = (note_s * SR / BLOCK as f32) as usize;

        for semi in 0..=SEMIS {
            engine.tracks_mut()[MELODIC_TRACK].retune(semi as f32);
            engine.trigger(MELODIC_TRACK, 1.0);
            for _ in 0..blocks_per_note {
                engine.process(&mut l, &mut r);
                append_block(&mut pass, &l, &r);
            }
        }
        for _ in 0..((tail_s * SR / BLOCK as f32) as usize) {
            engine.process(&mut l, &mut r);
            append_block(&mut pass, &l, &r);
        }

        // Normalise each pass to a fixed *peak*, not a fixed RMS.
        //
        // RMS matching was right when this file compared two implementations
        // and loudness was the confound. Comparing four algorithms of one
        // stage, it is actively wrong: a plucked string has a ~26 dB crest
        // factor, so scaling it to a common RMS drives the transients into
        // the clamp and every pass comes back reading as clipped. The
        // algorithms are already level-matched to within 8 dB by
        // construction (`the_algorithms_are_level_matched_to_each_other`), so
        // peak-normalising with headroom keeps them comparable and keeps the
        // file honest about what the stage actually does.
        let peak = pass.iter().fold(0.0f32, |m, v| m.max(v.abs()));
        let g = if peak > 1.0e-9 { 0.7 / peak } else { 1.0 };
        out.extend(pass.iter().map(|v| v * g));
    }

    (out, notes)
}
