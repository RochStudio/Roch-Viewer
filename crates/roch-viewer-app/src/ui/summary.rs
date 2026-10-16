//! The Summary tab: the About panels across the top, then three bordered
//! timing columns.
//!
//! The first About panel is the identity -- CPU, cores, microcode; board and
//! BIOS -- packed tight so a long board name stays readable, each entry
//! pushed out to the start of the timing column under it where it can be.
//! The second is the clock block, one entry per Summary column, so DRAM
//! Frequency starts where tCL does and ends where its value does.
//!
//! Below them: the primary and secondary timings with the four RTL rows; the
//! tertiary turnarounds (with the PHY rows and the DIMM rails on AM5); and
//! the voltage snapshot followed by the RTT, ODT and RON groups.
//!
//! [`summary_plan`] decides all of that from the rows by name, without
//! drawing; [`show`] measures and paints it.

use std::collections::{BTreeMap, BTreeSet};

use eframe::egui::{self, Color32, Rect, Ui, pos2};

use rochviewer::display_values::{module_pair_text, summary_compact_ohm};
use rochviewer::platform_profiles::Platform;
use rochviewer::row::Row;

use super::draw::{self, COLUMN_GAP, DETAIL_COLUMN_GAP, Edge, PANEL_GAP, PANEL_PADX, PANEL_PADY};
use super::layout::{
    AM5_SUMMARY_OMITTED, AM5_SUMMARY_PHY_NAMES, AM5_SUMMARY_TIMING_PRIORITY, INTEL_DDR5_SUMMARY_ADDED,
    INTEL_DDR5_SUMMARY_OMITTED, SUMMARY_ABOUT_LABELS, SUMMARY_BASE_COLUMNS, SUMMARY_COLUMN_TAIL, SUMMARY_DIMM_RAIL_CATEGORIES,
    SUMMARY_SIGNAL_GROUPS, SUMMARY_STATIC_ROWS, SummaryEntry, am5_summary_timing_columns, insert_summary_rows_after,
    intel_summary_timing_columns, row_named, summary_signal_timings, summary_snapshot_voltage_rows, summary_system_memory_blocks,
    summary_system_memory_names,
};
use super::tabs::TabContext;
use super::theme;

/// How a timing-column row reads its value.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Reading {
    /// `_read_compact_value`, dimmed when the row says so.
    Compact,
    /// The signal groups: a dual row as the Training tab shows it.
    Signal,
    /// The AM5 PHY block: always the pair, ohms compacted.
    Dual,
}

/// One line of a Summary timing column.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Line {
    Row { index: usize, label: String, reading: Reading },
    /// Two rows slashed together, or the first alone.
    Rows { label: String, first: Option<usize>, second: Option<usize>, has_second: bool },
    /// One dual row's own A and B.
    Sides { label: String, index: Option<usize> },
}

/// One About entry: the row and the label it shows (empty for none).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct AboutPair {
    pub index: usize,
    pub label: String,
}

/// Everything the Summary draws, by row index.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct SummaryPlan {
    /// The rows packed tight: CPU / Cores / Microcode, Model / BIOS, AGESA.
    pub identity: Vec<Vec<AboutPair>>,
    /// The rows on the Summary columns, a hole as `None`.
    pub aligned: Vec<Vec<Option<AboutPair>>>,
    /// The three timing columns.
    pub columns: Vec<Vec<Line>>,
}

impl SummaryPlan {
    /// Every row the Summary shows, in the order it draws them.
    pub fn row_indices(&self) -> Vec<usize> {
        let mut out = Vec::new();
        out.extend(self.identity.iter().flatten().map(|pair| pair.index));
        out.extend(self.aligned.iter().flatten().flatten().map(|pair| pair.index));
        for line in self.columns.iter().flatten() {
            match line {
                Line::Row { index, .. } => out.push(*index),
                Line::Rows { first, second, .. } => out.extend(first.iter().chain(second.iter()).copied()),
                Line::Sides { index, .. } => out.extend(index.iter().copied()),
            }
        }
        out
    }
}

/// The rows the About panel draws from, by name.
///
/// The Telemetry copy of a row shares its name and is appended after it, so
/// it would win here and put the Summary on the live refresh. For the static
/// rows the Summary keeps the reading taken once; otherwise the last row of
/// a name wins, as it did when the Python built a dict from the table.
fn about_rows(rows: &[Row]) -> BTreeMap<&'static str, usize> {
    let eligible = summary_system_memory_names();
    let mut by_name: BTreeMap<&'static str, usize> = BTreeMap::new();
    for (index, row) in rows.iter().enumerate() {
        let Some(name) = eligible.iter().find(|n| **n == row.name) else { continue };
        if SUMMARY_STATIC_ROWS.contains(name) && row.live && by_name.contains_key(name) {
            continue;
        }
        by_name.insert(name, index);
    }
    by_name
}

fn about_label(row: &Row) -> String {
    SUMMARY_ABOUT_LABELS
        .iter()
        .find(|(name, _)| *name == row.name)
        .map(|(_, label)| label.to_string())
        .unwrap_or_else(|| row.label().to_string())
}

fn entry_lines(rows: &[Row], entries: &[SummaryEntry], labels: &BTreeMap<String, String>, reading: Reading) -> Vec<Line> {
    let mut lines = Vec::new();
    for entry in entries {
        match entry {
            SummaryEntry::Name(name) => {
                // Keep the exact requested order; a name with no row is
                // skipped.
                if let Some(index) = row_named(rows, name) {
                    let label = labels.get(name).cloned().unwrap_or_else(|| rows[index].name.clone());
                    lines.push(Line::Row { index, label, reading });
                }
            }
            SummaryEntry::Rows { label, first, second } => {
                let first_index = row_named(rows, first);
                let second_index = second.and_then(|n| row_named(rows, n));
                if first_index.is_some() || second_index.is_some() {
                    lines.push(Line::Rows {
                        label: label.to_string(),
                        first: first_index,
                        second: second_index,
                        has_second: second.is_some(),
                    });
                }
            }
            SummaryEntry::Sides { label, name } => {
                if let Some(index) = row_named(rows, name) {
                    lines.push(Line::Sides { label: label.to_string(), index: Some(index) });
                }
            }
        }
    }
    lines
}

/// Build the full-width system strip and dense three-column Summary view.
pub fn summary_plan(rows: &[Row], platform: Platform) -> SummaryPlan {
    let mut plan = SummaryPlan::default();

    let about = about_rows(rows);
    let available: BTreeSet<&str> = about.keys().copied().collect();
    let pair = |name: &str| about.get(name).map(|&index| AboutPair { index, label: about_label(&rows[index]) });
    for (names, aligned) in summary_system_memory_blocks(&available) {
        // An aligned row is laid out on the Summary columns, so its first
        // entry starts where tCL does, its second where tREFI does, and its
        // third where RTT WR does. Every other row packs tight, each entry
        // beginning where the previous value ended.
        if aligned && names.len() > 1 && names.len() <= SUMMARY_BASE_COLUMNS {
            plan.aligned.push(names.iter().map(|name| name.and_then(pair)).collect());
        } else {
            plan.identity.push(names.iter().flatten().filter_map(|name| pair(name)).collect());
        }
    }

    let names: BTreeSet<&str> = rows.iter().map(|row| row.name.as_str()).collect();
    let am5 = names.contains("Gear Down Mode");
    let (mut primary_secondary, tertiary): (Vec<SummaryEntry>, Vec<SummaryEntry>) = if am5 {
        let (first, leftover) = am5_summary_timing_columns(rows);
        (first.iter().map(|n| SummaryEntry::name(n)).collect(), leftover.iter().map(|n| SummaryEntry::name(n)).collect())
    } else {
        let ddr5 = platform == Platform::Lga1700Ddr5;
        intel_summary_timing_columns(
            rows,
            if ddr5 { INTEL_DDR5_SUMMARY_OMITTED } else { &[] },
            if ddr5 { INTEL_DDR5_SUMMARY_ADDED } else { &[] },
        )
    };
    // The command rate reads as a timing, so it belongs under tRC rather
    // than in the system strip. Named both ways because the platforms spell
    // the row differently; the one that does not exist here is skipped when
    // the section looks its row up.
    primary_secondary = insert_summary_rows_after(primary_secondary, "tRC", vec![SummaryEntry::name("CR")]);

    // Always pin the precharge/mode/power-down group at the end of column 0
    // so those rows cannot disappear if category filters drift. The order
    // comes from AM5_SUMMARY_TIMING_PRIORITY rather than being written again
    // here, and an omitted row stays omitted.
    let tail: Vec<&str> = AM5_SUMMARY_TIMING_PRIORITY
        .iter()
        .copied()
        .filter(|n| SUMMARY_COLUMN_TAIL.contains(n) && names.contains(n) && !AM5_SUMMARY_OMITTED.contains(n))
        .collect();
    if am5 && !tail.is_empty() {
        primary_secondary.retain(|e| !e.as_name().is_some_and(|n| tail.contains(&n)));
        primary_secondary.extend(tail.iter().map(|n| SummaryEntry::name(n)));
    }

    let none = BTreeMap::new();
    let first = entry_lines(rows, &primary_secondary, &none, Reading::Compact);
    let mut middle = entry_lines(rows, &tertiary, &none, Reading::Compact);
    let mut third = Vec::new();

    // Dual ChA/ChB PHY block under tertiary; the omitted ones are dropped.
    let phy: Vec<SummaryEntry> = AM5_SUMMARY_PHY_NAMES
        .iter()
        .filter(|n| names.contains(*n) && !AM5_SUMMARY_OMITTED.contains(n))
        .map(|n| SummaryEntry::name(n))
        .collect();
    if am5 && !phy.is_empty() {
        middle.extend(entry_lines(rows, &phy, &none, Reading::Dual));
    }

    let voltage_lines = |indices: &[usize]| -> Vec<Line> {
        let entries: Vec<SummaryEntry> = indices.iter().map(|&i| SummaryEntry::name(&rows[i].name)).collect();
        let labels: BTreeMap<String, String> = indices.iter().map(|&i| (rows[i].name.clone(), rows[i].label().to_string())).collect();
        entry_lines(rows, &entries, &labels, Reading::Compact)
    };
    let mut snapshot = summary_snapshot_voltage_rows(rows);
    // On AM5 the DIMMs' own PMIC rails close the middle column, under
    // tPHYRDL: the middle column is the short one, and the rails are
    // per-module readings rather than the CPU's. The CPU and board rails
    // keep the head of the third column.
    let dimm: Vec<usize> =
        snapshot.iter().copied().filter(|&i| SUMMARY_DIMM_RAIL_CATEGORIES.contains(&rows[i].category.as_str())).collect();
    if !dimm.is_empty() && am5 {
        snapshot.retain(|i| !dimm.contains(i));
        middle.extend(voltage_lines(&dimm));
    }
    if !snapshot.is_empty() {
        third.extend(voltage_lines(&snapshot));
    }
    // Summary keeps the high-level electrical groups: Voltages, RTT, ODT and
    // RON. The IMC VREF levels are on IMC, with the per-device DQ VREF and
    // the rest.
    let signal = summary_signal_timings(rows);
    for group in SUMMARY_SIGNAL_GROUPS {
        for &index in &signal {
            if group.contains(&rows[index].category.as_str()) {
                third.push(Line::Row { index, label: rows[index].name.clone(), reading: Reading::Signal });
            }
        }
    }
    plan.columns = vec![first, middle, third];
    plan
}

/// The text and colour of one timing-column line.
fn line_value(ctx: &TabContext, line: &Line) -> (String, String, Color32) {
    let palette = ctx.palette;
    match line {
        Line::Row { index, label, reading } => {
            let row = &ctx.rows()[*index];
            match reading {
                Reading::Compact => (label.clone(), ctx.compact_value(*index), ctx.value_color(*index)),
                Reading::Signal => {
                    let value = if row.is_dual() { ctx.detail_channel_text(*index) } else { ctx.compact_value(*index) };
                    (label.clone(), value, palette.value)
                }
                Reading::Dual => {
                    let value = match (row.is_dual(), ctx.selected) {
                        (true, Some(channel)) => summary_compact_ohm(&ctx.side(*index, channel)),
                        (true, None) => match ctx.values.sides(*index) {
                            Some((a, b)) => module_pair_text(&a, &b),
                            None => ctx.values.text(*index),
                        },
                        (false, _) => summary_compact_ohm(&ctx.compact_value(*index)),
                    };
                    (label.clone(), value, palette.value)
                }
            }
        }
        Line::Rows { label, first, second, has_second } => {
            let read = |index: &Option<usize>| index.map_or_else(|| "N/A".to_string(), |i| ctx.compact_value(i));
            let value = if *has_second { format!("{}/{}", read(first), read(second)) } else { read(first) };
            (label.clone(), value, palette.value)
        }
        Line::Sides { label, index } => {
            let value = match (index, ctx.selected) {
                (None, _) => "N/A".to_string(),
                (Some(i), Some(channel)) => ctx.side(*i, channel),
                (Some(i), None) => match ctx.values.sides(*i) {
                    Some((a, b)) => module_pair_text(&a, &b),
                    None => ctx.values.text(*i),
                },
            };
            (label.clone(), value, palette.value)
        }
    }
}

/// Draw the Summary into `area`, the content rect under the tab strip.
/// The timing columns' text and where each column starts and its values end.
struct Columns {
    texts: Vec<Vec<(String, String, Color32)>>,
    edges: Vec<f32>,
    starts: Vec<f32>,
}

fn measure(painter: &egui::Painter, ctx: &TabContext, plan: &SummaryPlan, left: f32) -> Columns {
    let font = theme::font(theme::FONT_SIZE);
    let width = |text: &str| draw::text_width(painter, text, &font);
    // Each timing column's lines as text.
    let texts: Vec<Vec<(String, String, Color32)>> =
        plan.columns.iter().map(|lines| lines.iter().map(|line| line_value(ctx, line)).collect()).collect();

    // A column's names are as wide as its longest name -- timing or clock --
    // and its values end where its widest value does, so the clock values
    // and the timing values under them stop on one edge.
    let count = texts.len();
    let mut names_w = vec![0.0f32; count];
    let mut values_w = vec![0.0f32; count];
    // Measured in the all-modules view as well: the Tk grid sized its
    // columns once, at build, and a selection never shrank them.
    let all = ctx.all_modules();
    for (c, lines) in plan.columns.iter().enumerate() {
        for (line, (label, value, _)) in lines.iter().zip(&texts[c]) {
            names_w[c] = names_w[c].max(width(label));
            values_w[c] = values_w[c].max(width(value));
            if ctx.selected.is_some() {
                values_w[c] = values_w[c].max(width(&line_value(&all, line).1));
            }
        }
    }
    for row in &plan.aligned {
        for (c, pair) in row.iter().enumerate().take(count) {
            if let Some(pair) = pair {
                names_w[c] = names_w[c].max(width(&pair.label));
                values_w[c] = values_w[c].max(width(&ctx.compact_value(pair.index)));
            }
        }
    }
    let edges: Vec<f32> = (0..count).map(|c| names_w[c] + COLUMN_GAP + values_w[c]).collect();
    let mut starts = Vec::with_capacity(count);
    let mut x = left + PANEL_PADX;
    for edge in &edges {
        starts.push(x);
        // Rounded up to an even width; see tabs::even_text_width.
        x += edge + DETAIL_COLUMN_GAP + ((edge + DETAIL_COLUMN_GAP).round() as i64 % 2) as f32;
    }
    Columns { texts, edges, starts }
}

/// What the Summary needs: its columns' width at their content's size, and
/// the height of its panels.
#[cfg_attr(not(test), allow(dead_code))]
pub fn needed_size(ui: &Ui, ctx: &TabContext, plan: &SummaryPlan) -> egui::Vec2 {
    let columns = measure(ui.painter(), ctx, plan, 0.0);
    let width = match (columns.starts.last(), columns.edges.last()) {
        (Some(start), Some(edge)) => start + edge + PANEL_PADX,
        _ => 0.0,
    };
    let panel = |rows: usize| 2.0 * PANEL_PADY + theme::ROW_HEIGHT * rows as f32;
    let mut height = panel(plan.columns.iter().map(Vec::len).max().unwrap_or(0));
    for rows in [plan.identity.len(), plan.aligned.len()] {
        if rows > 0 {
            height += panel(rows) + PANEL_GAP;
        }
    }
    egui::vec2(width, height)
}

/// Draw the Summary into `area`, the content rect under the tab strip.
pub fn show(ui: &mut Ui, ctx: &TabContext, plan: &SummaryPlan, area: Rect) {
    let palette = ctx.palette;
    let font = theme::font(theme::FONT_SIZE);
    let painter = ui.painter_at(area);
    let width = |text: &str| draw::text_width(&painter, text, &font);
    let pitch = theme::ROW_HEIGHT;
    let Columns { texts: columns, edges, starts } = measure(&painter, ctx, plan, area.left());
    let count = columns.len();
    let about_text = |pair: &AboutPair| (pair.label.clone(), ctx.compact_value(pair.index));

    let mut top = area.top();
    let full = |top: f32, rows: usize| {
        Rect::from_min_max(pos2(area.left(), top), pos2(area.right(), top + 2.0 * PANEL_PADY + pitch * rows as f32))
    };

    // The identity panel: each pair packed after the last, its name pushed
    // out to the start of the timing column under it when it falls short.
    // A value already wider than its column -- a long board name -- is left
    // where it is rather than cut.
    if !plan.identity.is_empty() {
        let panel = full(top, plan.identity.len());
        draw::panel(&painter, panel, palette);
        for (n, pairs) in plan.identity.iter().enumerate() {
            let rect = draw::row_rect(area.left(), area.right(), top + PANEL_PADY, pitch, n);
            let mut x = starts[0];
            for (i, pair) in pairs.iter().enumerate() {
                if i > 0 && i < starts.len() {
                    x = x.max(starts[i]);
                }
                let (label, value) = about_text(pair);
                if !label.is_empty() {
                    draw::text_in_row(&painter, rect, x, Edge::Left, &label, &font, palette.text);
                    x += width(&label) + COLUMN_GAP;
                }
                draw::text_in_row(&painter, rect, x, Edge::Left, &value, &font, palette.value);
                x += width(&value) + DETAIL_COLUMN_GAP;
            }
        }
        top = panel.bottom() + PANEL_GAP;
    }

    // The clock block, on the columns.
    if !plan.aligned.is_empty() {
        let panel = full(top, plan.aligned.len());
        draw::panel(&painter, panel, palette);
        for (n, row) in plan.aligned.iter().enumerate() {
            let rect = draw::row_rect(area.left(), area.right(), top + PANEL_PADY, pitch, n);
            for (c, pair) in row.iter().enumerate().take(count) {
                let Some(pair) = pair else { continue };
                let (label, value) = about_text(pair);
                draw::text_in_row(&painter, rect, starts[c], Edge::Left, &label, &font, palette.text);
                draw::text_in_row(&painter, rect, starts[c] + edges[c], Edge::Right, &value, &font, palette.value);
            }
        }
        top = panel.bottom() + PANEL_GAP;
    }

    // The timing columns, each on its own panel, all as tall as the tallest.
    let rows = columns.iter().map(Vec::len).max().unwrap_or(0);
    let bottom = top + 2.0 * PANEL_PADY + pitch * rows as f32;
    for (c, lines) in columns.iter().enumerate() {
        let left = starts[c] - PANEL_PADX;
        let right = if c + 1 == count { area.right() } else { starts[c + 1] - PANEL_PADX - super::draw::PANEL_COLUMN_SPACING };
        let panel = Rect::from_min_max(pos2(left, top), pos2(right, bottom));
        draw::panel(&painter, panel, palette);
        for (n, (label, value, color)) in lines.iter().enumerate() {
            let rect = draw::row_rect(starts[c], right - PANEL_PADX, top + PANEL_PADY, pitch, n);
            draw::text_in_row(&painter, rect, starts[c], Edge::Left, label, &font, palette.text);
            draw::text_in_row(&painter, rect, starts[c] + edges[c], Edge::Right, value, &font, *color);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use rochviewer::row::{Side, Value};

    fn r(name: &str, category: &str, tab: &str) -> Row {
        Row::new(name, category, tab, "Left").with_value("1")
    }

    fn lga1700_ddr5_rows() -> Vec<Row> {
        let mut rows = vec![
            r("CPU", "Processor", "System Info"),
            r("Cores / Threads", "Processor", "System Info"),
            r("Microcode", "Processor", "System Info"),
            r("Manufacturer", "Motherboard", "System Info"),
            r("Model", "Motherboard", "System Info"),
            r("BIOS", "Motherboard", "System Info"),
            r("DRAM Frequency", "Clocks", "System Info"),
            r("QCLK Ratio", "Clocks", "System Info"),
            r("MCLK", "Clocks", "System Info"),
            r("UCLK", "Clocks", "System Info"),
            r("Gear Mode", "Clocks", "System Info"),
            r("Memory Capacity", "System", "SPD"),
            r("Power Down", "Features", "Training"),
            r("Uncore", "General", "Summary").with_display_name("Ring Clock"),
            r("BCLK", "Frequency", "CPU"),
        ];
        for name in ["tCL", "tRCD", "tRP", "tRAS", "tRC", "CR"] {
            rows.push(r(name, "Primary", "Timings"));
        }
        for name in ["tWR", "tRTP", "tCWL"] {
            rows.push(r(name, "Secondary", "Timings"));
        }
        for name in ["tREFI", "tREFIx9", "tRFCns", "tRFC2", "tRFCpb"] {
            rows.push(r(name, "Refresh timings", "Timings"));
        }
        for name in ["tRDRD_sg", "tWRWR_dd"] {
            rows.push(r(name, "Tertiary", "Timings"));
        }
        for name in ["tCKE", "tXP"] {
            rows.push(r(name, "Power down", "Timings"));
        }
        rows.push(r("tMOD", "Command", "Timings"));
        for name in ["RTL MC0 CHA R0", "RTL MC0 CHB R0", "RTL MC1 CHA R0", "RTL MC1 CHB R0"] {
            rows.push(r(name, "RTL", "RTL"));
        }
        rows.push(r("Vcore snapshot", "CPU and motherboard", "Voltages").with_display_name("Vcore"));
        rows.push(r("Reading mode", "Snapshot", "Voltages"));
        rows.push(
            Row::new("RTT Wr", "RTT", "Training", "Left")
                .with_sides(Side::value(Value::text("40")), Side::value(Value::text("40"))),
        );
        rows.push(r("RTT Loopback", "RTT", "Training"));
        rows.push(r("CA ODT Group A", "ODT", "Training"));
        rows.push(r("Pull Up Drv", "RON", "Training"));
        rows
    }

    fn line_labels(lines: &[Line]) -> Vec<&str> {
        lines
            .iter()
            .map(|line| match line {
                Line::Row { label, .. } | Line::Rows { label, .. } | Line::Sides { label, .. } => label.as_str(),
            })
            .collect()
    }

    #[test]
    fn the_intel_ddr5_summary_reads_as_the_python_one() {
        let rows = lga1700_ddr5_rows();
        let plan = summary_plan(&rows, Platform::Lga1700Ddr5);
        let identity: Vec<Vec<&str>> =
            plan.identity.iter().map(|row| row.iter().map(|p| p.label.as_str()).collect()).collect();
        assert_eq!(identity, vec![vec!["CPU", "Cores / Threads", "Microcode"], vec!["Model", "BIOS"]]);
        let aligned: Vec<Vec<Option<&str>>> =
            plan.aligned.iter().map(|row| row.iter().map(|p| p.as_ref().map(|p| p.label.as_str())).collect()).collect();
        // Uncore reads as Ring on the strip, overriding its own label.
        assert_eq!(
            aligned,
            vec![
                vec![Some("DRAM Frequency"), Some("BCLK"), Some("MCLK")],
                vec![Some("Memory Capacity"), Some("QCLK Ratio"), Some("Ring")],
                vec![Some("Gear Mode"), Some("Power Down"), Some("UCLK")],
            ]
        );
        // CR under tRC, the refresh cycle rows under tWR, the RTL rows last.
        assert_eq!(
            line_labels(&plan.columns[0]),
            vec![
                "tCL", "tRCD", "tRP", "tRAS", "tRC", "CR", "tWR", "tRFCns", "tRFC2", "tRFCpb", "tRTP", "tCWL",
                "RTL MC0 CHA R0", "RTL MC0 CHB R0", "RTL MC1 CHA R0", "RTL MC1 CHB R0",
            ]
        );
        // tREFI heads the middle column; DDR5 leaves tREFIx9 and tMOD off and
        // puts tXP under tCKE.
        assert_eq!(line_labels(&plan.columns[1]), vec!["tREFI", "tCKE", "tXP", "tRDRD_sg", "tWRWR_dd"]);
        // The rails by their display names, then the signal groups by their
        // own names, without the loopback.
        assert_eq!(line_labels(&plan.columns[2]), vec!["Vcore", "RTT Wr", "CA ODT Group A", "Pull Up Drv"]);
    }

    #[test]
    fn the_static_row_keeps_the_reading_taken_once() {
        let mut rows = vec![r("DRAM Frequency", "Clocks", "System Info")];
        rows.push(r("DRAM Frequency", "Clocks", "Sensors").live());
        let plan = summary_plan(&rows, Platform::Lga1700Ddr5);
        assert_eq!(plan.aligned[0][0].as_ref().unwrap().index, 0);
        // Any other name: the later row wins.
        let rows = vec![r("MCLK", "Clocks", "System Info"), r("MCLK", "Clocks", "Sensors").live()];
        let plan = summary_plan(&rows, Platform::Lga1700Ddr5);
        assert_eq!(plan.aligned[0][2].as_ref().unwrap().index, 1);
    }

    #[test]
    fn am5_moves_the_dimm_rails_and_phy_to_the_middle() {
        let mut rows = vec![
            r("Gear Down Mode", "Controller", "Training"),
            r("AGESA", "Firmware", "System Info"),
            r("tCL", "Primary", "Timings"),
            r("tRDRDSCL", "Read to read", "Timings"),
            r("tREFI", "Refresh timings", "Timings"),
        ];
        rows.push(
            Row::new("tPHYRDL", "PHY", "Timings", "Left")
                .with_sides(Side::value(Value::text("34")), Side::value(Value::text("36"))),
        );
        rows.push(r("VDD", "CHA memory", "Voltages"));
        rows.push(r("VDDCR_VDD", "CPU", "Voltages"));
        let plan = summary_plan(&rows, Platform::Am5);
        assert_eq!(line_labels(&plan.columns[0]), vec!["tCL"]);
        assert_eq!(line_labels(&plan.columns[1]), vec!["tREFI", "tRDRDSCL", "tPHYRDL", "VDD"]);
        assert_eq!(line_labels(&plan.columns[2]), vec!["VDDCR_VDD"]);
        // AGESA is identity, on its own tight row.
        assert!(plan.identity.iter().any(|row| row.iter().any(|p| p.label == "AGESA")));
    }
}
