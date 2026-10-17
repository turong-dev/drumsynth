//! `render cheatsheet` — the parameter reference, generated from the engine.
//!
//! Organised the way the control surface is: by [`ParamGroup`], with each
//! row's NRPN address `(group, index)` as its identity. That address is the
//! only external name a parameter has — the flat slot index is an internal
//! layout detail since the CC map was removed — so it is what the table
//! leads with.
//!
//! Everything here is read out of `MACHINE_INFO`, so it cannot drift from
//! the engine.

use drum_engine::machines::{MachineId, ParamGroup, ParamKind, NUM_PARAMS};

/// One row in the cheat-sheet table.
pub(crate) struct CheatRow {
    /// Group the parameter is addressed in.
    pub(crate) group: ParamGroup,
    /// Index within the group — the NRPN parameter-select LSB.
    pub(crate) index: usize,
    /// Flat index into the parameter array. Internal; shown because
    /// `MACHINE_INFO` and every `set_params` are written in these terms.
    pub(crate) slot: usize,
    pub(crate) name: &'static str,
    pub(crate) abbrev: &'static str,
    pub(crate) kind: ParamKind,
    pub(crate) default: f32,
}

impl CheatRow {
    /// `(GROUP, index)` as a controller would be configured with it.
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

/// Every addressable row of a machine's table, in group order.
///
/// `Reserved` rows are skipped: they have no address, which is the point of
/// being reserved. Within a group the rows keep their flat order, because
/// that is the order the group's indices are assigned in.
fn rows_for(machine: MachineId, keep: impl Fn(usize) -> bool) -> Vec<CheatRow> {
    let table = machine.params();
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

/// Generate the cheat-sheet in the requested format (`"markdown"` or `"html"`).
///
/// The output is split into:
/// - one **Common Parameters** table for the rows every machine shares, and
/// - one table per machine for the rows that are its own.
pub(crate) fn generate_cheatsheet(format: &str) -> String {
    let machines = MachineId::ALL;

    // A slot is common when every machine gives it the same name and it is
    // not reserved — the track-routed rows, in practice. Everything else is
    // the machine's own, which is what VOICE is for.
    let mut is_common = [false; NUM_PARAMS];
    for (slot, common) in is_common.iter_mut().enumerate() {
        let first = machines[0].params()[slot].name;
        if first != "RESV" && machines.iter().all(|m| m.params()[slot].name == first) {
            *common = true;
        }
    }

    let common_rows = rows_for(machines[0], |slot| is_common[slot]);
    let machine_rows = |m: MachineId| -> Vec<CheatRow> { rows_for(m, |slot| !is_common[slot]) };

    if format == "html" {
        generate_cheatsheet_html(machines, &common_rows, &machine_rows)
    } else {
        generate_cheatsheet_markdown(machines, &common_rows, &machine_rows)
    }
}

const HEADER: &str = "| Address | Name | Abbrev | Kind | Default | Slot |\n\
                      |---------|------|--------|------|---------|------|\n";

fn markdown_rows(out: &mut String, rows: &[CheatRow]) {
    out.push_str(HEADER);
    for row in rows {
        out.push_str(&format!(
            "| `{}` | {} | {} | {} | {} | {} |\n",
            row.address(),
            row.name,
            row.abbrev,
            row.kind_label(),
            row.default_label(),
            row.slot
        ));
    }
    out.push('\n');
}

/// Markdown output: one heading + table per section.
fn generate_cheatsheet_markdown(
    machines: [MachineId; MachineId::COUNT],
    common_rows: &[CheatRow],
    machine_rows: &impl Fn(MachineId) -> Vec<CheatRow>,
) -> String {
    let mut out = String::new();

    out.push_str("# Drum Synth Param Cheat-Sheet\n\n");
    out.push_str("Generated from `MACHINE_INFO` in the engine.\n\n");
    out.push_str(INTRO);
    out.push('\n');

    out.push_str("## Common Parameters (same on every machine)\n\n");
    out.push_str("Track-level routing, the strip, and the LFOs. Values are factory defaults.\n\n");
    markdown_rows(&mut out, common_rows);

    for m in machines {
        out.push_str(&format!("## {}\n\n", m.label()));
        markdown_rows(&mut out, &machine_rows(m));
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
**Address** is what a controller sends: `(group, index)` over NRPN.  \n\
**Slot** is the flat index into the engine's parameter array — internal, \
shown because `MACHINE_INFO` and `set_params` are written in those terms. \
Reserved slots have no address and are not listed.  \n\
**Kind**: `continuous` is a plain 0..1 sweep; `bipolar` is centred at 0.5; \
`N options` is a fixed set, where option `k` is the value `k / (N - 1)`.
";

/// HTML output: self-contained styled page.
fn generate_cheatsheet_html(
    machines: [MachineId; MachineId::COUNT],
    common_rows: &[CheatRow],
    machine_rows: &impl Fn(MachineId) -> Vec<CheatRow>,
) -> String {
    let mut out = String::new();

    out.push_str("<!DOCTYPE html>\n<html lang=\"en\">\n<head>\n");
    out.push_str("<meta charset=\"utf-8\">\n");
    out.push_str("<meta name=\"viewport\" content=\"width=device-width, initial-scale=1\">\n");
    out.push_str("<title>Drum Synth Param Cheat-Sheet</title>\n");
    out.push_str("<style>\n");
    out.push_str(CHEATSHEET_CSS);
    out.push_str("</style>\n");
    out.push_str("</head>\n<body>\n");

    out.push_str("<header>\n");
    out.push_str("<h1>Drum Synth Param Cheat-Sheet</h1>\n");
    out.push_str("<p>Generated from <code>MACHINE_INFO</code> in the engine.</p>\n");
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
    out.push_str("<li><a href=\"#common\">Common Parameters</a></li>\n");
    for m in machines {
        let id = machine_anchor(m);
        out.push_str(&format!("<li><a href=\"#{id}\">{}</a></li>\n", m.label()));
    }
    out.push_str("</ul>\n</nav>\n\n");

    out.push_str("<section id=\"common\">\n");
    out.push_str("<h2>Common Parameters <span class=\"badge\">same on every machine</span></h2>\n");
    out.push_str(
        "<p>Track-level routing, the strip, and the LFOs. Values are factory defaults.</p>\n",
    );
    html_table(&mut out, common_rows);
    out.push_str("</section>\n\n");

    for m in machines {
        let id = machine_anchor(m);
        out.push_str(&format!("<section id=\"{id}\">\n"));
        out.push_str(&format!("<h2>{}</h2>\n", m.label()));
        html_table(&mut out, &machine_rows(m));
        out.push_str("</section>\n\n");
    }

    out.push_str("<footer>\n<p><strong>Address</strong> is what a controller sends: <code>(group, index)</code> over NRPN. <strong>Slot</strong> is the flat index into the engine's parameter array — internal, shown because <code>MACHINE_INFO</code> and <code>set_params</code> are written in those terms. Reserved slots have no address and are not listed.</p>\n");
    out.push_str("</footer>\n");
    out.push_str("</body>\n</html>\n");

    out
}

fn html_table(out: &mut String, rows: &[CheatRow]) {
    out.push_str("<table>\n<thead>\n<tr><th>Address</th><th>Name</th><th>Abbrev</th><th>Kind</th><th>Default</th><th>Slot</th></tr>\n</thead>\n<tbody>\n");
    for row in rows {
        out.push_str(&format!(
            "<tr><td><code>{}</code></td><td>{}</td><td>{}</td><td>{}</td><td>{}</td><td>{}</td></tr>\n",
            row.address(),
            row.name,
            row.abbrev,
            row.kind_label(),
            row.default_label(),
            row.slot
        ));
    }
    out.push_str("</tbody>\n</table>\n");
}

fn machine_anchor(m: MachineId) -> String {
    m.name().replace('-', "_")
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
"#;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_addressable_row_appears_exactly_once() {
        // Common + machine-specific must partition the addressable rows:
        // a row in neither is a parameter the sheet cannot tell you how to
        // reach, and a row in both is an address printed twice.
        for m in MachineId::ALL {
            let mut seen: Vec<usize> = generate_rows(m);
            seen.sort_unstable();
            let addressable: Vec<usize> = m
                .params()
                .iter()
                .enumerate()
                .filter(|(_, i)| i.group as u8 != ParamGroup::Reserved as u8)
                .map(|(slot, _)| slot)
                .collect();
            assert_eq!(seen, addressable, "{}", m.label());
        }
    }

    /// Slots the generator prints for one machine, across both sections.
    fn generate_rows(m: MachineId) -> Vec<usize> {
        let machines = MachineId::ALL;
        let mut is_common = [false; NUM_PARAMS];
        for (slot, common) in is_common.iter_mut().enumerate() {
            let first = machines[0].params()[slot].name;
            if first != "RESV" && machines.iter().all(|x| x.params()[slot].name == first) {
                *common = true;
            }
        }
        let mut out: Vec<usize> = rows_for(machines[0], |s| is_common[s])
            .iter()
            .map(|r| r.slot)
            .collect();
        out.extend(rows_for(m, |s| !is_common[s]).iter().map(|r| r.slot));
        out
    }

    #[test]
    fn addresses_match_what_resolve_finds() {
        // The sheet's `(group, index)` has to be the address the engine
        // resolves, or it is a lie that nothing else would catch.
        use drum_engine::machines::{resolve, ParamId};
        for m in MachineId::ALL {
            for row in rows_for(m, |_| true) {
                let flat = resolve(
                    m.params(),
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
    fn the_machine_selector_prints_as_an_option() {
        let rows = rows_for(MachineId::ALL[0], |_| true);
        let mach = rows.iter().find(|r| r.name == "MACH").unwrap();
        assert_eq!(mach.address(), "(TRACK, 0)");
        assert_eq!(mach.kind_label(), format!("{} options", MachineId::COUNT));
        assert_eq!(mach.default_label(), "option 0");
    }

    #[test]
    fn markdown_and_html_both_render() {
        let md = generate_cheatsheet("markdown");
        assert!(md.contains("`(TRACK, 2)`"), "pan should be TRACK 2");
        assert!(
            !md.contains("| CC |") || md.contains("| 99 |"),
            "no CC map table"
        );
        let html = generate_cheatsheet("html");
        assert!(html.contains("(TRACK, 2)"));
        assert!(html.ends_with("</html>\n"));
    }
}
