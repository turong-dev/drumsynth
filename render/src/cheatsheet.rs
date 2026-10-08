use drum_engine::machines::{MachineId, MACROS_PER_BANK, NUM_MACROS};

/// One row in the cheat-sheet table.
pub(crate) struct CheatRow {
    pub(crate) slot: usize,
    pub(crate) cc: usize,
    pub(crate) bank: &'static str,
    pub(crate) name: &'static str,
    pub(crate) abbrev: &'static str,
    pub(crate) default: f32,
    pub(crate) is_resv: bool,
}

pub(crate) trait CheatsheetRow {
    fn slot(&self) -> usize;
    fn cc(&self) -> usize;
    fn bank(&self) -> &str;
    fn name(&self) -> &str;
    fn abbrev(&self) -> &str;
    fn default(&self) -> f32;
    fn is_resv(&self) -> bool;
}

impl CheatsheetRow for CheatRow {
    fn slot(&self) -> usize {
        self.slot
    }
    fn cc(&self) -> usize {
        self.cc
    }
    fn bank(&self) -> &str {
        self.bank
    }
    fn name(&self) -> &str {
        self.name
    }
    fn abbrev(&self) -> &str {
        self.abbrev
    }
    fn default(&self) -> f32 {
        self.default
    }
    fn is_resv(&self) -> bool {
        self.is_resv
    }
}

/// Generate the cheat-sheet in the requested format (`"markdown"` or `"html"`).
///
/// The output is split into:
/// - one **Common Parameters** table for track-routed slots (same on every
///   machine), and
/// - one table per machine for its machine-internal slots.
///
/// Both are derived from `MACHINE_INFO` in the engine, so they can't drift.
pub(crate) fn generate_cheatsheet(format: &str) -> String {
    let machines = MachineId::ALL;

    // Classify each slot as track-routed (same name on every machine, not
    // RESV) or machine-internal.
    let mut is_track = [false; NUM_MACROS];
    for (slot, track) in is_track.iter_mut().enumerate() {
        let first = machines[0].macros()[slot].name;
        if first != "RESV" && machines.iter().all(|m| m.macros()[slot].name == first) {
            *track = true;
        }
    }

    let bank_name = |slot: usize| match slot / MACROS_PER_BANK {
        0 => "MACH",
        1 => "FILT",
        2 => "TRACK",
        3 => "MOD",
        _ => "????",
    };

    let rows_for = |machine: Option<MachineId>, slots: &[usize]| -> Vec<CheatRow> {
        slots
            .iter()
            .map(|&slot| {
                let m = machine.unwrap_or(machines[0]);
                let info = m.macros()[slot];
                CheatRow {
                    slot,
                    cc: 20 + slot,
                    bank: bank_name(slot),
                    name: info.name,
                    abbrev: info.abbrev,
                    default: info.default,
                    is_resv: info.name == "RESV",
                }
            })
            .collect()
    };

    let track_slots: Vec<usize> = (0..NUM_MACROS).filter(|&s| is_track[s]).collect();
    let machine_slots: Vec<usize> = (0..NUM_MACROS).filter(|&s| !is_track[s]).collect();

    let common_rows = rows_for(None, &track_slots);

    if format == "html" {
        generate_cheatsheet_html(machines, &common_rows, &machine_slots, &rows_for)
    } else {
        generate_cheatsheet_markdown(machines, &common_rows, &machine_slots, &rows_for)
    }
}

/// Markdown output: one heading + table per section.
fn generate_cheatsheet_markdown(
    machines: [MachineId; MachineId::COUNT],
    common_rows: &[impl CheatsheetRow],
    machine_slots: &[usize],
    rows_for: &impl Fn(Option<MachineId>, &[usize]) -> Vec<CheatRow>,
) -> String {
    let mut out = String::new();

    out.push_str("# Drum Synth Macro Cheat-Sheet\n\n");
    out.push_str("Generated from `MACHINE_INFO` in the engine.\n\n");

    // Common table
    out.push_str("## Common Parameters (track-routed)\n\n");
    out.push_str("Same meaning on every machine. Values are factory defaults (0..1).\n\n");
    out.push_str("| Slot | CC | Bank | Name | Abbrev | Default |\n");
    out.push_str("|------|-----|------|------|--------|---------|\n");
    for row in common_rows {
        out.push_str(&format!(
            "| {} | {} | {} | {} | {} | {} |\n",
            row.slot(),
            row.cc(),
            row.bank(),
            row.name(),
            row.abbrev(),
            if row.is_resv() {
                "RESV".to_string()
            } else {
                format!("{:.2}", row.default())
            }
        ));
    }
    out.push('\n');

    // Per-machine tables
    for m in machines {
        let rows = rows_for(Some(m), machine_slots);
        out.push_str(&format!("## {}\n\n", m.label()));
        out.push_str("| Slot | CC | Bank | Name | Abbrev | Default |\n");
        out.push_str("|------|-----|------|------|--------|---------|\n");
        for row in &rows {
            out.push_str(&format!(
                "| {} | {} | {} | {} | {} | {} |\n",
                row.slot(),
                row.cc(),
                row.bank(),
                row.name(),
                row.abbrev(),
                if row.is_resv() {
                    "RESV".to_string()
                } else {
                    format!("{:.2}", row.default())
                }
            ));
        }
        out.push('\n');
    }

    out.push_str("---\n\n");
    out.push_str("**RESV** = reserved/unused on that machine.  \n");
    out.push_str("CC = MIDI CC number (20 + slot).  \n");
    out.push_str("Bank: MACH (0–7), FILT (8–15), TRACK (16–23), MOD (24–31).\n");

    out
}

/// HTML output: self-contained styled page.
fn generate_cheatsheet_html(
    machines: [MachineId; MachineId::COUNT],
    common_rows: &[impl CheatsheetRow],
    machine_slots: &[usize],
    rows_for: &impl Fn(Option<MachineId>, &[usize]) -> Vec<CheatRow>,
) -> String {
    let mut out = String::new();

    out.push_str("<!DOCTYPE html>\n<html lang=\"en\">\n<head>\n");
    out.push_str("<meta charset=\"utf-8\">\n");
    out.push_str("<meta name=\"viewport\" content=\"width=device-width, initial-scale=1\">\n");
    out.push_str("<title>Drum Synth Macro Cheat-Sheet</title>\n");
    out.push_str("<style>\n");
    out.push_str(CHEATSHEET_CSS);
    out.push_str("</style>\n");
    out.push_str("</head>\n<body>\n");

    out.push_str("<header>\n");
    out.push_str("<h1>Drum Synth Macro Cheat-Sheet</h1>\n");
    out.push_str("<p>Generated from <code>MACHINE_INFO</code> in the engine.</p>\n");
    out.push_str("</header>\n\n");

    // Nav
    out.push_str("<nav class=\"toc\">\n<h2>Contents</h2>\n<ul>\n");
    out.push_str("<li><a href=\"#common\">Common Parameters</a></li>\n");
    for m in machines {
        let id = machine_anchor(m);
        out.push_str(&format!("<li><a href=\"#{id}\">{}</a></li>\n", m.label()));
    }
    out.push_str("</ul>\n</nav>\n\n");

    // Common table
    out.push_str("<section id=\"common\">\n");
    out.push_str("<h2>Common Parameters <span class=\"badge\">track-routed</span></h2>\n");
    out.push_str("<p>Same meaning on every machine. Values are factory defaults (0..1).</p>\n");
    out.push_str("<table>\n<thead>\n<tr><th>Slot</th><th>CC</th><th>Bank</th><th>Name</th><th>Abbrev</th><th>Default</th></tr>\n</thead>\n<tbody>\n");
    for row in common_rows {
        out.push_str(&html_row(row));
    }
    out.push_str("</tbody>\n</table>\n</section>\n\n");

    // Per-machine tables
    for m in machines {
        let rows = rows_for(Some(m), machine_slots);
        let id = machine_anchor(m);
        out.push_str(&format!("<section id=\"{id}\">\n"));
        out.push_str(&format!("<h2>{}</h2>\n", m.label()));
        out.push_str("<table>\n<thead>\n<tr><th>Slot</th><th>CC</th><th>Bank</th><th>Name</th><th>Abbrev</th><th>Default</th></tr>\n</thead>\n<tbody>\n");
        for row in &rows {
            out.push_str(&html_row(row));
        }
        out.push_str("</tbody>\n</table>\n</section>\n\n");
    }

    out.push_str("<footer>\n<p><strong>RESV</strong> = reserved/unused. CC = MIDI CC (20 + slot). Bank: MACH (0–7), FILT (8–15), TRACK (16–23), MOD (24–31).</p>\n");
    out.push_str("</footer>\n");
    out.push_str("</body>\n</html>\n");

    out
}

fn html_row(row: &impl CheatsheetRow) -> String {
    let class = if row.is_resv() { " class=\"resv\"" } else { "" };
    let default = if row.is_resv() {
        "RESV".to_string()
    } else {
        format!("{:.2}", row.default())
    };
    format!(
        "<tr{}><td>{}</td><td>{}</td><td>{}</td><td>{}</td><td>{}</td><td>{}</td></tr>\n",
        class,
        row.slot(),
        row.cc(),
        row.bank(),
        row.name(),
        row.abbrev(),
        default
    )
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
