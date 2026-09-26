use std::env;
use std::path::PathBuf;

fn main() {
    let target = env::var("TARGET").unwrap();
    let out_dir = PathBuf::from(env::var("OUT_DIR").unwrap());
    let manifest_dir = PathBuf::from(env::var("CARGO_MANIFEST_DIR").unwrap());
    let vendor = manifest_dir.join("vendor");

    let mut build = cc::Build::new();
    build.cpp(true);
    build.include(manifest_dir.join("include"));
    build.include(&vendor);
    build.include(vendor.join("stmlib"));
    build.include(vendor.join("plaits"));
    build.include(vendor.join("warps"));
    build.include(vendor.join("stages"));
    build.include(vendor.join("clouds"));
    build.include(vendor.join("tides2"));

    // Host or cross-compiled ARM Cortex-M7.
    if target == "thumbv7em-none-eabihf" {
        // Prefer the ARM GNU toolchain; fall back to clang with a target
        // triple if it is not installed.
        if env::var("MI_DSP_CXX").is_err() {
            if which::which("arm-none-eabi-g++").is_ok() {
                env::set_var("MI_DSP_CXX", "arm-none-eabi-g++");
            } else if which::which("clang++").is_ok() {
                env::set_var("MI_DSP_CXX", "clang++");
            } else {
                panic!(
                    "thumbv7em-none-eabihf build requires arm-none-eabi-g++ or clang++. \
                     Install one or set MI_DSP_CXX."
                );
            }
        }
        build.compiler(env::var("MI_DSP_CXX").unwrap());
        build.flag_if_supported("--target=thumbv7em-none-eabihf");
        build.flag_if_supported("-mcpu=cortex-m7");
        // cc-rs injects `-march=armv7e-m` ahead of our flags, and an explicit
        // `-march` takes the architecture decision away from `-mcpu` — the
        // object comes out tagged `Tag_CPU_name: "7E-M"` rather than
        // Cortex-M7. The FPU and ABI still land correctly (verified via
        // `readelf -A`: FPv5/FP-D16, VFP registers), but the scheduling model
        // is the part worth having: the Rust side measured 1.1% from
        // `-C target-cpu=cortex-m7`, and that gain is invisible in an
        // instruction census, so state it explicitly rather than hope.
        build.flag_if_supported("-mtune=cortex-m7");
        build.flag_if_supported("-mfloat-abi=hard");
        build.flag_if_supported("-mfpu=fpv5-d16");
        build.flag_if_supported("-mthumb");

        // Do not link a C++ standard library. cc-rs defaults to `-lstdc++`,
        // which does not exist bare-metal and which we do not want anyway: the
        // vendored code is built `-fno-exceptions -fno-rtti`, allocates
        // nothing, and uses `new` only in its placement form. It links with no
        // undefined C++ ABI symbols at all — not even `__cxa_pure_virtual`,
        // since every virtual in the Plaits engine hierarchy is implemented.
        build.cpp_link_stdlib(None);
    }

    // No exceptions, no RTTI, no fast-math: keep float behaviour deterministic
    // and close to the Rust side.
    build.flag_if_supported("-fno-exceptions");
    build.flag_if_supported("-fno-rtti");
    build.flag_if_supported("-ffp-contract=off");
    build.flag_if_supported("-Wno-unused-parameter");
    // clang spells this singular, GCC plural. `flag_if_supported` probes each,
    // so passing both silences the vendored `STATIC_ASSERT` macro on either
    // compiler instead of only on clang.
    build.flag_if_supported("-Wno-unused-local-typedef");
    build.flag_if_supported("-Wno-unused-local-typedefs");
    // Stages uses a variable-length array in ProcessOscillator; it is bounded by
    // the caller's block size. Silence the Clang extension warning.
    build.flag_if_supported("-Wno-vla-cxx-extension");
    build.flag_if_supported("-Wno-vla");
    build.opt_level(3);

    // On host targets the vendored code uses ARM inline assembly unless TEST is
    // defined. Define it so unit tests and benches run on x86_64 / arm64.
    if target != "thumbv7em-none-eabihf" {
        build.define("TEST", None);
    }

    // Plaits voice shim and the voice implementation it wraps.
    build.file(manifest_dir.join("src/mi_dsp_shim.cc"));
    build.file(manifest_dir.join("src/mi_warps_shim.cc"));
    build.file(manifest_dir.join("src/mi_stages_shim.cc"));
    build.file(manifest_dir.join("src/mi_clouds_shim.cc"));

    // Engine set 1.
    for name in [
        "additive",
        "bass_drum",
        "chord",
        "fm",
        "grain",
        "hi_hat",
        "modal",
        "noise",
        "particle",
        "snare_drum",
        "speech",
        "string",
        "swarm",
        "virtual_analog",
        "waveshaping",
        "wavetable",
    ] {
        build.file(vendor.join(format!("plaits/dsp/engine/{name}_engine.cc")));
    }

    // Engine set 2.
    for name in [
        "chiptune",
        "phase_distortion",
        "six_op",
        "string_machine",
        "virtual_analog_vcf",
        "wave_terrain",
    ] {
        build.file(vendor.join(format!("plaits/dsp/engine2/{name}_engine.cc")));
    }

    // Shared Plaits DSP sources and lookup tables.
    build.file(vendor.join("plaits/dsp/voice.cc"));
    build.file(vendor.join("plaits/resources.cc"));

    // Physical modelling, FM, chord and speech helpers.
    build.file(vendor.join("plaits/dsp/chords/chord_bank.cc"));
    for name in ["algorithms", "dx_units"] {
        build.file(vendor.join(format!("plaits/dsp/fm/{name}.cc")));
    }
    for name in ["modal_voice", "resonator", "string_voice", "string"] {
        build.file(vendor.join(format!("plaits/dsp/physical_modelling/{name}.cc")));
    }
    for name in [
        "lpc_speech_synth_controller",
        "lpc_speech_synth_phonemes",
        "lpc_speech_synth_words",
        "lpc_speech_synth",
        "naive_speech_synth",
        "sam_speech_synth",
    ] {
        build.file(vendor.join(format!("plaits/dsp/speech/{name}.cc")));
    }

    // stmlib utilities used by Plaits.
    build.file(vendor.join("stmlib/dsp/units.cc"));
    build.file(vendor.join("stmlib/dsp/atan.cc"));
    build.file(vendor.join("stmlib/utils/random.cc"));
    // Note: user_data_receiver.cc is not compiled because it depends on the
    // stm_audio_bootloader library which we do not vendor.

    // NOTE: `peaks/` is vendored and complete as of 2026-09-26, but still
    // deliberately NOT compiled. The Rust wrapper does not exist yet, and
    // pulling the C++ in now would add ~40 KB of dead `wav_digits` to the
    // image for nothing. Phase 14.4 adds both together. See
    // docs/peaks-vendoring.md for provenance and two integration traps (the
    // GateFlags type is Peaks-local, and the raw output needs a trim).

    // Warps meta-modulator.
    for name in ["oscillator", "modulator", "vocoder", "filter_bank"] {
        build.file(vendor.join(format!("warps/dsp/{name}.cc")));
    }
    build.file(vendor.join("warps/resources.cc"));

    // Stages segment generator / LFO / envelope.
    build.file(vendor.join("stages/segment_generator.cc"));
    build.file(vendor.join("stages/resources.cc"));
    build.file(vendor.join("tides2/ramp/ramp_extractor.cc"));

    // Clouds texture synthesizer.
    build.file(vendor.join("clouds/resources.cc"));
    build.file(vendor.join("clouds/dsp/granular_processor.cc"));
    build.file(vendor.join("clouds/dsp/correlator.cc"));
    build.file(vendor.join("clouds/dsp/mu_law.cc"));
    for name in ["phase_vocoder", "frame_transformation", "stft"] {
        build.file(vendor.join(format!("clouds/dsp/pvoc/{name}.cc")));
    }

    build.compile("mi_dsp");

    // Tell cargo to rerun if vendored sources or the shim change.
    println!("cargo:rerun-if-changed=src/mi_dsp_shim.cc");
    println!("cargo:rerun-if-changed=src/mi_warps_shim.cc");
    println!("cargo:rerun-if-changed=src/mi_stages_shim.cc");
    println!("cargo:rerun-if-changed=src/mi_clouds_shim.cc");
    println!("cargo:rerun-if-changed=include/mi_dsp_shim.h");
    println!("cargo:rerun-if-changed=vendor");

    // Place a dummy marker so downstream crates can detect mi-dsp presence.
    std::fs::write(out_dir.join("mi_dsp_built"), b"").unwrap();
}

// Minimal `which` clone so we don't add a dependency just for toolchain
// probing. Copied inline to keep build-dependencies tiny.
mod which {
    use std::path::Path;

    pub fn which(name: &str) -> Result<std::path::PathBuf, ()> {
        let path = std::env::var_os("PATH").ok_or(())?;
        for dir in std::env::split_paths(&path) {
            let candidate = dir.join(name);
            if is_executable(&candidate) {
                return Ok(candidate);
            }
        }
        Err(())
    }

    #[cfg(unix)]
    fn is_executable(path: &Path) -> bool {
        use std::os::unix::prelude::*;
        std::fs::metadata(path)
            .map(|m| m.is_file() && m.permissions().mode() & 0o111 != 0)
            .unwrap_or(false)
    }

    #[cfg(windows)]
    fn is_executable(path: &Path) -> bool {
        std::fs::metadata(path)
            .map(|m| m.is_file())
            .unwrap_or(false)
    }
}
