//! Block-rate wrapper for the Mutable Instruments Peaks drum models.
//!
//! Peaks voices are 16-bit fixed point with a Peaks-local gate-flag byte; both
//! are converted at the shim boundary, so nothing here deals in either. See
//! `docs/peaks-vendoring.md` — in particular the note that the models peak at
//! full scale by design and that they have **no velocity input at all**.

use core::ffi::c_void;
use core::mem::MaybeUninit;

use crate::sys;

#[repr(C)]
struct Storage(MaybeUninit<[u8; sys::MI_PEAKS_STORAGE_SIZE]>);

impl Storage {
    const fn new() -> Self {
        Self(MaybeUninit::uninit())
    }

    fn as_ptr(&mut self) -> *mut c_void {
        self.0.as_mut_ptr().cast()
    }
}

/// One of the four Peaks drum models in this source drop.
///
/// `HighHat` is a fixed sound: Peaks' `HighHat::Configure` is empty upstream, so
/// [`PeaksVoice::configure`] ignores its parameters for that model rather than
/// failing.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum PeaksModel {
    /// Resonant bass drum with a click transient.
    BassDrum,
    /// Two-oscillator snare with a noise "snappy" component.
    SnareDrum,
    /// Six-metallic-oscillator closed hat. No parameters.
    HighHat,
    /// Two-operator FM drum.
    FmDrum,
}

impl PeaksModel {
    const fn wire(self) -> i32 {
        match self {
            Self::BassDrum => sys::MI_PEAKS_MODEL_BASS_DRUM,
            Self::SnareDrum => sys::MI_PEAKS_MODEL_SNARE_DRUM,
            Self::HighHat => sys::MI_PEAKS_MODEL_HIGH_HAT,
            Self::FmDrum => sys::MI_PEAKS_MODEL_FM_DRUM,
        }
    }

    /// Every model, for sweeps and tests.
    pub const ALL: [PeaksModel; 4] = [Self::BassDrum, Self::SnareDrum, Self::HighHat, Self::FmDrum];

    /// Human-readable name, matching `MiMachineId::name` style.
    pub const fn name(self) -> &'static str {
        match self {
            Self::BassDrum => "Bass Drum",
            Self::SnareDrum => "Snare Drum",
            Self::HighHat => "High Hat",
            Self::FmDrum => "FM Drum",
        }
    }
}

/// A single Peaks drum voice, holding whichever model is selected.
pub struct PeaksVoice {
    storage: Storage,
    model: PeaksModel,
}

impl PeaksVoice {
    /// Create a voice on `model`, initialised and ready to render.
    pub fn new(model: PeaksModel) -> Self {
        let mut voice = Self {
            storage: Storage::new(),
            model,
        };
        voice.init();
        voice
    }

    /// (Re-)initialise the voice in place. Also called when the model changes,
    /// because the C++ object is placement-new'd over the same storage.
    pub fn init(&mut self) {
        #[allow(unsafe_code)]
        unsafe {
            sys::mi_peaks_init(self.storage.as_ptr(), self.model.wire());
        }
    }

    /// Which model this voice currently holds.
    pub fn model(&self) -> PeaksModel {
        self.model
    }

    /// Switch model. The voice is re-initialised, so any sound in flight stops.
    pub fn set_model(&mut self, model: PeaksModel) {
        if model != self.model {
            self.model = model;
            self.init();
        }
    }

    /// Set the model's four parameters, each 0..1.
    ///
    /// The mapping is per model and follows Peaks' own front panel:
    ///
    /// | slot | BassDrum | SnareDrum | FmDrum | HighHat |
    /// |---|---|---|---|---|
    /// | 0 | pitch | pitch | pitch | — |
    /// | 1 | punch | tone | FM amount | — |
    /// | 2 | tone | snap | decay | — |
    /// | 3 | decay | decay | noise | — |
    ///
    /// Pitch is centred: 0.5 is the model's default, and the ends reach roughly
    /// ±1 octave for BassDrum/SnareDrum (Peaks takes a signed 16-bit value
    /// there) and the full keyboard range for FmDrum.
    pub fn configure(&mut self, parameters: [f32; 4]) {
        let p = Self::to_q16(self.model, parameters);
        #[allow(unsafe_code)]
        unsafe {
            sys::mi_peaks_configure(self.storage.as_ptr(), self.model.wire(), p.as_ptr());
        }
    }

    /// Quantise the four 0..1 parameters to the model's 16-bit interface.
    fn to_q16(model: PeaksModel, v: [f32; 4]) -> [u16; 4] {
        let at = |i: usize| (v[i].clamp(0.0, 1.0) * 65535.0) as u16;
        match model {
            // BassDrum/SnareDrum read parameter[0] as a *signed* pitch offset
            // (`parameter[0] - 32768`), so it must be centred on 0.5.
            PeaksModel::BassDrum | PeaksModel::SnareDrum => [at(0), at(1), at(2), at(3)],
            // FmDrum reads parameter[0] directly as an absolute MIDI pitch.
            PeaksModel::FmDrum => {
                let pitch = 24.0 + v[0].clamp(0.0, 1.0) * 96.0;
                [(pitch * 128.0) as u16, at(1), at(2), at(3)]
            }
            // HighHat has no parameters upstream.
            PeaksModel::HighHat => [0, 0, 0, 0],
        }
    }

    /// Process one block. `gate_flags` uses the `GATE_*` constants; the shim
    /// translates them to Peaks' own bit values.
    ///
    /// Output is nominally in -1..1 but Peaks deliberately drives its internal
    /// saturation, so peaks sit at or near full scale. That is the sound, not an
    /// overflow — see `docs/peaks-vendoring.md`.
    pub fn process(&mut self, gate_flags: &[u8], out: &mut [f32]) {
        let n = gate_flags.len().min(out.len());
        assert!(n <= 96, "Peaks block size cannot exceed 96");
        if n == 0 {
            return;
        }
        #[allow(unsafe_code)]
        unsafe {
            sys::mi_peaks_process(
                self.storage.as_ptr(),
                self.model.wire(),
                gate_flags.as_ptr(),
                out.as_mut_ptr(),
                n,
            );
        }
    }
}

/// A level below which a Peaks voice counts as silent, in f32.
///
/// Exported so consumers gate on the same number the wrapper's own tests use.
///
/// Peaks is 16-bit fixed point with a saturating `CLIP` at the end of each
/// model, so its tail settles onto a limit-cycle floor rather than true zero:
/// measured at roughly 60-100 int16, about -70 dBFS, depending on parameters.
/// Anything tighter never fires. This clears the floor with room to spare and is
/// far below anything audible.
///
/// The mi-drum slot's own `SILENCE_THRESHOLD` is 1e-6, which a Peaks voice will
/// never reach, so a voice-type-aware slot has to gate more loosely for Peaks
/// or the track would stay active forever.
pub const SILENCE_F32: f32 = 3.0e-3;

#[cfg(test)]
mod tests {
    use super::*;
    use crate::stages::{GATE_HIGH, GATE_LOW, GATE_RISING};
    use std::vec::Vec;

    /// Render one hit — rising gate on the first sample, low after — and return
    /// the peak and the block at which it fell silent. Triggering every block
    /// instead would keep the drum saturated and hide a non-decaying model.
    fn render_hit(model: PeaksModel, blocks: usize) -> (f32, usize) {
        let mut voice = PeaksVoice::new(model);
        voice.configure([0.5, 0.3, 0.5, 0.3]);

        let mut buf = [0.0f32; 96];
        let mut gate = [GATE_LOW; 96];
        gate[0] = GATE_RISING;
        voice.process(&gate, &mut buf);

        let mut peak = 0.0f32;
        let mut silent_at = usize::MAX;
        for b in 1..blocks {
            voice.process(&[GATE_LOW; 96], &mut buf);
            let mut block_peak = 0.0f32;
            for &s in &buf {
                block_peak = block_peak.max(s.abs());
            }
            peak = peak.max(block_peak);
            if silent_at == usize::MAX && block_peak < SILENCE_F32 {
                silent_at = b;
            }
        }
        (peak, silent_at)
    }

    #[test]
    fn peaks_creates() {
        for model in PeaksModel::ALL {
            let _voice = PeaksVoice::new(model);
        }
    }

    /// Every model must make sound, stay finite, and decay. A model that never
    /// goes quiet would hold a track active forever.
    #[test]
    fn every_model_sounds_and_decays() {
        for model in PeaksModel::ALL {
            let (peak, silent_at) = render_hit(model, 400);
            assert!(peak > 0.05, "{} was silent (peak {peak})", model.name());
            assert!(peak <= 1.0, "{} exceeded unity (peak {peak})", model.name());
            assert!(
                silent_at != usize::MAX,
                "{} never decayed to silence",
                model.name()
            );
        }
    }

    /// Output must stay finite at the extremes of every parameter, since Peaks
    /// saturates internally and a bad mapping could feed it a huge value.
    #[test]
    fn extreme_parameters_stay_finite() {
        for model in PeaksModel::ALL {
            for &v in &[0.0f32, 1.0] {
                let mut voice = PeaksVoice::new(model);
                voice.configure([v; 4]);
                let mut buf = [0.0f32; 96];
                let mut gate = [GATE_LOW; 96];
                gate[0] = GATE_RISING;
                for _ in 0..20 {
                    voice.process(&gate, &mut buf);
                    for &s in &buf {
                        assert!(s.is_finite(), "{} produced {s}", model.name());
                    }
                    gate = [GATE_HIGH; 96];
                    voice.process(&gate, &mut buf);
                }
            }
        }
    }

    /// Switching model must re-initialise: the new model has to sound, not
    /// inherit the old one's state.
    #[test]
    fn switching_model_reinitialises() {
        let mut voice = PeaksVoice::new(PeaksModel::BassDrum);
        voice.configure([0.5, 0.3, 0.5, 0.3]);
        let mut buf = [0.0f32; 96];
        let mut gate = [GATE_LOW; 96];
        gate[0] = GATE_RISING;
        voice.process(&gate, &mut buf);

        voice.set_model(PeaksModel::FmDrum);
        assert_eq!(voice.model(), PeaksModel::FmDrum);
        voice.configure([0.5, 0.3, 0.5, 0.3]);
        let mut hit = 0.0f32;
        for _ in 0..40 {
            voice.process(&gate, &mut buf);
            for &s in &buf {
                hit = hit.max(s.abs());
            }
        }
        assert!(hit > 0.01, "FM drum silent after switching from bass drum");
    }

    /// The gate convention must actually reach the models. Peaks' own
    /// `GATE_FLAG_RISING` is 2, and a translation slip here is exactly the bug
    /// documented in docs/peaks-vendoring.md, so pin it: with no rising edge
    /// anywhere, nothing may sound.
    #[test]
    fn without_a_rising_edge_nothing_sounds() {
        let mut voice = PeaksVoice::new(PeaksModel::BassDrum);
        voice.configure([0.5, 0.3, 0.5, 0.3]);
        let mut buf = [0.0f32; 96];
        let mut peak = 0.0f32;
        for _ in 0..200 {
            voice.process(&[GATE_LOW; 96], &mut buf);
            for &s in &buf {
                peak = peak.max(s.abs());
            }
        }
        assert!(
            peak < 1.0e-6,
            "gate translation is wrong: {peak} without a trigger"
        );
    }

    /// Sanity check that the models are actually different from each other.
    #[test]
    fn models_are_distinct() {
        let mut outs: Vec<Vec<f32>> = Vec::new();
        for model in PeaksModel::ALL {
            let mut voice = PeaksVoice::new(model);
            voice.configure([0.5, 0.3, 0.5, 0.3]);
            let mut buf = [0.0f32; 96];
            let mut gate = [GATE_LOW; 96];
            gate[0] = GATE_RISING;
            voice.process(&gate, &mut buf);
            let mut v: Vec<f32> = buf.to_vec();
            for _ in 0..20 {
                voice.process(&[GATE_LOW; 96], &mut buf);
                v.extend_from_slice(&buf);
            }
            outs.push(v);
        }
        for i in 1..outs.len() {
            let delta = outs[0]
                .iter()
                .zip(outs[i].iter())
                .map(|(a, b)| (a - b).abs())
                .fold(0.0f32, f32::max);
            assert!(delta > 1.0e-3, "model {i} sounds identical to BassDrum");
        }
    }
}
