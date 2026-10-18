//! Rendering a [`Topology`] as mermaid flowchart source.
//!
//! One source string serves both outputs: markdown fences it, and the HTML
//! page hands the same text to mermaid in the browser. Keeping them
//! identical is why there is no theme directive and no `classDef` here —
//! the sheet is read on GitHub in either colour scheme and in a dark HTML
//! page, and a palette baked into the source would be wrong in one of them.
//! Node *shape* carries the kind instead, which survives both.

use device_core::topology::{NodeKind, Topology};

/// Mermaid's delimiters for a node shape, chosen by what the node does.
///
/// Mermaid has no semantic node types, so the convention is ours:
/// rounded for things that make sound, parallelogram for envelopes and
/// modulators (they slope, like a ramp), plain rectangles for processing,
/// hexagons where signals meet, and a subroutine box for the handoff.
fn shape(kind: NodeKind) -> (&'static str, &'static str) {
    match kind {
        NodeKind::Source => ("([", "])"),
        NodeKind::Modulator => ("[/", "/]"),
        NodeKind::Env => ("[/", "/]"),
        NodeKind::Filter => ("[", "]"),
        NodeKind::Shaper => ("[", "]"),
        NodeKind::Mix => ("{{", "}}"),
        NodeKind::Out => ("[[", "]]"),
    }
}

/// Mermaid treats `"` as the label delimiter and has no escape for it, so
/// the few characters that would break a label are replaced rather than
/// escaped. Nothing in the committed graphs needs this; it is here so that
/// adding a machine cannot produce a page that silently fails to draw.
fn label_text(s: &str) -> String {
    s.replace('"', "'").replace('\n', " ")
}

/// The mermaid source for one topology, without a fence.
///
/// Parameters appear in the node they control, under the label. That is the
/// point of the diagram: the table says `SHAPE.MIX` is continuous and
/// defaults to 0.35, and only the picture says it is the dry/wet between
/// the shaper and Ripples.
pub(crate) fn render(topology: &Topology) -> String {
    let mut out = String::from("flowchart LR\n");

    for n in topology.nodes {
        let (open, close) = shape(n.kind);
        let mut label = label_text(n.label);
        if !n.params.is_empty() {
            label.push_str("<br/>");
            label.push_str(&n.params.join(" · "));
        }
        out.push_str(&format!("  {}{}\"{}\"{}\n", n.id, open, label, close));
    }

    for e in topology.edges {
        match e.label {
            Some(l) => out.push_str(&format!(
                "  {} -->|\"{}\"| {}\n",
                e.from,
                label_text(l),
                e.to
            )),
            None => out.push_str(&format!("  {} --> {}\n", e.from, e.to)),
        }
    }

    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use device_core::topology::{node, node_p, Edge, NodeKind, Topology};

    static SAMPLE: Topology = Topology {
        summary: "two boxes",
        nodes: &[
            node_p("osc", "Sine", NodeKind::Source, &["TUNE", "DEC"]),
            node("out", "To strip", NodeKind::Out),
        ],
        edges: &[Edge::labelled("osc", "out", "audio")],
        mod_routes: &[],
    };

    #[test]
    fn nodes_carry_their_params_and_edges_carry_their_labels() {
        let src = render(&SAMPLE);
        assert!(src.starts_with("flowchart LR\n"));
        assert!(src.contains(r#"osc(["Sine<br/>TUNE · DEC"])"#));
        assert!(src.contains(r#"out[["To strip"]]"#));
        assert!(src.contains(r#"osc -->|"audio"| out"#));
    }

    #[test]
    fn an_unlabelled_edge_is_a_plain_arrow() {
        static T: Topology = Topology {
            summary: "",
            nodes: &[
                node("a", "A", NodeKind::Source),
                node("b", "B", NodeKind::Out),
            ],
            edges: &[Edge::new("a", "b")],
            mod_routes: &[],
        };
        assert!(render(&T).contains("  a --> b\n"));
    }

    #[test]
    fn a_quote_in_a_label_cannot_break_the_source() {
        static T: Topology = Topology {
            summary: "",
            nodes: &[node("a", "the \"thing\"", NodeKind::Mix)],
            edges: &[],
            mod_routes: &[],
        };
        let src = render(&T);
        assert!(!src.contains(r#""the "thing""#), "unescaped quote: {src}");
        assert!(src.contains("the 'thing'"));
    }
}
