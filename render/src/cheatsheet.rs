//! `render cheatsheet` — the parameter reference, generated from the engine.
//!
//! Organised the way the control surface is: by [`ParamGroup`], with each
//! row's NRPN address `(group, index)` as its identity. That address is the
//! only external name a parameter has — the flat slot index is an internal
//! layout detail since the CC map was removed — so it is what the table
//! leads with.
//!
//! Everything here is read out of the device's own parameter tables, so it
//! cannot drift from the engine.
//!
//! # Two devices, two shapes
//!
//! The generator is generic over [`CheatDevice`], so `drum` and `mi-drum`
//! both get a sheet. They do not get the *same* sheet, because their tables
//! differ in kind rather than in content:
//!
//! - **drum** gives each machine its own table, so the sheet splits into the
//!   rows every machine shares and one section per machine for the rest.
//! - **mi-drum** hands every machine the same static table, so that split
//!   would file 100% of rows under "common" and emit 28 empty sections. It
//!   gets one table, and its catalogue appears as what it actually is: the
//!   options of the `MACH` selector.
//!
//! Which shape a device gets is *derived*, not declared — if no machine has
//! a row of its own, there is nothing to put in a per-machine section. A
//! device that varied only some of its machines would fall out correctly
//! without anyone revisiting this.

use device_core::topology::Topology;
use drum_engine::machines::{MachineId, ParamGroup, ParamKind, NUM_PARAMS as DRUM_PARAMS};
use drum_engine::DeviceModel;
use mi_drum_engine::{MiMachineId, NUM_PARAMS as MI_PARAMS};

use crate::mermaid;

/// What the cheat-sheet needs from a device on top of [`DeviceModel`].
///
/// [`DeviceModel`] already vends the parameter table, the machine label and
/// the catalogue (`count` / `from_index`). The two things missing are a
/// filename-safe short name for HTML anchors and the sheet's own title —
/// both display concerns, which is why they live here in the renderer
/// rather than being pushed into `core`'s trait and onto the firmware.
pub(crate) trait CheatDevice<const N: usize>: DeviceModel<N> {
    /// Short canonical name, as used in CLI args. Becomes the HTML anchor.
    fn short_name(self) -> &'static str;

    /// This machine's signal path.
    fn topology(self) -> &'static Topology;

    /// The path everything downstream of a machine shares — the track
    /// strip. Drawn once, in the shared section, so the per-machine
    /// diagrams can stop at its input instead of repeating it.
    fn strip_topology() -> &'static Topology;

    /// Title of the generated sheet.
    const TITLE: &'static str;
}

impl CheatDevice<DRUM_PARAMS> for MachineId {
    fn short_name(self) -> &'static str {
        self.name()
    }

    fn topology(self) -> &'static Topology {
        drum_engine::machines::topology::topology(self)
    }

    fn strip_topology() -> &'static Topology {
        &drum_engine::machines::topology::STRIP
    }

    const TITLE: &'static str = "Drum Synth Param Cheat-Sheet";
}

impl CheatDevice<MI_PARAMS> for MiMachineId {
    fn short_name(self) -> &'static str {
        self.name()
    }

    fn topology(self) -> &'static Topology {
        mi_drum_engine::topology::topology(self)
    }

    fn strip_topology() -> &'static Topology {
        &mi_drum_engine::topology::STRIP_PATH
    }

    const TITLE: &'static str = "MI-Drum Param Cheat-Sheet";
}

/// One row in the cheat-sheet table.
pub(crate) struct CheatRow {
    /// Group the parameter is addressed in.
    pub(crate) group: ParamGroup,
    /// Index within the group — the NRPN parameter-select LSB.
    pub(crate) index: usize,
    /// Flat index into the parameter array. Internal; shown because the
    /// device's tables and every `set_params` are written in these terms.
    pub(crate) slot: usize,
    pub(crate) name: &'static str,
    pub(crate) abbrev: &'static str,
    pub(crate) kind: ParamKind,
    pub(crate) default: f32,
}

impl CheatRow {
    /// `(GROUP, index)` as a controller would be configured with it.
    ///
    /// The tables print the index under a group heading rather than this
    /// composite, so it survives only as the form the tests assert in — it
    /// is the thing those tests are about.
    #[cfg(test)]
    fn address(&self) -> String {
        format!("({}, {})", self.group.name(), self.index)
    }

    /// What the `0..1` value means, in a column's worth of words.
    fn kind_label(&self) -> String {
        match self.kind {
            ParamKind::Continuous => "continuous".to_string(),
            ParamKind::Bipolar => "bipolar".to_string(),
            ParamKind::Discrete { count } => format!("{count} options"),
        }
    }

    fn default_label(&self) -> String {
        match self.kind {
            // A discrete default is an option, not a fraction: printing
            // "0.00" for machine 0 of 15 tells you nothing you can send.
            ParamKind::Discrete { count } if count > 1 => {
                let k = (self.default * (count - 1) as f32 + 0.5) as u32;
                format!("option {k}")
            }
            _ => format!("{:.2}", self.default),
        }
    }
}

/// The device's catalogue, in index order.
fn catalogue<D: CheatDevice<N>, const N: usize>() -> Vec<D> {
    (0..D::count()).filter_map(D::from_index).collect()
}

/// Every addressable row of a machine's table, in group order.
///
/// `Reserved` rows are skipped: they have no address, which is the point of
/// being reserved. Within a group the rows keep their flat order, because
/// that is the order the group's indices are assigned in.
fn rows_for<D: CheatDevice<N>, const N: usize>(
    machine: D,
    keep: impl Fn(usize) -> bool,
) -> Vec<CheatRow> {
    let table = machine.param_table();
    let mut out = Vec::new();
    for group in ParamGroup::ADDRESSABLE {
        let mut index = 0;
        for (slot, info) in table.iter().enumerate() {
            if info.group as u8 != group as u8 {
                continue;
            }
            // The ordinal counts every row in the group, including ones
            // this section filters out — it is the device's address, not a
            // position in this table.
            let this = index;
            index += 1;
            if !keep(slot) {
                continue;
            }
            out.push(CheatRow {
                group,
                index: this,
                slot,
                name: info.name,
                abbrev: info.abbrev,
                kind: info.kind,
                default: info.default,
            });
        }
    }
    out
}

/// Which slots every machine agrees on.
///
/// A slot is common when every machine gives it the same name and it is not
/// reserved — the track-routed rows, in practice. Everything else is the
/// machine's own, which is what VOICE is for.
fn common_slots<D: CheatDevice<N>, const N: usize>(machines: &[D]) -> [bool; N] {
    let mut is_common = [false; N];
    for (slot, common) in is_common.iter_mut().enumerate() {
        let first = machines[0].param_table()[slot].name;
        if first != "RESV" && machines.iter().all(|m| m.param_table()[slot].name == first) {
            *common = true;
        }
    }
    is_common
}

/// The sheet, split the way this device's tables call for.
struct Sheet<D> {
    /// Rows every machine shares — the whole sheet on a device whose
    /// machines share one table.
    common: Vec<CheatRow>,
    /// Per-machine rows. Empty for every machine on a device whose machines
    /// share one table, which is what [`Sheet::varies`] reads.
    per_machine: Vec<(D, Vec<CheatRow>)>,
}

impl<D> Sheet<D> {
    /// Whether any machine has rows of its own, and therefore whether the
    /// per-machine sections exist at all.
    fn varies(&self) -> bool {
        self.per_machine.iter().any(|(_, rows)| !rows.is_empty())
    }
}

fn build_sheet<D: CheatDevice<N>, const N: usize>(machines: &[D]) -> Sheet<D> {
    let is_common = common_slots(machines);
    Sheet {
        common: rows_for(machines[0], |slot| is_common[slot]),
        per_machine: machines
            .iter()
            .map(|&m| (m, rows_for(m, |slot| !is_common[slot])))
            .collect(),
    }
}

/// Generate the cheat-sheet for `device` in `format`.
///
/// `device` is the CLI name (`"drum"` or `"mi-drum"`); `format` is
/// `"markdown"` or `"html"`. An unknown device is an error rather than a
/// default, so a typo does not silently hand back the wrong device's sheet.
pub(crate) fn generate_cheatsheet(device: &str, format: &str) -> Result<String, String> {
    match device {
        "drum" => Ok(generate::<MachineId, DRUM_PARAMS>(format)),
        "mi-drum" => Ok(generate::<MiMachineId, MI_PARAMS>(format)),
        other => Err(format!(
            "unknown device '{other}'. available: drum, mi-drum"
        )),
    }
}

fn generate<D: CheatDevice<N>, const N: usize>(format: &str) -> String {
    let machines = catalogue::<D, N>();
    let sheet = build_sheet::<D, N>(&machines);

    if format == "html" {
        generate_html::<D, N>(&machines, &sheet)
    } else {
        generate_markdown::<D, N>(&machines, &sheet)
    }
}

/// Split rows into one run per group, in the order [`rows_for`] emitted
/// them — which is [`ParamGroup::ADDRESSABLE`] order, the order a control
/// surface pages through.
///
/// A group with no rows in this section simply does not appear, so a
/// machine with only VOICE rows gets one table rather than seven, six of
/// them empty.
fn by_group(rows: &[CheatRow]) -> Vec<(ParamGroup, &[CheatRow])> {
    let mut out: Vec<(ParamGroup, &[CheatRow])> = Vec::new();
    let mut start = 0;
    for i in 0..rows.len() {
        let last = i + 1 == rows.len();
        if last || rows[i + 1].group as u8 != rows[i].group as u8 {
            out.push((rows[i].group, &rows[start..=i]));
            start = i + 1;
        }
    }
    out
}

/// Heading for a group's table: the name a controller sends and the number
/// it sends it as.
fn group_heading(group: ParamGroup) -> String {
    format!("{} — group {}", group.name(), group as u8)
}

const HEADER: &str = "| Index | Name | Abbrev | Kind | Default | Slot |\n\
                      |-------|------|--------|------|---------|------|\n";

/// One table per group, each under its own heading.
///
/// The address column is gone: under a `VOICE — group 1` heading, printing
/// `(VOICE, 0)`, `(VOICE, 1)`, `(VOICE, 2)` down the page repeats the
/// heading on every row. The index is the half that varies, and the heading
/// carries the half that does not.
fn markdown_rows(out: &mut String, rows: &[CheatRow], depth: usize) {
    for (group, group_rows) in by_group(rows) {
        out.push_str(&format!(
            "{} {}\n\n",
            "#".repeat(depth),
            group_heading(group)
        ));
        markdown_group_table(out, group_rows);
    }
}

fn markdown_group_table(out: &mut String, rows: &[CheatRow]) {
    out.push_str(HEADER);
    for row in rows {
        out.push_str(&format!(
            "| {} | {} | {} | {} | {} | {} |\n",
            row.index,
            row.name,
            row.abbrev,
            row.kind_label(),
            row.default_label(),
            row.slot
        ));
    }
    out.push('\n');
}

/// The `MACH` catalogue: which option number selects which machine.
///
/// `MACH` prints as "N options" like any other discrete row, and nothing
/// else on the sheet says what option 7 is. On a device with per-machine
/// sections the headings imply it; here it is stated outright.
fn markdown_catalogue<D: CheatDevice<N>, const N: usize>(out: &mut String, machines: &[D]) {
    out.push_str("| Option | Name | Machine |\n|--------|------|---------|\n");
    for (i, m) in machines.iter().enumerate() {
        out.push_str(&format!(
            "| {} | `{}` | {} |\n",
            i,
            m.short_name(),
            m.label()
        ));
    }
    out.push('\n');
}

/// A topology as a fenced mermaid block, with its summary above it and its
/// modulation table below.
fn markdown_diagram(out: &mut String, topology: &Topology) {
    if !topology.summary.is_empty() {
        out.push_str(topology.summary);
        out.push_str("\n\n");
    }
    out.push_str("```mermaid\n");
    out.push_str(&mermaid::render(topology));
    out.push_str("```\n\n");
    markdown_mod_routes(out, topology);
}

/// Modulation as a table rather than as graph edges.
///
/// A destination chosen by a parameter value has no fixed edge to draw:
/// every possible one is unreadable, and the default is wrong as soon as it
/// is changed. The table states the choice instead.
fn markdown_mod_routes(out: &mut String, topology: &Topology) {
    if topology.mod_routes.is_empty() {
        return;
    }
    out.push_str("| Modulator | Destination | Params |\n");
    out.push_str("|-----------|-------------|--------|\n");
    for r in topology.mod_routes {
        let params: Vec<String> = r.params.iter().map(|p| format!("`{p}`")).collect();
        out.push_str(&format!(
            "| {} | {} | {} |\n",
            r.source,
            r.dest,
            params.join(", ")
        ));
    }
    out.push('\n');
}

/// Markdown output: one heading + table per section.
fn generate_markdown<D: CheatDevice<N>, const N: usize>(
    machines: &[D],
    sheet: &Sheet<D>,
) -> String {
    let mut out = String::new();

    out.push_str(&format!("# {}\n\n", D::TITLE));
    out.push_str("Generated from the engine's own parameter tables.\n\n");
    out.push_str(INTRO);
    out.push('\n');

    out.push_str("## Machines\n\n");
    out.push_str("The options of the `MACH` parameter, in catalogue order.\n\n");
    markdown_catalogue::<D, N>(&mut out, machines);

    if sheet.varies() {
        out.push_str("## Common Parameters (same on every machine)\n\n");
        out.push_str(
            "Track-level routing, the strip, and the LFOs. Values are factory defaults.\n\n",
        );
        markdown_diagram(&mut out, D::strip_topology());
        markdown_rows(&mut out, &sheet.common, 3);

        for (m, rows) in &sheet.per_machine {
            out.push_str(&format!("## {}\n\n", m.label()));
            markdown_diagram(&mut out, m.topology());
            markdown_rows(&mut out, rows, 3);
        }
    } else {
        out.push_str("## Parameters\n\n");
        out.push_str(
            "Every machine on this device shares one parameter table: the machine \
             changes what the voice does with the VOICE rows, not which rows exist. \
             Values are factory defaults.\n\n",
        );
        markdown_diagram(&mut out, D::strip_topology());
        markdown_rows(&mut out, &sheet.common, 3);
    }

    out.push_str("---\n\n");
    out.push_str(FOOTER);

    out
}

/// The paragraph that explains how to send one of these.
const INTRO: &str = "\
Every parameter is addressed over **NRPN** as `(group, index)`, at 14-bit
resolution. On MIDI channel `N` — which selects track `N` — send:

| CC | value |
|----|-------|
| 99 | group number |
| 98 | index within the group |
|  6 | value, high 7 bits |
| 38 | value, low 7 bits |

CC 96 / 97 increment and decrement the selected parameter instead; their
data byte is the step size in 14-bit LSBs (0 means 1), so a controller with
an acceleration curve can send it directly. On a parameter with a fixed set
of options, a step moves exactly one option.

Group numbers: TRACK 0, VOICE 1, FILTER 2, SHAPER 3, AMP 4, LFO 5, ENV 6.
";

const FOOTER: &str = "\
**Index** is the second half of what a controller sends. Its table's \
heading is the first half: a row under `VOICE — group 1` at index 3 is the \
NRPN address `(1, 3)`.  \n\
**Slot** is the flat index into the engine's parameter array — internal, \
shown because the device's tables and `set_params` are written in those \
terms. Reserved slots have no address and are not listed.  \n\
**Kind**: `continuous` is a plain 0..1 sweep; `bipolar` is centred at 0.5; \
`N options` is a fixed set, where option `k` is the value `k / (N - 1)`.
";

/// HTML output: self-contained styled page.
fn generate_html<D: CheatDevice<N>, const N: usize>(machines: &[D], sheet: &Sheet<D>) -> String {
    let mut out = String::new();

    out.push_str("<!DOCTYPE html>\n<html lang=\"en\">\n<head>\n");
    out.push_str("<meta charset=\"utf-8\">\n");
    out.push_str("<meta name=\"viewport\" content=\"width=device-width, initial-scale=1\">\n");
    out.push_str(&format!("<title>{}</title>\n", D::TITLE));
    out.push_str("<style>\n");
    out.push_str(CHEATSHEET_CSS);
    out.push_str("</style>\n");
    out.push_str("</head>\n<body>\n");

    out.push_str("<header>\n");
    out.push_str(&format!("<h1>{}</h1>\n", D::TITLE));
    out.push_str("<p>Generated from the engine's own parameter tables.</p>\n");
    out.push_str("</header>\n\n");

    out.push_str("<section id=\"nrpn\">\n<h2>Sending a parameter</h2>\n");
    out.push_str("<p>Every parameter is addressed over <strong>NRPN</strong> as <code>(group, index)</code>, at 14-bit resolution. On MIDI channel <code>N</code> — which selects track <code>N</code> — send CC 99 = group, CC 98 = index, then CC 6 (high 7 bits) and CC 38 (low 7 bits).</p>\n");
    out.push_str("<p>CC 96 / 97 increment and decrement the selected parameter instead; the data byte is the step size in 14-bit LSBs (0 means 1). On a parameter with a fixed set of options, a step moves exactly one option.</p>\n");
    out.push_str(
        "<p>Group numbers: TRACK 0, VOICE 1, FILTER 2, SHAPER 3, AMP 4, LFO 5, ENV 6.</p>\n",
    );
    out.push_str("</section>\n\n");

    // Nav
    out.push_str("<nav class=\"toc\">\n<h2>Contents</h2>\n<ul>\n");
    out.push_str("<li><a href=\"#machines\">Machines</a></li>\n");
    if sheet.varies() {
        out.push_str("<li><a href=\"#common\">Common Parameters</a></li>\n");
        for (m, _) in &sheet.per_machine {
            let id = machine_anchor::<D, N>(*m);
            out.push_str(&format!("<li><a href=\"#{id}\">{}</a></li>\n", m.label()));
        }
    } else {
        out.push_str("<li><a href=\"#params\">Parameters</a></li>\n");
    }
    out.push_str("</ul>\n</nav>\n\n");

    out.push_str("<section id=\"machines\">\n<h2>Machines</h2>\n");
    out.push_str("<p>The options of the <code>MACH</code> parameter, in catalogue order.</p>\n");
    html_catalogue::<D, N>(&mut out, machines);
    out.push_str("</section>\n\n");

    if sheet.varies() {
        out.push_str("<section id=\"common\">\n");
        out.push_str(
            "<h2>Common Parameters <span class=\"badge\">same on every machine</span></h2>\n",
        );
        out.push_str(
            "<p>Track-level routing, the strip, and the LFOs. Values are factory defaults.</p>\n",
        );
        html_diagram(&mut out, D::strip_topology());
        html_table(&mut out, &sheet.common);
        out.push_str("</section>\n\n");

        for (m, rows) in &sheet.per_machine {
            let id = machine_anchor::<D, N>(*m);
            out.push_str(&format!("<section id=\"{id}\">\n"));
            out.push_str(&format!("<h2>{}</h2>\n", m.label()));
            html_diagram(&mut out, m.topology());
            html_table(&mut out, rows);
            out.push_str("</section>\n\n");
        }
    } else {
        out.push_str("<section id=\"params\">\n");
        out.push_str("<h2>Parameters <span class=\"badge\">same on every machine</span></h2>\n");
        out.push_str("<p>Every machine on this device shares one parameter table: the machine changes what the voice does with the VOICE rows, not which rows exist. Values are factory defaults.</p>\n");
        html_diagram(&mut out, D::strip_topology());
        html_table(&mut out, &sheet.common);
        out.push_str("</section>\n\n");
    }

    out.push_str("<footer>\n<p><strong>Index</strong> is the second half of what a controller sends; its table's heading is the first half, so a row under <code>VOICE — group 1</code> at index 3 is the NRPN address <code>(1, 3)</code>. <strong>Slot</strong> is the flat index into the engine's parameter array — internal, shown because the device's tables and <code>set_params</code> are written in those terms. Reserved slots have no address and are not listed.</p>\n");
    out.push_str("</footer>\n");
    html_mermaid_script(&mut out);
    out.push_str("</body>\n</html>\n");

    out
}

/// A topology as a collapsible panel holding mermaid source.
///
/// The source sits in the DOM as text and mermaid replaces it in place, so
/// a reader with no network sees the graph written out rather than an empty
/// box. That is the whole mitigation for the CDN — see the module docs on
/// `html_mermaid_script`.
///
/// Collapsed by default: fifteen expanded diagrams is a long scroll past
/// the tables people came for.
fn html_diagram(out: &mut String, topology: &Topology) {
    out.push_str("<details class=\"diagram\">\n");
    out.push_str("<summary>Signal path</summary>\n");
    if !topology.summary.is_empty() {
        out.push_str(&format!("<p>{}</p>\n", topology.summary));
    }
    out.push_str("<pre class=\"mermaid\">\n");
    out.push_str(&html_escape(&mermaid::render(topology)));
    out.push_str("</pre>\n");
    html_mod_routes(out, topology);
    out.push_str("</details>\n");
}

fn html_mod_routes(out: &mut String, topology: &Topology) {
    if topology.mod_routes.is_empty() {
        return;
    }
    out.push_str("<table>\n<thead>\n<tr><th>Modulator</th><th>Destination</th><th>Params</th></tr>\n</thead>\n<tbody>\n");
    for r in topology.mod_routes {
        let params: Vec<String> = r
            .params
            .iter()
            .map(|p| format!("<code>{p}</code>"))
            .collect();
        out.push_str(&format!(
            "<tr><td>{}</td><td>{}</td><td>{}</td></tr>\n",
            r.source,
            r.dest,
            params.join(", ")
        ));
    }
    out.push_str("</tbody>\n</table>\n");
}

/// Mermaid, loaded from a CDN.
///
/// This is the one thing on the page that is not self-contained, and it is
/// a deliberate, temporary trade: one diagram source for both outputs, at
/// the cost of the diagrams not drawing without a network. The tables —
/// which are what the sheet is mostly for — still render offline, and the
/// `<pre>` fallback above means the graph is still *readable* offline, just
/// as text rather than as boxes.
///
/// Replacing this with SVG emitted from the same graph data restores a
/// fully offline page; the graph data does not change, only the renderer.
fn html_mermaid_script(out: &mut String) {
    out.push_str("<script type=\"module\">\n");
    out.push_str(
        "import mermaid from \"https://cdn.jsdelivr.net/npm/mermaid@11/dist/mermaid.esm.min.mjs\";\n",
    );
    // `securityLevel: strict` keeps the renderer from honouring click
    // handlers or HTML in labels; the labels are ours, but the page is a
    // generated artifact people pass around.
    out.push_str(
        "mermaid.initialize({ startOnLoad: true, theme: \"dark\", securityLevel: \"strict\" });\n",
    );
    out.push_str("</script>\n");
}

/// The three characters that would otherwise end the `<pre>` early or open
/// a tag inside it. Mermaid reads `textContent`, so these round-trip.
fn html_escape(s: &str) -> String {
    s.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
}

/// One table per group, each under its own heading. See [`markdown_rows`]
/// for why the address column is an index column here.
fn html_table(out: &mut String, rows: &[CheatRow]) {
    for (group, group_rows) in by_group(rows) {
        out.push_str(&format!(
            "<h3>{}</h3>\n",
            html_escape(&group_heading(group))
        ));
        html_group_table(out, group_rows);
    }
}

fn html_group_table(out: &mut String, rows: &[CheatRow]) {
    out.push_str("<table>\n<thead>\n<tr><th>Index</th><th>Name</th><th>Abbrev</th><th>Kind</th><th>Default</th><th>Slot</th></tr>\n</thead>\n<tbody>\n");
    for row in rows {
        out.push_str(&format!(
            "<tr><td>{}</td><td>{}</td><td>{}</td><td>{}</td><td>{}</td><td>{}</td></tr>\n",
            row.index,
            row.name,
            row.abbrev,
            row.kind_label(),
            row.default_label(),
            row.slot
        ));
    }
    out.push_str("</tbody>\n</table>\n");
}

fn html_catalogue<D: CheatDevice<N>, const N: usize>(out: &mut String, machines: &[D]) {
    out.push_str("<table>\n<thead>\n<tr><th>Option</th><th>Name</th><th>Machine</th></tr>\n</thead>\n<tbody>\n");
    for (i, m) in machines.iter().enumerate() {
        out.push_str(&format!(
            "<tr><td>{}</td><td><code>{}</code></td><td>{}</td></tr>\n",
            i,
            m.short_name(),
            m.label()
        ));
    }
    out.push_str("</tbody>\n</table>\n");
}

fn machine_anchor<D: CheatDevice<N>, const N: usize>(m: D) -> String {
    m.short_name().replace('-', "_")
}

const CHEATSHEET_CSS: &str = r#"
:root {
  --bg: #0f1117;
  --surface: #1a1d27;
  --border: #2a2e3a;
  --text: #e0e0e6;
  --muted: #8b8fa3;
  --accent: #7aa2f7;
  --resv: #4a4e5a;
  --badge-bg: #2a3a5a;
  --badge-fg: #7aa2f7;
}
* { box-sizing: border-box; margin: 0; padding: 0; }
body {
  font-family: -apple-system, BlinkMacSystemFont, "Segoe UI", system-ui, sans-serif;
  background: var(--bg);
  color: var(--text);
  line-height: 1.6;
  max-width: 960px;
  margin: 0 auto;
  padding: 2rem 1.5rem;
}
header { margin-bottom: 2rem; }
h1 { font-size: 1.75rem; font-weight: 700; }
h2 { font-size: 1.25rem; font-weight: 600; margin-top: 2.5rem; margin-bottom: 0.75rem; }
h3 {
  font-size: 0.8rem;
  font-weight: 600;
  text-transform: uppercase;
  letter-spacing: 0.06em;
  color: var(--accent);
  margin-top: 1.5rem;
  margin-bottom: 0.4rem;
}
h3:first-of-type { margin-top: 0.5rem; }
table + h3 { margin-top: 1.75rem; }
p { color: var(--muted); margin-bottom: 1rem; }
code { font-family: "SF Mono", "Fira Code", monospace; font-size: 0.9em; }
.badge {
  display: inline-block;
  font-size: 0.7rem;
  font-weight: 600;
  padding: 0.15rem 0.5rem;
  border-radius: 4px;
  background: var(--badge-bg);
  color: var(--badge-fg);
  vertical-align: middle;
  margin-left: 0.5rem;
}
nav.toc {
  background: var(--surface);
  border: 1px solid var(--border);
  border-radius: 8px;
  padding: 1rem 1.5rem;
  margin-bottom: 2rem;
}
nav.toc h2 { margin-top: 0; font-size: 1rem; }
nav.toc ul { list-style: none; columns: 2; }
nav.toc a { color: var(--accent); text-decoration: none; font-size: 0.9rem; }
nav.toc a:hover { text-decoration: underline; }
section {
  background: var(--surface);
  border: 1px solid var(--border);
  border-radius: 8px;
  padding: 1.5rem;
  margin-bottom: 1.5rem;
}
table { width: 100%; border-collapse: collapse; font-size: 0.875rem; }
th, td { text-align: left; padding: 0.5rem 0.75rem; border-bottom: 1px solid var(--border); }
th { color: var(--muted); font-weight: 600; font-size: 0.8rem; text-transform: uppercase; letter-spacing: 0.05em; }
td { font-variant-numeric: tabular-nums; }
tr.resv { color: var(--resv); }
tr.resv td { font-style: italic; }
tr:hover { background: rgba(122, 162, 247, 0.05); }
footer { margin-top: 2rem; padding-top: 1rem; border-top: 1px solid var(--border); }
footer p { font-size: 0.8rem; }
details.diagram {
  border: 1px solid var(--border);
  border-radius: 6px;
  padding: 0.5rem 0.75rem;
  margin-bottom: 1rem;
  background: var(--bg);
}
details.diagram > summary {
  cursor: pointer;
  color: var(--accent);
  font-size: 0.85rem;
  font-weight: 600;
  list-style-position: outside;
}
details.diagram[open] > summary { margin-bottom: 0.75rem; }
details.diagram p { font-size: 0.85rem; }
/* Until mermaid replaces it, this holds the graph as readable text — which
   is all an offline reader gets, and is deliberately legible. */
pre.mermaid {
  font-family: "SF Mono", "Fira Code", monospace;
  font-size: 0.8rem;
  color: var(--muted);
  overflow-x: auto;
  white-space: pre;
  margin-bottom: 1rem;
}
pre.mermaid svg { max-width: 100%; height: auto; }
"#;

#[cfg(test)]
mod tests {
    use super::*;
    use drum_engine::machines::{resolve, ParamId};

    /// Common + machine-specific must partition the addressable rows: a row
    /// in neither is a parameter the sheet cannot tell you how to reach, and
    /// a row in both is an address printed twice.
    fn every_addressable_row_appears_once<D: CheatDevice<N>, const N: usize>() {
        let machines = catalogue::<D, N>();
        let sheet = build_sheet::<D, N>(&machines);
        for (m, rows) in &sheet.per_machine {
            let mut seen: Vec<usize> = sheet.common.iter().map(|r| r.slot).collect();
            seen.extend(rows.iter().map(|r| r.slot));
            seen.sort_unstable();
            let addressable: Vec<usize> = m
                .param_table()
                .iter()
                .enumerate()
                .filter(|(_, i)| i.group as u8 != ParamGroup::Reserved as u8)
                .map(|(slot, _)| slot)
                .collect();
            assert_eq!(seen, addressable, "{}", m.label());
        }
    }

    #[test]
    fn every_addressable_row_appears_exactly_once() {
        every_addressable_row_appears_once::<MachineId, DRUM_PARAMS>();
        every_addressable_row_appears_once::<MiMachineId, MI_PARAMS>();
    }

    /// The sheet's `(group, index)` has to be the address the engine
    /// resolves, or it is a lie that nothing else would catch.
    fn addresses_resolve<D: CheatDevice<N>, const N: usize>() {
        for m in catalogue::<D, N>() {
            for row in rows_for(m, |_| true) {
                let flat = resolve(
                    m.param_table(),
                    ParamId {
                        group: row.group,
                        index: row.index as u8,
                    },
                );
                assert_eq!(
                    flat,
                    Some(row.slot),
                    "{} {} resolves elsewhere",
                    m.label(),
                    row.address()
                );
            }
        }
    }

    #[test]
    fn addresses_match_what_resolve_finds() {
        addresses_resolve::<MachineId, DRUM_PARAMS>();
        addresses_resolve::<MiMachineId, MI_PARAMS>();
    }

    #[test]
    fn the_machine_selector_prints_as_an_option() {
        let rows = rows_for(MachineId::ALL[0], |_| true);
        let mach = rows.iter().find(|r| r.name == "MACH").unwrap();
        assert_eq!(mach.address(), "(TRACK, 0)");
        assert_eq!(mach.kind_label(), format!("{} options", MachineId::COUNT));
        assert_eq!(mach.default_label(), "option 0");
    }

    #[test]
    fn mi_drums_machine_selector_spans_its_own_catalogue() {
        // The selector's option count is this device's catalogue, not the
        // drum device's — the bug a hardcoded generator would reintroduce.
        let rows = rows_for(MiMachineId::ALL[0], |_| true);
        let mach = rows.iter().find(|r| r.name == "MACH").unwrap();
        assert_eq!(mach.address(), "(TRACK, 0)");
        assert_eq!(mach.kind_label(), format!("{} options", MiMachineId::COUNT));
    }

    #[test]
    fn the_drum_device_varies_per_machine_and_mi_drum_does_not() {
        // The property the section split is derived from. mi-drum hands
        // every machine the same static table; drum does not.
        let drum = catalogue::<MachineId, DRUM_PARAMS>();
        assert!(build_sheet::<MachineId, DRUM_PARAMS>(&drum).varies());

        let mi = catalogue::<MiMachineId, MI_PARAMS>();
        assert!(!build_sheet::<MiMachineId, MI_PARAMS>(&mi).varies());
    }

    #[test]
    fn markdown_and_html_both_render() {
        let md = generate_cheatsheet("drum", "markdown").unwrap();
        assert!(md.contains("### TRACK — group 0"));
        assert!(
            md.contains("| 2 | PAN | PAN | bipolar"),
            "pan is TRACK index 2"
        );
        assert!(
            !md.contains("| CC |") || md.contains("| 99 |"),
            "no CC map table"
        );
        let html = generate_cheatsheet("drum", "html").unwrap();
        assert!(html.contains("<h3>TRACK — group 0</h3>"));
        assert!(html.ends_with("</html>\n"));
    }

    #[test]
    fn rows_are_split_into_one_table_per_group() {
        // The group is in the heading, so it is not repeated on every row.
        let md = generate_cheatsheet("mi-drum", "markdown").unwrap();
        for (name, number) in [
            ("TRACK", 0),
            ("VOICE", 1),
            ("FILTER", 2),
            ("SHAPER", 3),
            ("LFO", 5),
            ("ENV", 6),
        ] {
            assert!(
                md.contains(&format!("### {name} — group {number}")),
                "no table for {name}"
            );
        }
        // One table header per group present, and no composite addresses.
        assert_eq!(md.matches("| Index | Name |").count(), 6);
        assert!(!md.contains("(VOICE, "), "address column should be gone");

        // A group this device places no rows in gets no empty table.
        assert!(!md.contains("AMP — group 4"));
    }

    #[test]
    fn a_machine_with_one_group_gets_one_table() {
        // Hat Classic has VOICE and FILTER rows of its own and nothing
        // else: it should get two tables, not seven with five empty.
        let md = generate_cheatsheet("drum", "markdown").unwrap();
        let section = md
            .split("## Hat Classic")
            .nth(1)
            .and_then(|s| s.split("\n## ").next())
            .expect("a Hat Classic section");
        assert_eq!(section.matches("| Index | Name |").count(), 2, "{section}");
        assert!(section.contains("### VOICE — group 1"));
        assert!(section.contains("### FILTER — group 2"));
    }

    #[test]
    fn mi_drum_renders_as_one_table_with_a_catalogue() {
        let md = generate_cheatsheet("mi-drum", "markdown").unwrap();
        assert!(md.contains("MI-Drum Param Cheat-Sheet"));
        assert!(md.contains("## Parameters"));
        assert!(
            !md.contains("## Common Parameters"),
            "one shared table should not be split into common + per-machine"
        );
        // Every machine is reachable as a MACH option.
        for m in MiMachineId::ALL {
            assert!(md.contains(m.name()), "{} missing from catalogue", m.name());
        }
        // The strip rows only mi-drum has.
        assert!(md.contains("SHAPE.MIX"));
        assert!(md.contains("RIP.CUT"));

        let html = generate_cheatsheet("mi-drum", "html").unwrap();
        assert!(html.ends_with("</html>\n"));
        assert!(html.contains("id=\"params\""));
        assert!(!html.contains("id=\"common\""));
    }

    #[test]
    fn an_unknown_device_is_an_error() {
        assert!(generate_cheatsheet("nope", "markdown").is_err());
    }

    #[test]
    fn every_section_carries_a_diagram() {
        // One per machine plus one for the strip. A section without one is
        // a machine whose signal path the sheet does not show.
        let md = generate_cheatsheet("drum", "markdown").unwrap();
        assert_eq!(
            md.matches("```mermaid").count(),
            MachineId::COUNT + 1,
            "expected a diagram per machine plus the strip"
        );

        // mi-drum draws one path, because it has one.
        let mi = generate_cheatsheet("mi-drum", "markdown").unwrap();
        assert_eq!(mi.matches("```mermaid").count(), 1);
    }

    #[test]
    fn modulation_is_a_table_not_an_edge() {
        // Decision from #17: a destination chosen by a param value has no
        // fixed edge, so the LFOs are described in a table instead. If they
        // ever appear as graph edges, that decision has been undone.
        let md = generate_cheatsheet("drum", "markdown").unwrap();
        assert!(md.contains("| Modulator | Destination | Params |"));
        assert!(md.contains("`LFO1.DST`"));
        let diagrams: String = md
            .split("```mermaid")
            .skip(1)
            .filter_map(|s| s.split("```").next())
            .collect();
        assert!(
            !diagrams.contains("LFO1.DST"),
            "selectable destinations should not be drawn as edges"
        );
    }

    #[test]
    fn html_diagrams_are_collapsed_and_degrade_without_the_cdn() {
        let html = generate_cheatsheet("drum", "html").unwrap();
        // Collapsed: `<details>` with no `open` attribute.
        assert!(html.contains("<details class=\"diagram\">"));
        assert!(!html.contains("<details class=\"diagram\" open"));
        // The source sits in the DOM, so an offline reader sees the graph
        // as text rather than an empty panel.
        assert!(html.contains("<pre class=\"mermaid\">"));
        assert!(html.contains("flowchart LR"));
        assert!(html.contains("cdn.jsdelivr.net"));
    }

    #[test]
    fn mermaid_source_is_escaped_for_the_html_page() {
        // `<br/>` in a node label would end the <pre> early if it went in
        // raw. Mermaid reads textContent, so the escaped form round-trips.
        let html = generate_cheatsheet("drum", "html").unwrap();
        let pre = html
            .split("<pre class=\"mermaid\">")
            .nth(1)
            .and_then(|s| s.split("</pre>").next())
            .expect("a diagram");
        assert!(pre.contains("&lt;br/&gt;"), "label breaks must be escaped");
        assert!(!pre.contains("<br/>"));
    }
}
