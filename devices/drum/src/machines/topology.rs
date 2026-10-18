//! Signal paths for the drum machines.
//!
//! One [`Topology`] per machine, plus [`STRIP`] for the track strip every
//! machine feeds. The split matters: the voice differs per machine, the
//! strip does not, and repeating filter → amp → sends fifteen times is
//! noise. Each machine's graph therefore ends at a single `out` node, which
//! is the strip's input.
//!
//! The graphs are read off the voices' `tick` functions. They are checked
//! against the parameter tables — see the tests at the bottom of this file —
//! so a parameter added to a machine and forgotten here fails the build.

use device_core::topology::{node, node_p, Edge, ModRoute, NodeKind, Topology};

use super::MachineId;

/// Where every machine's `out` node leads: the shared track strip.
///
/// Order is `Track::tick`'s, which is not the order the struct field list
/// suggests — the amp envelope runs first, then drive, then the filter.
pub static STRIP: Topology = Topology {
    summary: "Every machine's output runs through the same track strip: amp \
              envelope, drive, filter, level, pan, then the two sends.",
    nodes: &[
        node("voice", "Machine output", NodeKind::Source),
        node_p(
            "ampenv",
            "AHD amp env",
            NodeKind::Env,
            &["STRIP.ATK", "STRIP.HOLD", "STRIP.DEC"],
        ),
        node("vca", "VCA", NodeKind::Mix),
        node("drive", "Soft clip", NodeKind::Shaper),
        node_p(
            "filter",
            "Strip SVF",
            NodeKind::Filter,
            &["STRIP.CUT", "STRIP.RESO"],
        ),
        node_p("level", "Level", NodeKind::Mix, &["LEVEL"]),
        node_p("pan", "Pan", NodeKind::Mix, &["PAN"]),
        node_p("out", "Output pair", NodeKind::Out, &["OUT"]),
        node_p(
            "sends",
            "Delay / reverb sends",
            NodeKind::Out,
            &["SEND.DLY", "SEND.RVB"],
        ),
    ],
    edges: &[
        Edge::new("voice", "vca"),
        Edge::labelled("ampenv", "vca", "amp"),
        Edge::new("vca", "drive"),
        Edge::new("drive", "filter"),
        Edge::new("filter", "level"),
        Edge::new("level", "pan"),
        Edge::new("pan", "out"),
        Edge::labelled("level", "sends", "post-fader"),
    ],
    mod_routes: &[
        ModRoute {
            source: "LFO 1",
            dest: "Selectable: machine params 0-7, strip cutoff, resonance, \
                   drive, pan, level, amp decay, delay send, reverb send",
            params: &["LFO1.RATE", "LFO1.DEP", "LFO1.DST"],
        },
        ModRoute {
            source: "LFO 2",
            dest: "Selectable: the same sixteen destinations as LFO 1",
            params: &["LFO2.RATE", "LFO2.DEP", "LFO2.DST"],
        },
    ],
};

/// The signal path for `machine`.
pub fn topology(machine: MachineId) -> &'static Topology {
    match machine {
        MachineId::BdClassic => &BD_CLASSIC,
        MachineId::BdFm => &BD_FM,
        MachineId::BdVa => &BD_VA,
        MachineId::Tom => &TOM,
        MachineId::SdNatural => &SD_NATURAL,
        MachineId::SdFm => &SD_FM,
        MachineId::Rs => &RS,
        MachineId::Cp => &CP,
        MachineId::HatClassic => &HAT_CLASSIC,
        MachineId::HhBasic => &HH_BASIC,
        MachineId::CyMetallic => &CY_METALLIC,
        MachineId::CbClassic => &CB_CLASSIC,
        MachineId::SyTone => &SY_TONE,
        MachineId::DubSiren => &DUB_SIREN,
        MachineId::SweepFx => &SWEEP_FX,
    }
}

/// The handoff node every machine graph ends on.
const OUT: &str = "out";

static BD_CLASSIC: Topology = Topology {
    summary: "A sine whose pitch drops from a click onto a steady fundamental, \
              under an amp envelope, then driven into saturation for weight.",
    nodes: &[
        node_p("pitchenv", "Pitch env", NodeKind::Env, &["SWEEP", "SWP_T"]),
        node_p("osc", "Sine", NodeKind::Source, &["TUNE"]),
        node_p("ampenv", "Amp env", NodeKind::Env, &["DEC"]),
        node("vca", "VCA", NodeKind::Mix),
        node_p("drive", "Soft clip", NodeKind::Shaper, &["DRIVE"]),
        node(OUT, "To strip", NodeKind::Out),
    ],
    edges: &[
        Edge::labelled("pitchenv", "osc", "pitch"),
        Edge::new("osc", "vca"),
        Edge::labelled("ampenv", "vca", "amp"),
        Edge::new("vca", "drive"),
        Edge::new("drive", OUT),
    ],
    mod_routes: &[],
};

static BD_FM: Topology = Topology {
    summary: "A pitch-swept carrier phase-modulated by a second sine, whose \
              depth has its own decay envelope.",
    nodes: &[
        node_p("pitchenv", "Pitch env", NodeKind::Env, &["SWEEP", "SWP_T"]),
        node_p("modenv", "Mod env", NodeKind::Env, &["MOD.DC"]),
        node_p(
            "modosc",
            "Modulator sine",
            NodeKind::Modulator,
            &["MOD.HZ", "MOD.AMT"],
        ),
        node_p("carrier", "Carrier sine", NodeKind::Source, &["TUNE"]),
        node_p("ampenv", "Amp env", NodeKind::Env, &["DEC"]),
        node("vca", "VCA", NodeKind::Mix),
        node("drive", "Soft clip", NodeKind::Shaper),
        node(OUT, "To strip", NodeKind::Out),
    ],
    edges: &[
        Edge::labelled("modenv", "modosc", "depth"),
        Edge::labelled("modosc", "carrier", "FM"),
        Edge::labelled("pitchenv", "carrier", "pitch"),
        Edge::new("carrier", "vca"),
        Edge::labelled("ampenv", "vca", "amp"),
        Edge::new("vca", "drive"),
        Edge::new("drive", OUT),
    ],
    mod_routes: &[],
};

static BD_VA: Topology = Topology {
    summary: "A bridged-T resonator pinged by the envelope rather than an \
              oscillator gated by it — the analogue-circuit model.",
    nodes: &[
        node_p("pitchenv", "Pitch env", NodeKind::Env, &["SWEEP", "SWP_T"]),
        node_p("exciter", "Excitation env", NodeKind::Env, &["DEC"]),
        node_p(
            "reson",
            "Bridged-T resonator",
            NodeKind::Filter,
            &["TUNE", "Q"],
        ),
        node(OUT, "To strip", NodeKind::Out),
    ],
    edges: &[
        Edge::labelled("exciter", "reson", "ping"),
        Edge::labelled("pitchenv", "reson", "pitch"),
        Edge::new("reson", OUT),
    ],
    mod_routes: &[],
};

static TOM: Topology = Topology {
    summary: "A pitch-swept sine body with a highpassed-noise stick click \
              mixed over it. No drive — the body stays clean.",
    nodes: &[
        node_p("pitchenv", "Pitch env", NodeKind::Env, &["SWEEP", "SWP_T"]),
        node_p("osc", "Sine", NodeKind::Source, &["TUNE"]),
        node_p("ampenv", "Amp env", NodeKind::Env, &["DEC"]),
        node("bodyvca", "Body VCA", NodeKind::Mix),
        node("noise", "Noise", NodeKind::Source),
        node("hp", "Highpass", NodeKind::Filter),
        node_p("stickenv", "Stick env", NodeKind::Env, &["STICK"]),
        node("stickvca", "Stick VCA", NodeKind::Mix),
        node("sum", "Sum", NodeKind::Mix),
        node(OUT, "To strip", NodeKind::Out),
    ],
    edges: &[
        Edge::labelled("pitchenv", "osc", "pitch"),
        Edge::new("osc", "bodyvca"),
        Edge::labelled("ampenv", "bodyvca", "amp"),
        Edge::new("noise", "hp"),
        Edge::new("hp", "stickvca"),
        Edge::labelled("stickenv", "stickvca", "amp"),
        Edge::new("bodyvca", "sum"),
        Edge::new("stickvca", "sum"),
        Edge::new("sum", OUT),
    ],
    mod_routes: &[],
};

static SD_NATURAL: Topology = Topology {
    summary: "Two detuned body sines — the shell — crossfaded against a \
              highpassed noise burst, the wires. NMIX is the crossfade.",
    nodes: &[
        node_p("oscA", "Body sine A", NodeKind::Source, &["TUNE"]),
        node_p("oscB", "Body sine B", NodeKind::Source, &["RATIO"]),
        node_p("bodyenv", "Body env", NodeKind::Env, &["BDEC"]),
        node("bodyvca", "Body VCA", NodeKind::Mix),
        node("noise", "Noise", NodeKind::Source),
        node_p("hp", "Highpass", NodeKind::Filter, &["HPF"]),
        node_p("noiseenv", "Noise env", NodeKind::Env, &["NDEC"]),
        node("noisevca", "Noise VCA", NodeKind::Mix),
        node_p("mix", "Body / rattle mix", NodeKind::Mix, &["NMIX"]),
        node("drive", "Soft clip", NodeKind::Shaper),
        node(OUT, "To strip", NodeKind::Out),
    ],
    edges: &[
        Edge::new("oscA", "bodyvca"),
        Edge::new("oscB", "bodyvca"),
        Edge::labelled("bodyenv", "bodyvca", "amp"),
        Edge::new("noise", "hp"),
        Edge::new("hp", "noisevca"),
        Edge::labelled("noiseenv", "noisevca", "amp"),
        Edge::new("bodyvca", "mix"),
        Edge::new("noisevca", "mix"),
        Edge::new("mix", "drive"),
        Edge::new("drive", OUT),
    ],
    mod_routes: &[],
};

static SD_FM: Topology = Topology {
    summary: "The SD Natural layout with an FM body instead of two sines: a \
              carrier phase-modulated by a ratio-locked operator.",
    nodes: &[
        node_p("modenv", "Mod env", NodeKind::Env, &["MENV"]),
        node_p(
            "modosc",
            "Modulator",
            NodeKind::Modulator,
            &["RAT", "MOD.AMT"],
        ),
        node_p("carrier", "Carrier sine", NodeKind::Source, &["TUNE"]),
        node_p("bodyenv", "Body env", NodeKind::Env, &["BDEC"]),
        node("bodyvca", "Body VCA", NodeKind::Mix),
        node("noise", "Noise", NodeKind::Source),
        node("hp", "Highpass", NodeKind::Filter),
        node_p("noiseenv", "Noise env", NodeKind::Env, &["NDEC"]),
        node("noisevca", "Noise VCA", NodeKind::Mix),
        node_p("mix", "Body / rattle mix", NodeKind::Mix, &["NMIX"]),
        node("drive", "Soft clip", NodeKind::Shaper),
        node(OUT, "To strip", NodeKind::Out),
    ],
    edges: &[
        Edge::labelled("modenv", "modosc", "depth"),
        Edge::labelled("modosc", "carrier", "FM"),
        Edge::new("carrier", "bodyvca"),
        Edge::labelled("bodyenv", "bodyvca", "amp"),
        Edge::new("noise", "hp"),
        Edge::new("hp", "noisevca"),
        Edge::labelled("noiseenv", "noisevca", "amp"),
        Edge::new("bodyvca", "mix"),
        Edge::new("noisevca", "mix"),
        Edge::new("mix", "drive"),
        Edge::new("drive", OUT),
    ],
    mod_routes: &[],
};

static RS: Topology = Topology {
    summary: "Two detuned sines saturated into a rim-like edge, with a \
              highpassed noise tick laid over the top.",
    nodes: &[
        node_p("oscA", "Sine A", NodeKind::Source, &["TUNE"]),
        node_p("oscB", "Sine B", NodeKind::Source, &["DET"]),
        node("pair", "Sum", NodeKind::Mix),
        node("sat", "Soft clip", NodeKind::Shaper),
        node_p("bodyenv", "Body env", NodeKind::Env, &["DEC"]),
        node("bodyvca", "Body VCA", NodeKind::Mix),
        node("noise", "Noise", NodeKind::Source),
        node_p("hp", "Highpass", NodeKind::Filter, &["HPF"]),
        node_p("noiseenv", "Tick env", NodeKind::Env, &["NDEC"]),
        node("noisevca", "Tick VCA", NodeKind::Mix),
        node_p("mix", "Sum", NodeKind::Mix, &["NLEV"]),
        node(OUT, "To strip", NodeKind::Out),
    ],
    edges: &[
        Edge::new("oscA", "pair"),
        Edge::new("oscB", "pair"),
        Edge::new("pair", "sat"),
        Edge::new("sat", "bodyvca"),
        Edge::labelled("bodyenv", "bodyvca", "amp"),
        Edge::new("noise", "hp"),
        Edge::new("hp", "noisevca"),
        Edge::labelled("noiseenv", "noisevca", "amp"),
        Edge::new("bodyvca", "mix"),
        Edge::new("noisevca", "mix"),
        Edge::new("mix", OUT),
    ],
    mod_routes: &[],
};

static CP: Topology = Topology {
    summary: "Bandpassed noise under a burst envelope — the hand claps — \
              balanced against a short body tone.",
    nodes: &[
        node("noise", "Noise", NodeKind::Source),
        node_p("hp", "Highpass", NodeKind::Filter, &["HPF"]),
        node_p("lp", "Lowpass", NodeKind::Filter, &["LPF"]),
        node_p("burstenv", "Burst env", NodeKind::Env, &["NDEC"]),
        node("noisevca", "Noise VCA", NodeKind::Mix),
        node_p("body", "Body tone", NodeKind::Source, &["TUNE", "RATIO"]),
        node_p("bodyenv", "Body env", NodeKind::Env, &["BDEC"]),
        node("bodyvca", "Body VCA", NodeKind::Mix),
        node_p("mix", "Balance", NodeKind::Mix, &["BAL"]),
        node("drive", "Soft clip", NodeKind::Shaper),
        node(OUT, "To strip", NodeKind::Out),
    ],
    edges: &[
        Edge::new("noise", "hp"),
        Edge::new("hp", "lp"),
        Edge::new("lp", "noisevca"),
        Edge::labelled("burstenv", "noisevca", "bursts"),
        Edge::new("body", "bodyvca"),
        Edge::labelled("bodyenv", "bodyvca", "amp"),
        Edge::new("noisevca", "mix"),
        Edge::new("bodyvca", "mix"),
        Edge::new("mix", "drive"),
        Edge::new("drive", OUT),
    ],
    mod_routes: &[],
};

static HAT_CLASSIC: Topology = Topology {
    summary: "Bandpassed noise under one decay envelope. The simplest voice \
              in the catalogue — the filter is the whole sound.",
    nodes: &[
        node("noise", "Noise", NodeKind::Source),
        node_p("hp", "Highpass", NodeKind::Filter, &["HPF"]),
        node_p("lp", "Lowpass", NodeKind::Filter, &["LPF"]),
        node_p("env", "Decay env", NodeKind::Env, &["DEC"]),
        node("vca", "VCA", NodeKind::Mix),
        node(OUT, "To strip", NodeKind::Out),
    ],
    edges: &[
        Edge::new("noise", "hp"),
        Edge::new("hp", "lp"),
        Edge::new("lp", "vca"),
        Edge::labelled("env", "vca", "amp"),
        Edge::new("vca", OUT),
    ],
    mod_routes: &[],
};

static HH_BASIC: Topology = Topology {
    summary: "Six detuned square oscillators summed and bandpassed — the \
              classic metallic hat — with a transient accent on top.",
    nodes: &[
        node_p(
            "oscs",
            "Six square oscs",
            NodeKind::Source,
            &["TUNE", "TONE", "RST"],
        ),
        node_p("bpf", "Bandpass", NodeKind::Filter, &["BPF"]),
        node_p("env", "Decay env", NodeKind::Env, &["DEC"]),
        node_p("tenv", "Transient env", NodeKind::Env, &["TDEC"]),
        node("vca", "VCA", NodeKind::Mix),
        node(OUT, "To strip", NodeKind::Out),
    ],
    edges: &[
        Edge::new("oscs", "bpf"),
        Edge::new("bpf", "vca"),
        Edge::labelled("env", "vca", "amp"),
        Edge::labelled("tenv", "vca", "accent"),
        Edge::new("vca", OUT),
    ],
    mod_routes: &[],
};

static CY_METALLIC: Topology = Topology {
    summary: "Two sines ring-modulated into inharmonic partials — the cymbal \
              — with a highpassed noise stick hit over it.",
    nodes: &[
        node_p("oscA", "Sine A", NodeKind::Source, &["TUNE"]),
        node_p("oscB", "Sine B", NodeKind::Source, &["TONE"]),
        node("ring", "Ring mod", NodeKind::Mix),
        node_p("env", "Decay env", NodeKind::Env, &["DEC"]),
        node("vca", "VCA", NodeKind::Mix),
        node("noise", "Noise", NodeKind::Source),
        node_p("hp", "Highpass", NodeKind::Filter, &["NCOL"]),
        node_p("tenv", "Transient env", NodeKind::Env, &["TDEC"]),
        node("stickvca", "Stick VCA", NodeKind::Mix),
        node("sum", "Sum", NodeKind::Mix),
        node("drive", "Soft clip", NodeKind::Shaper),
        node(OUT, "To strip", NodeKind::Out),
    ],
    edges: &[
        Edge::labelled("oscA", "ring", "carrier"),
        Edge::labelled("oscB", "ring", "modulator"),
        Edge::new("ring", "vca"),
        Edge::labelled("env", "vca", "amp"),
        Edge::new("noise", "hp"),
        Edge::new("hp", "stickvca"),
        Edge::labelled("tenv", "stickvca", "amp"),
        Edge::new("vca", "sum"),
        Edge::new("stickvca", "sum"),
        Edge::new("sum", "drive"),
        Edge::new("drive", OUT),
    ],
    mod_routes: &[],
};

static CB_CLASSIC: Topology = Topology {
    summary: "Two detuned squares summed and bandpassed — the cowbell \
              interval is the detune.",
    nodes: &[
        node_p("sqA", "Square A", NodeKind::Source, &["TUNE"]),
        node_p("sqB", "Square B", NodeKind::Source, &["DET"]),
        node("sum", "Sum", NodeKind::Mix),
        node_p("bpf", "Bandpass", NodeKind::Filter, &["BPF"]),
        node_p("env", "Decay env", NodeKind::Env, &["DEC"]),
        node("vca", "VCA", NodeKind::Mix),
        node(OUT, "To strip", NodeKind::Out),
    ],
    edges: &[
        Edge::new("sqA", "sum"),
        Edge::new("sqB", "sum"),
        Edge::new("sum", "bpf"),
        Edge::new("bpf", "vca"),
        Edge::labelled("env", "vca", "amp"),
        Edge::new("vca", OUT),
    ],
    mod_routes: &[],
};

static SY_TONE: Topology = Topology {
    summary: "A two-operator FM voice with modulator feedback — the \
              synth-tone machine, closer to a synth voice than a drum.",
    nodes: &[
        node_p("modenv", "Mod env", NodeKind::Env, &["MENV"]),
        node_p(
            "modosc",
            "Modulator",
            NodeKind::Modulator,
            &["RATIO", "MOD.AMT", "FDBK"],
        ),
        node_p("carrier", "Carrier sine", NodeKind::Source, &["TUNE"]),
        node_p("ampenv", "Amp env", NodeKind::Env, &["DEC"]),
        node("vca", "VCA", NodeKind::Mix),
        node("drive", "Soft clip", NodeKind::Shaper),
        node(OUT, "To strip", NodeKind::Out),
    ],
    edges: &[
        Edge::labelled("modenv", "modosc", "depth"),
        Edge::labelled("modosc", "modosc", "feedback"),
        Edge::labelled("modosc", "carrier", "FM"),
        Edge::new("carrier", "vca"),
        Edge::labelled("ampenv", "vca", "amp"),
        Edge::new("vca", "drive"),
        Edge::new("drive", OUT),
    ],
    mod_routes: &[],
};

static DUB_SIREN: Topology = Topology {
    summary: "A sine whose pitch is swept by an audio-rate internal LFO, \
              gated by a held AHD envelope. A gesture, not a hit.",
    nodes: &[
        node_p(
            "lfo",
            "Internal LFO",
            NodeKind::Modulator,
            &["RATE", "SHAPE"],
        ),
        node_p("depth", "Pitch depth", NodeKind::Mix, &["DEPTH"]),
        node_p("osc", "Sine", NodeKind::Source, &["TUNE"]),
        node_p("env", "Gated AHD env", NodeKind::Env, &["DEC"]),
        node("vca", "VCA", NodeKind::Mix),
        node(OUT, "To strip", NodeKind::Out),
    ],
    edges: &[
        Edge::new("lfo", "depth"),
        Edge::labelled("depth", "osc", "pitch"),
        Edge::new("osc", "vca"),
        Edge::labelled("env", "vca", "amp"),
        Edge::new("vca", OUT),
    ],
    mod_routes: &[],
};

static SWEEP_FX: Topology = Topology {
    summary: "Noise through a resonant filter whose cutoff an internal LFO \
              sweeps — risers and falls rather than a drum.",
    nodes: &[
        node_p(
            "lfo",
            "Internal LFO",
            NodeKind::Modulator,
            &["RATE", "MODE"],
        ),
        node_p("depth", "Sweep depth", NodeKind::Mix, &["DEPTH"]),
        node("noise", "Noise", NodeKind::Source),
        node_p("svf", "Resonant SVF", NodeKind::Filter, &["START", "RESO"]),
        node("sat", "Soft clip", NodeKind::Shaper),
        node_p("env", "Decay env", NodeKind::Env, &["DEC"]),
        node("vca", "VCA", NodeKind::Mix),
        node(OUT, "To strip", NodeKind::Out),
    ],
    edges: &[
        Edge::new("lfo", "depth"),
        Edge::labelled("depth", "svf", "cutoff"),
        Edge::new("noise", "svf"),
        Edge::new("svf", "sat"),
        Edge::new("sat", "vca"),
        Edge::labelled("env", "vca", "amp"),
        Edge::new("vca", OUT),
    ],
    mod_routes: &[],
};

#[cfg(test)]
mod tests {
    use super::*;
    use device_core::params::{ParamGroup, ParamRoute};

    /// Every parameter a node names has to exist on that machine and be
    /// addressable. A typo here would otherwise print a label for a control
    /// that cannot be sent.
    #[test]
    fn every_named_param_exists_and_is_addressable() {
        for m in MachineId::ALL {
            for name in topology(m).param_names() {
                let found = m.param_by_name(name);
                let (_, info) = found.unwrap_or_else(|| {
                    panic!(
                        "{}: topology names '{}', which is not a param",
                        m.label(),
                        name
                    )
                });
                assert_ne!(
                    info.group as u8,
                    ParamGroup::Reserved as u8,
                    "{}: topology names reserved slot '{}'",
                    m.label(),
                    name
                );
            }
        }
    }

    /// Every row the voice itself consumes has to appear somewhere in its
    /// diagram. This is the check that catches a parameter added to a voice
    /// and forgotten here.
    ///
    /// `ParamRoute::Slot` is the right predicate rather than "group is
    /// VOICE": it means precisely "the voice reads this in `set_params`",
    /// which is what a voice diagram is obliged to show. It also catches the
    /// machine-specific FILTER rows — `HPF`, `BPF`, `Q`, `NCOL` — which a
    /// group test would miss because the strip's own cutoff shares that
    /// group.
    #[test]
    fn every_param_the_voice_consumes_appears_in_the_diagram() {
        for m in MachineId::ALL {
            let t = topology(m);
            for info in m.params().iter() {
                // Strip, LFO, pan, sends and routing are drawn once on
                // STRIP, not on every machine. Reserved rows are not drawn
                // at all: they have no address and no meaning.
                if !matches!(info.route, ParamRoute::Slot) || info.name == "RESV" {
                    continue;
                }
                assert!(
                    t.all_param_names().any(|n| n == info.name),
                    "{}: param '{}' is in the table but not in the diagram",
                    m.label(),
                    info.name
                );
            }
        }
    }

    /// The strip's own rows get the same treatment.
    #[test]
    fn the_strip_diagram_names_real_params() {
        let m = MachineId::ALL[0];
        for name in STRIP.param_names() {
            assert!(
                m.param_by_name(name).is_some(),
                "strip topology names '{name}', which is not a param"
            );
        }
        for route in STRIP.mod_routes {
            for name in route.params {
                assert!(
                    m.param_by_name(name).is_some(),
                    "strip mod route names '{name}', which is not a param"
                );
            }
        }
    }

    /// Edges have to join nodes that exist, or a renderer silently invents
    /// an empty box for the dangling end.
    #[test]
    fn every_edge_joins_real_nodes() {
        let all = MachineId::ALL.iter().map(|m| (m.label(), topology(*m)));
        for (label, t) in all.chain(core::iter::once(("strip", &STRIP))) {
            for e in t.edges {
                assert!(t.node(e.from).is_some(), "{label}: edge from '{}'", e.from);
                assert!(t.node(e.to).is_some(), "{label}: edge to '{}'", e.to);
            }
        }
    }

    /// Node ids have to be unique, or edges become ambiguous.
    #[test]
    fn node_ids_are_unique() {
        for m in MachineId::ALL {
            let t = topology(m);
            for (i, n) in t.nodes.iter().enumerate() {
                assert!(
                    !t.nodes[..i].iter().any(|p| p.id == n.id),
                    "{}: duplicate node id '{}'",
                    m.label(),
                    n.id
                );
            }
        }
    }

    /// Each machine graph ends at the strip, so the two diagrams join up.
    #[test]
    fn every_machine_hands_off_to_the_strip() {
        for m in MachineId::ALL {
            let t = topology(m);
            assert!(
                t.node(OUT).is_some(),
                "{}: no '{OUT}' node to hand off from",
                m.label()
            );
            assert!(
                t.edges.iter().any(|e| e.to == OUT),
                "{}: nothing reaches '{OUT}'",
                m.label()
            );
        }
    }
}
