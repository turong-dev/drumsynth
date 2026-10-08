use drum_engine::{BLOCK, SAMPLE_RATE};

/// Append one processed stereo block to an interleaved output buffer.
pub(crate) fn append_block(out: &mut Vec<f32>, l: &[f32; BLOCK], r: &[f32; BLOCK]) {
    for k in 0..BLOCK {
        out.push(l[k]);
        out.push(r[k]);
    }
}

/// Write interleaved stereo f32 to a 24-bit WAV.
///
/// Samples outside ±1.0 are clamped and counted; non-finite samples are
/// counted separately (they land at 0 on the saturating float→int cast), so
/// a NaN from the engine cannot pass for silence without saying so.
pub(crate) fn write_wav(path: &str, interleaved: &[f32]) -> Result<(), Box<dyn std::error::Error>> {
    let spec = hound::WavSpec {
        channels: 2,
        sample_rate: SAMPLE_RATE as u32,
        bits_per_sample: 24,
        sample_format: hound::SampleFormat::Int,
    };

    let mut writer = hound::WavWriter::create(path, spec)?;
    let scale = (1i32 << 23) as f32 - 1.0;

    let mut clipped = 0usize;
    let mut non_finite = 0usize;
    for &s in interleaved {
        if !s.is_finite() {
            non_finite += 1;
        } else if s.abs() > 1.0 {
            clipped += 1;
        }
        writer.write_sample((s.clamp(-1.0, 1.0) * scale) as i32)?;
    }
    writer.finalize()?;

    if clipped > 0 {
        eprintln!("warning: {clipped} samples clipped — the engine should not allow this");
    }
    if non_finite > 0 {
        eprintln!(
            "warning: {non_finite} non-finite samples (NaN/inf) written as 0 — \
             the engine should not allow this"
        );
    }
    Ok(())
}
