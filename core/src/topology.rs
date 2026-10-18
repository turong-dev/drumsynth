//! Signal-path descriptions, for the generated cheat-sheet.
//!
//! A list of NRPN addresses tells you what you can send and nothing about
//! what the parameter is *in*. `SHAPE.MIX` and `SHAPE.OUT` are not
//! comprehensible as table rows; they are comprehensible as positions in a
//! chain. This module is the data a renderer draws that chain from.
//!
//! # Why data and not prose
//!
//! The cheat-sheet's whole claim is that it cannot drift from the engine,
//! because every row is read out of the device's parameter table. A
//! hand-written diagram per machine would forfeit that: it would rot the
//! first time a voice was rewired, silently, with nothing able to notice.
//!
//! As data, a graph can be *checked*. Every [`Node::params`] entry names a
//! row that must exist in the machine's table and must not be reserved, and
//! every machine-specific row must appear on some node. That does not prove
//! the wiring matches the DSP — the graph still asserts an arrangement the
//! code could contradict — but it pins the half that actually moves, which
//! is the parameter set. A parameter added to a machine and forgotten in
//! its diagram fails the build.
//!
//! # Why this is behind a feature
//!
//! These tables are host-only documentation: the firmware never draws a
//! diagram. They live in the engine crates so they sit next to the code they
//! describe, and behind the `topology` feature so they cost the Teensy
//! nothing. Same disposition as `debug-params`. The host renderer turns the
//! feature on; the firmware does not.
//!
//! # Modulation is not an edge
//!
//! Selectable modulation routing is described by [`ModRoute`] and rendered
//! as a table, not as graph edges. A destination chosen by a parameter value
//! has no fixed edge to draw: rendering every possible one is unreadable,
//! and rendering the default is wrong the moment it is changed.

/// What a node does in the chain. Chiefly a drawing hint — it picks the
/// shape and colour a renderer gives the box.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum NodeKind {
    /// Something that generates audio: an oscillator, a noise source.
    Source,
    /// An oscillator used to modulate rather than to be heard.
    Modulator,
    /// An envelope.
    Env,
    /// A filter.
    Filter,
    /// Saturation, wavefolding, distortion — anything that reshapes a
    /// waveform without being a filter.
    Shaper,
    /// A sum, crossfade or VCA: where signals meet.
    Mix,
    /// Where the voice hands off — in practice, the track strip.
    Out,
}

impl NodeKind {
    /// Short word for the kind, for a renderer that labels or groups by it.
    pub const fn name(self) -> &'static str {
        match self {
            Self::Source => "source",
            Self::Modulator => "modulator",
            Self::Env => "env",
            Self::Filter => "filter",
            Self::Shaper => "shaper",
            Self::Mix => "mix",
            Self::Out => "out",
        }
    }
}

/// One box in a signal path.
#[derive(Clone, Copy, Debug)]
pub struct Node {
    /// Identifier, unique within a [`Topology`]. What [`Edge`] endpoints
    /// name, and what a renderer uses as the node id.
    pub id: &'static str,
    /// Human label for the box.
    pub label: &'static str,
    /// What the node does.
    pub kind: NodeKind,
    /// Parameters that control this node, by [`ParamInfo::name`].
    ///
    /// Names rather than flat slot indices: the slot layout is an internal
    /// detail that the ABI rules explicitly leave free to reorganise, and a
    /// table of magic numbers is unreadable in the source and silently wrong
    /// after a layout change. A name that does not resolve fails the tests.
    ///
    /// [`ParamInfo::name`]: crate::params::ParamInfo::name
    pub params: &'static [&'static str],
}

/// A connection between two [`Node`]s.
#[derive(Clone, Copy, Debug)]
pub struct Edge {
    /// [`Node::id`] of the source.
    pub from: &'static str,
    /// [`Node::id`] of the destination.
    pub to: &'static str,
    /// What travels along it, when that is not obvious — `"FM"`,
    /// `"ring mod"`, `"pitch"`. `None` draws a plain arrow.
    pub label: Option<&'static str>,
}

impl Edge {
    /// A plain unlabelled connection.
    pub const fn new(from: &'static str, to: &'static str) -> Self {
        Self {
            from,
            to,
            label: None,
        }
    }

    /// A connection carrying a named signal.
    pub const fn labelled(from: &'static str, to: &'static str, label: &'static str) -> Self {
        Self {
            from,
            to,
            label: Some(label),
        }
    }
}

/// One row of the modulation table: a source, where it can go, and the
/// parameters that set it up.
#[derive(Clone, Copy, Debug)]
pub struct ModRoute {
    /// The modulator — `"LFO 1"`, `"AD envelope"`.
    pub source: &'static str,
    /// Where it lands. For a selectable route, the set of choices in words
    /// rather than a single edge, because the choice is a parameter value.
    pub dest: &'static str,
    /// Parameters that configure the route, by name.
    pub params: &'static [&'static str],
}

/// A machine's signal path.
#[derive(Clone, Copy, Debug)]
pub struct Topology {
    /// One-line description of the chain, for a renderer that wants prose
    /// above the diagram.
    pub summary: &'static str,
    /// The boxes.
    pub nodes: &'static [Node],
    /// The connections between them.
    pub edges: &'static [Edge],
    /// Modulation routing, rendered as a table rather than as edges.
    pub mod_routes: &'static [ModRoute],
}

impl Topology {
    /// Look up a node by id.
    pub fn node(&self, id: &str) -> Option<&Node> {
        self.nodes.iter().find(|n| n.id == id)
    }

    /// Every parameter name mentioned by any node.
    pub fn param_names(&self) -> impl Iterator<Item = &'static str> + '_ {
        self.nodes.iter().flat_map(|n| n.params.iter().copied())
    }

    /// Every parameter name the diagram accounts for, on a node or in the
    /// modulation table.
    ///
    /// The modulation table is part of the diagram — a route's rate, depth
    /// and destination rows are described there instead of on an edge — so
    /// a completeness check has to count both or it would demand that an
    /// LFO's rate appear as a box.
    pub fn all_param_names(&self) -> impl Iterator<Item = &'static str> + '_ {
        self.param_names().chain(
            self.mod_routes
                .iter()
                .flat_map(|r| r.params.iter().copied()),
        )
    }
}

/// Build a [`Node`] with no parameters of its own — a sum, a fixed
/// saturator, the handoff to the strip.
pub const fn node(id: &'static str, label: &'static str, kind: NodeKind) -> Node {
    Node {
        id,
        label,
        kind,
        params: &[],
    }
}

/// Build a [`Node`] controlled by the named parameters.
pub const fn node_p(
    id: &'static str,
    label: &'static str,
    kind: NodeKind,
    params: &'static [&'static str],
) -> Node {
    Node {
        id,
        label,
        kind,
        params,
    }
}
