//! Signal path for the mi-drum device.
//!
//! One graph, not twenty-eight. Every machine on this device runs the same
//! fixed strip — the catalogue changes what the source box *is*, not what
//! the chain does with it — so [`STRIP_PATH`] is the device's path and
//! [`topology`] hands back the same thing for every machine.
//!
//! The Plaits or Peaks engine is drawn as a single opaque box. Its internals
//! are not addressable from here: `TUNE`, `HARM`, `TIMBRE` and `MORPH` mean
//! something different in each of the 28 engines, and nothing a reader could
//! act on would come from drawing a wave-terrain oscillator's guts.
//!
//! The track strip proper is almost absent, and that is correct: mi-drum
//! sets `strip_bypass`, so `Track` contributes level, pan, sends and routing
//! and nothing else. The filter and amp envelope a drum machine gets from
//! the core strip are, here, the device's own Ripples and the voice's LPG.

use device_core::topology::{node_p, Edge, ModRoute, NodeKind, Topology};

use super::MiMachineId;

/// The fixed strip, which is the whole device path.
pub static STRIP_PATH: Topology = Topology {
    summary: "Voice into the shaper — as carrier, with a selectable modulator \
              — then Ripples, then the track's level, pan and sends. The \
              machine changes the source box; the rest is fixed.",
    nodes: &[
        node_p(
            "voice",
            "Plaits / Peaks engine",
            NodeKind::Source,
            &[
                "TUNE", "HARM", "TIMBRE", "MORPH", "FM.AMT", "TM.MOD", "MM.MOD", "DECAY", "LPG.COL",
            ],
        ),
        node_p("carrier", "Carrier tap", NodeKind::Mix, &["AUX"]),
        node_p("modtap", "Modulator tap", NodeKind::Mix, &["SHAPE.IN"]),
        node_p(
            "shaperosc",
            "Strip oscillator",
            NodeKind::Modulator,
            &["SHAPE.OSC"],
        ),
        node_p(
            "shaper",
            "Shaper",
            NodeKind::Shaper,
            &["SHAPE.ALG", "SHAPE.TIM", "SHAPE.DRV"],
        ),
        node_p("tap", "Output tap", NodeKind::Mix, &["SHAPE.OUT"]),
        node_p("wet", "Dry / wet", NodeKind::Mix, &["SHAPE.MIX"]),
        node_p(
            "ripples",
            "Ripples SVF",
            NodeKind::Filter,
            &["RIP.CUT", "RIP.RES", "RIP.FM"],
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
        Edge::labelled("voice", "carrier", "main / aux"),
        Edge::new("voice", "modtap"),
        Edge::new("shaperosc", "modtap"),
        Edge::labelled("carrier", "shaper", "carrier"),
        Edge::labelled("modtap", "shaper", "modulator"),
        Edge::new("shaper", "tap"),
        Edge::new("tap", "wet"),
        Edge::labelled("carrier", "wet", "dry"),
        Edge::new("wet", "ripples"),
        Edge::new("ripples", "level"),
        Edge::new("level", "pan"),
        Edge::new("pan", "out"),
        Edge::labelled("level", "sends", "post-fader"),
    ],
    mod_routes: &[
        // Fixed destinations with their own depth each, rather than the drum
        // device's one selectable destination per LFO. Nothing to choose, so
        // nothing a diagram edge could get wrong — but the depths are still
        // parameters, and this is where they are accounted for.
        ModRoute {
            source: "LFO",
            dest: "Ripples cutoff and shaper timbre, each with its own depth",
            params: &["LFO.RATE", "LFO.DEPTH", "LFO.FIL", "LFO.WRP"],
        },
        ModRoute {
            source: "AD envelope",
            dest: "Ripples cutoff and shaper timbre, each with its own depth",
            params: &["AD.ATK", "AD.DEC", "AD.FIL", "AD.WRP"],
        },
    ],
};

/// The signal path for `machine` — the same path for all of them.
pub fn topology(machine: MiMachineId) -> &'static Topology {
    let _ = machine;
    &STRIP_PATH
}

#[cfg(test)]
mod tests {
    use super::*;
    use device_core::params::ParamRoute;

    /// Every parameter the device consumes itself has to appear in the
    /// diagram — on a node, or in the modulation table. See the drum
    /// device's equivalent for why `ParamRoute::Slot` is the predicate.
    #[test]
    fn every_param_the_voice_consumes_appears_in_the_diagram() {
        let m = MiMachineId::ALL[0];
        let t = topology(m);
        for info in m.params().iter() {
            if !matches!(info.route, ParamRoute::Slot) || info.name == "RESV" {
                continue;
            }
            assert!(
                t.all_param_names().any(|n| n == info.name),
                "param '{}' is in the table but not in the diagram",
                info.name
            );
        }
    }

    /// And every name the diagram uses has to be a real, addressable row.
    #[test]
    fn every_named_param_exists() {
        let m = MiMachineId::ALL[0];
        for name in topology(m).all_param_names() {
            assert!(
                m.param_by_name(name).is_some(),
                "topology names '{name}', which is not a param"
            );
        }
    }

    #[test]
    fn every_edge_joins_real_nodes() {
        let t = &STRIP_PATH;
        for e in t.edges {
            assert!(t.node(e.from).is_some(), "edge from '{}'", e.from);
            assert!(t.node(e.to).is_some(), "edge to '{}'", e.to);
        }
    }

    #[test]
    fn node_ids_are_unique() {
        let t = &STRIP_PATH;
        for (i, n) in t.nodes.iter().enumerate() {
            assert!(
                !t.nodes[..i].iter().any(|p| p.id == n.id),
                "duplicate node id '{}'",
                n.id
            );
        }
    }

    /// Every machine really does share the one path. If this ever stops
    /// being true, the cheat-sheet's single-diagram layout is wrong too.
    #[test]
    fn every_machine_shares_the_one_path() {
        for m in MiMachineId::ALL {
            assert!(core::ptr::eq(topology(m), &STRIP_PATH), "{}", m.label());
        }
    }
}
