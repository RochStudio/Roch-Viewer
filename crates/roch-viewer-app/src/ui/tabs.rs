//! The reading tabs: System Info, Timings, Training, IMC, RTL and Voltages.
//!
//! Each is drawn the same way. Rows are grouped by category into sections,
//! the sections by their `Column` into side-by-side columns, and each column
//! is one continuous table on its own bordered panel: the section name is a
//! row of the table in the value colour, the names run down the left, and
//! the values end on one edge 25px past the column's longest name. A column
//! whose sections read two channels names the module it shows once, on the
//! heading of its first such section.
//!
//! [`detail_layout`] decides what goes where without drawing anything;
//! [`show_detail`] measures and paints it.

use std::collections::BTreeMap;

use eframe::egui::{Color32, FontId, Rect, Ui, pos2};

use rochviewer::display_values::{EM_DASH, module_pair_text};
use rochviewer::platform_profiles::Platform;
use rochviewer::row::{Channel, Row};
use rochviewer::timings::Backend;

use super::draw::{self, COLUMN_GAP, DETAIL_COLUMN_GAP, Edge, PANEL_COLUMN_SPACING, PANEL_PADX, PANEL_PADY, TRAINING_COLUMN_GAP};
use super::layout::{
    CONTINUOUS_SECTION_TABS, IMC_SECTION_ORDER, SHADED_TABS, SKEW_SECTION_ORDER, TIMINGS_SECTION_ORDER, ordered_sections,
    section_row, timings_three_column_layout,
};
use super::reader::{Values, compact_dual_text};
use super::theme::{self, Palette};

/// What a tab draws from: the table, what has been read, the colours, and
/// the module selection.
#[derive(Clone, Copy)]
pub struct TabContext<'a> {
    pub backend: &'a Backend,
    pub values: &'a Values,
    pub palette: &'a Palette,
    /// The module the selector names, or `None` for all modules.
    pub selected: Option<Channel>,
    /// Slot names for the two channel columns: see
    /// [`super::layout::channel_slot_labels`].
    pub channel_labels: &'a BTreeMap<String, String>,
}

impl TabContext<'_> {
    pub fn rows(&self) -> &[Row] {
        &self.backend.rows
    }

    pub fn platform(&self) -> Platform {
        self.backend.platform
    }

    /// The same value the compact views show: Python's
    /// `_read_compact_value`, under the current module selection.
    pub fn compact_value(&self, index: usize) -> String {
        let row = &self.rows()[index];
        if row.is_dual() {
            return match self.values.sides(index) {
                Some((a, b)) => compact_dual_text(row, &a, &b, self.selected),
                None => self.values.text(index),
            };
        }
        self.values.text(index)
    }

    /// One channel of a dual row as read, or the pair when none is chosen:
    /// `_read_compact_side`.
    pub fn side(&self, index: usize, channel: Channel) -> String {
        match self.values.sides(index) {
            Some((a, b)) => if channel == Channel::A { a } else { b },
            None => EM_DASH.to_string(),
        }
    }

    /// A detailed table value after applying the module choice.
    ///
    /// Timings and Training share one module presentation: the all-modules
    /// view shows one value when A1/B1 match and an ordered slash pair when
    /// they differ. Selecting one physical module collapses either table to
    /// that module in the first value column.
    pub fn detail_channel_text(&self, index: usize) -> String {
        match (self.selected, self.values.sides(index)) {
            (Some(channel), Some(_)) => self.side(index, channel),
            (None, Some((a, b))) => module_pair_text(&a, &b),
            (_, None) => self.values.text(index),
        }
    }

    /// Dim a value the hardware is not currently acting on.
    ///
    /// Some rows stay readable but no longer apply -- tRFC outside Normal
    /// refresh mode, for instance, where the controller refreshes on tRFC2.
    /// Showing them at full weight implies they are in effect.
    pub fn value_color(&self, index: usize) -> Color32 {
        if self.values.dimmed(index) { self.palette.disabled_text } else { self.palette.value }
    }

    /// The two channel column headings for a dual row.
    ///
    /// The slot name wins when one is known, so a column reads A2 rather
    /// than ChA; otherwise the row's own labels stand, which keeps a
    /// platform that reports no usable DeviceLocator on ChA/ChB.
    pub fn channel_headers(&self, index: Option<usize>, default_a: &str, default_b: &str) -> (String, String) {
        let row = index.map(|i| &self.rows()[i]);
        let side_name = |channel: Channel, default: &str| {
            row.and_then(|r| r.side(channel)).and_then(|s| s.name.clone()).unwrap_or_else(|| default.to_string())
        };
        let a = self.channel_labels.get("a").cloned().unwrap_or_else(|| side_name(Channel::A, default_a));
        let b = self.channel_labels.get("b").cloned().unwrap_or_else(|| side_name(Channel::B, default_b));
        (a, b)
    }

    /// The same context in the all-modules view, which is what the column
    /// widths are measured in.
    pub fn all_modules(&self) -> TabContext<'_> {
        TabContext { selected: None, ..*self }
    }

    /// Name the selected module, or the shared all-modules value column.
    pub fn detail_channel_header(&self, a_text: &str, b_text: &str) -> String {
        detail_channel_header(self.selected, a_text, b_text)
    }
}

/// `_detail_channel_header` for the first value column: the selected
/// module's name, or "All modules". The second column's header is always
/// blank -- both tables collapse the two modules into one column.
pub fn detail_channel_header(selected: Option<Channel>, a_text: &str, b_text: &str) -> String {
    match selected {
        Some(Channel::A) => a_text.to_string(),
        Some(Channel::B) => b_text.to_string(),
        None => "All modules".to_string(),
    }
}

/// A frame on a detail tab asks for at least this much width: the Tk build's
/// columns never came out narrower, because the part-row that carried their
/// banding to the foot of the window was a frame, and an empty CustomTkinter
/// frame asks for 200px. A short column such as RTL's therefore keeps its
/// panel at that width rather than shrinking to its text.
pub const DETAIL_COLUMN_MIN_WIDTH: f32 = 200.0;

/// One section of a detail column: its category and the rows it draws.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DetailSection {
    pub name: String,
    pub rows: Vec<usize>,
}

/// One column of a detail tab.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DetailColumn {
    pub key: &'static str,
    pub sections: Vec<DetailSection>,
}

/// The columns a detail tab has room for. Timings uses three. Training
/// selects three only when its active profile has a middle group; the other
/// tabs use two wider columns.
pub fn detail_column_keys(rows: &[Row], tab: &str) -> &'static [&'static str] {
    let training_has_middle = tab == "Training" && rows.iter().any(|row| row.tab == "Training" && row.column == "Middle");
    if tab == "Timings" || training_has_middle { &["Left", "Middle", "Right"] } else { &["Left", "Right"] }
}

fn column_of(row: &Row) -> &str {
    if row.column.is_empty() { "Left" } else { &row.column }
}

/// What goes where on a detail tab: `load_all_tabs_content` without the
/// widgets. Empty columns are left out, so a tab whose rows all sit on the
/// left is drawn as one column across the tab.
pub fn detail_layout(rows: &[Row], tab: &str) -> Vec<DetailColumn> {
    // Diagnostic rows are summarised into one line on the tab, and kept in
    // full in the table, which is where anyone chasing a blank reading will
    // look.
    let tab_rows: Vec<&Row> = rows.iter().filter(|row| row.tab == tab && !row.extra_bool("diagnostic")).collect();
    // A category is a section for each run of consecutive rows that carry it.
    let mut categories: Vec<(String, Vec<String>)> = Vec::new();
    for row in &tab_rows {
        match categories.last_mut() {
            Some((category, names)) if *category == row.category => names.push(row.name.clone()),
            _ => categories.push((row.category.clone(), vec![row.name.clone()])),
        }
    }
    let keys = detail_column_keys(rows, tab);
    let has = |key: &str| keys.contains(&key);
    let timings_layout: &[(&str, &str)] =
        if tab == "Timings" && has("Middle") { timings_three_column_layout(categories.iter().map(|(c, _)| c.as_str())) } else { &[] };
    // A tab whose rows all name one column is drawn as one column, the
    // channel-pinned blocks included.
    let single_column = !tab_rows.iter().any(|row| column_of(row) != "Left" && has(column_of(row)));

    let mut left = Vec::new();
    let mut middle = Vec::new();
    let mut right = Vec::new();
    for (category, names) in categories {
        let mut column = tab_rows.iter().find(|row| row.category == category).map_or("Left", |row| column_of(row));
        if let Some((_, mapped)) = timings_layout.iter().find(|(c, _)| *c == category) {
            column = mapped;
        }
        if single_column {
            column = "Left";
        }
        match column {
            "Left" => left.push((category, names)),
            "Middle" if has("Middle") => middle.push((category, names)),
            _ => right.push((category, names)),
        }
    }
    let order: Option<&[&str]> = match tab {
        "Timings" => Some(TIMINGS_SECTION_ORDER),
        "Training" => Some(SKEW_SECTION_ORDER),
        "IMC" => Some(IMC_SECTION_ORDER),
        _ => None,
    };
    let mut columns = Vec::new();
    for (key, sections) in [("Left", left), ("Middle", middle), ("Right", right)] {
        let sections = match order {
            Some(order) => ordered_sections(sections, order),
            None => sections,
        };
        // A name with no matching row is skipped when the section is built.
        let sections: Vec<DetailSection> = sections
            .into_iter()
            .map(|(name, names)| {
                let rows = names.iter().filter_map(|n| section_row(rows, &name, n)).collect();
                DetailSection { name, rows }
            })
            .collect();
        if !sections.is_empty() {
            columns.push(DetailColumn { key, sections });
        }
    }
    columns
}

/// One drawn line of a detail column.
enum Line {
    Heading { text: String, channel: Option<String> },
    Row { label: String, value: String, color: Color32 },
}

/// A column's lines and where its values end.
struct Measured {
    lines: Vec<Line>,
    /// The values' right edge, from the column's left.
    text_width: f32,
    /// Whether any section reads two channels.
    dual: bool,
}

/// Round a leading column up to an even width, as the Tk build had to:
/// CustomTkinter's draw engine fills an even number of pixels and drops the
/// remainder, so an odd column left a hairline down the seam. The pixel it
/// is rounded up by went to the values, which are right-aligned, so they
/// move with it and the gutter stays 25.
pub fn even_text_width(text_width: f32, gap: f32) -> f32 {
    let with_gap = (text_width + gap).round() as i64;
    text_width + (with_gap % 2) as f32
}

fn measure_column(ui: &Ui, ctx: &TabContext, tab: &str, column: &DetailColumn, font: &FontId, bold: &FontId) -> Measured {
    let painter = ui.painter();
    let width = |text: &str, font: &FontId| draw::text_width(painter, text, font);
    let gap = if tab == "Training" { TRAINING_COLUMN_GAP } else { COLUMN_GAP };
    // System Info pairs a long OS name and motherboard model on the left
    // with Graphics on the right. Sharing one name/value width across every
    // section made both halves request the sum of their unrelated longest
    // strings and overflow the window, so there each section keeps its own
    // widths and only the values' edge is shared; the dense timing tabs
    // share one name width down the whole column.
    let per_section = tab == "System Info";
    let continuous = CONTINUOUS_SECTION_TABS.contains(&tab);

    let all = ctx.all_modules();
    let mut lines = Vec::new();
    let mut labelled = false;
    let (mut names_w, mut values_w) = (0.0f32, 0.0f32);
    let mut edge = 0.0f32;
    for section in &column.sections {
        let dual = section.rows.iter().any(|&i| ctx.rows()[i].is_dual());
        let heading = section.name.to_uppercase();
        let channel = if continuous && dual && !labelled {
            let (a, b) = ctx.channel_headers(section.rows.first().copied(), "A", "B");
            Some(ctx.detail_channel_header(&a, &b))
        } else {
            None
        };
        labelled |= dual;
        let (mut section_names, mut section_values) = (0.0f32, 0.0f32);
        // The heading shares the row grid only when it carries channel
        // names; a plain heading sits above the grid and is not measured.
        if dual && continuous {
            section_names = section_names.max(width(&heading, bold));
        }
        if let Some(text) = &channel {
            section_values = section_values.max(width(text, bold));
            // The Tk grid measured its columns once, in the all-modules
            // view, and a narrower selection never shrank them.
            section_values = section_values.max(width(&detail_channel_header(None, "", ""), bold));
        }
        lines.push(Line::Heading { text: heading, channel });
        for &index in &section.rows {
            let row = &ctx.rows()[index];
            let label = row.label().to_string();
            let value = if row.is_dual() { ctx.detail_channel_text(index) } else { ctx.compact_value(index) };
            section_names = section_names.max(width(&label, font));
            section_values = section_values.max(width(&value, font));
            if ctx.selected.is_some() && row.is_dual() {
                section_values = section_values.max(width(&all.detail_channel_text(index), font));
            }
            lines.push(Line::Row { label, value, color: ctx.value_color(index) });
        }
        if per_section {
            // Only a section with values keeps the gap after its names.
            let gap = if section_values > 0.0 { gap } else { 0.0 };
            edge = edge.max(section_names + gap + section_values);
        }
        names_w = names_w.max(section_names);
        values_w = values_w.max(section_values);
    }
    if !per_section {
        edge = names_w + if values_w > 0.0 { gap } else { 0.0 } + values_w;
    }
    let dual = column.sections.iter().any(|section| section.rows.iter().any(|&i| ctx.rows()[i].is_dual()));
    Measured { lines, text_width: edge, dual }
}

/// Whether a tab bands its rows, and on which: only the tabs in
/// SHADED_TABS, alternating from an unshaded first row.
pub fn shade_row(tab: Option<&str>, data_row: usize) -> bool {
    tab.is_some_and(|tab| SHADED_TABS.contains(&tab)) && data_row % 2 == 1
}

/// Where one detail column goes.
pub struct ColumnGeometry {
    /// Its bordered panel.
    pub panel: Rect,
    /// Where its names start.
    pub left: f32,
    /// Where its values end.
    pub value_edge: f32,
    /// What the column's text needs across, from `left` to its frame's edge.
    #[cfg_attr(not(test), allow(dead_code))]
    pub frame_width: f32,
    lines: Vec<Line>,
}

#[cfg_attr(not(test), allow(dead_code))]
impl ColumnGeometry {
    pub fn line_count(&self) -> usize {
        self.lines.len()
    }

    /// The channel names this column's headings carry: one, on its first
    /// two-channel section, or none.
    pub fn channel_labels(&self) -> Vec<&str> {
        self.lines
            .iter()
            .filter_map(|line| match line {
                Line::Heading { channel: Some(text), .. } => Some(text.as_str()),
                _ => None,
            })
            .collect()
    }
}

/// A detail tab laid out in `area`.
pub struct DetailGeometry {
    pub columns: Vec<ColumnGeometry>,
    pub pitch: f32,
    font: FontId,
    bold: FontId,
}

#[cfg_attr(not(test), allow(dead_code))]
impl DetailGeometry {
    /// The height the tab's tallest column needs, insets included.
    pub fn needed_height(&self) -> f32 {
        let lines = self.columns.iter().map(ColumnGeometry::line_count).max().unwrap_or(0);
        2.0 * PANEL_PADY + self.pitch * lines as f32
    }

    /// The width the columns need: every panel at its content's width.
    pub fn needed_width(&self) -> f32 {
        match (self.columns.first(), self.columns.last()) {
            (Some(first), Some(last)) => last.left + last.frame_width + PANEL_PADX - first.panel.left(),
            _ => 0.0,
        }
    }
}

/// Lay a detail tab out in `area` without drawing it.
pub fn detail_geometry(ui: &Ui, ctx: &TabContext, tab: &str, area: Rect) -> DetailGeometry {
    let platform = ctx.platform();
    let pitch = theme::row_height_for_tab(platform, tab);
    let size = theme::font_size_for_tab(platform, tab);
    let font = theme::font(size);
    let bold = theme::bold(size);
    let measured: Vec<Measured> =
        detail_layout(ctx.rows(), tab).iter().map(|c| measure_column(ui, ctx, tab, c, &font, &bold)).collect();
    let last = measured.len().saturating_sub(1);
    let mut cell_left = area.left();
    let mut columns = Vec::new();
    for (index, column) in measured.into_iter().enumerate() {
        // Each leading column is its own content plus the gap; the last
        // takes whatever is left, so a narrow tab still fills the window.
        let text_width = if index == last { column.text_width } else { even_text_width(column.text_width, DETAIL_COLUMN_GAP) };
        // A two-channel column keeps an empty second value column, which
        // took a pixel in the Tk grid.
        let frame_width = (text_width + if column.dual { 1.0 } else { 0.0 }).max(DETAIL_COLUMN_MIN_WIDTH);
        let panel_right = if index == last { area.right() } else { cell_left + PANEL_PADX + frame_width + PANEL_PADX };
        let panel = Rect::from_min_max(pos2(cell_left, area.top()), pos2(panel_right, area.bottom()));
        let left = cell_left + PANEL_PADX;
        columns.push(ColumnGeometry { panel, left, value_edge: left + text_width, frame_width, lines: column.lines });
        cell_left = panel_right + PANEL_COLUMN_SPACING;
    }
    DetailGeometry { columns, pitch, font, bold }
}

/// Draw one detail tab into `area`, the space below the tab strip.
pub fn show_detail(ui: &mut Ui, ctx: &TabContext, tab: &str, area: Rect) {
    let geometry = detail_geometry(ui, ctx, tab, area);
    let painter = ui.painter_at(area);
    let palette = ctx.palette;
    let (font, bold, pitch) = (&geometry.font, &geometry.bold, geometry.pitch);
    for column in &geometry.columns {
        draw::panel(&painter, column.panel, palette);
        let top = area.top() + PANEL_PADY;
        let row_painter = painter.with_clip_rect(column.panel.shrink(1.0));
        for (n, line) in column.lines.iter().enumerate() {
            let rect = draw::row_rect(column.left, column.panel.right() - PANEL_PADX, top, pitch, n);
            draw::band(&row_painter, rect, palette, shade_row(Some(tab), n));
            match line {
                Line::Heading { text, channel } => {
                    // On a continuous tab the heading is a row among rows, so
                    // it takes the value colour to stand out from the names
                    // beside it.
                    draw::text_in_row(&row_painter, rect, column.left, Edge::Left, text, bold, palette.value);
                    if let Some(channel) = channel {
                        draw::text_in_row(&row_painter, rect, column.value_edge, Edge::Right, channel, bold, palette.text);
                    }
                }
                Line::Row { label, value, color } => {
                    draw::text_in_row(&row_painter, rect, column.left, Edge::Left, label, font, palette.text);
                    draw::text_in_row(&row_painter, rect, column.value_edge, Edge::Right, value, font, *color);
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ui::layout::tests::row;
    use rochviewer::row::{Side, Value};

    fn names(rows: &[Row], layout: &[DetailColumn]) -> Vec<(&'static str, Vec<(String, Vec<String>)>)> {
        layout
            .iter()
            .map(|c| {
                (
                    c.key,
                    c.sections
                        .iter()
                        .map(|s| (s.name.clone(), s.rows.iter().map(|&i| rows[i].name.clone()).collect()))
                        .collect(),
                )
            })
            .collect()
    }

    #[test]
    fn timings_take_the_platform_three_column_layout_and_order() {
        let mut rows = Vec::new();
        for (category, name) in [
            ("Tertiary", "tRDRD_sg"),
            ("Primary", "tCL"),
            ("Other Timings", "tZQCAL"),
            ("Refresh timings", "tREFI"),
            ("Secondary", "tWR"),
            ("CAS to CAS", "tCCD_S"),
            ("Command", "tCSH"),
            ("Power down", "tCKE"),
        ] {
            rows.push(Row::new(name, category, "Timings", "Left"));
        }
        // One row on another column, or the tab is a single column and the
        // layout below does not apply.
        rows[0].column = "Middle".into();
        let layout = detail_layout(&rows, "Timings");
        let shown = names(&rows, &layout);
        let sections = |key: &str| -> Vec<String> {
            shown.iter().find(|(k, _)| *k == key).unwrap().1.iter().map(|(s, _)| s.clone()).collect()
        };
        assert_eq!(sections("Left"), vec!["Primary", "Secondary", "Command"]);
        assert_eq!(sections("Middle"), vec!["Refresh timings", "Tertiary"]);
        assert_eq!(sections("Right"), vec!["CAS to CAS", "Power down", "Other Timings"]);
    }

    #[test]
    fn a_category_is_a_section_per_run_and_rows_resolve_by_name() {
        let rows = vec![
            Row::new("CPU", "Processor", "System Info", "Left"),
            Row::new("GPU", "Graphics", "System Info", "Right"),
            Row::new("Microcode", "Processor", "System Info", "Left"),
            Row::new("Diag", "Processor", "System Info", "Left").with_extra("diagnostic", rochviewer::row::Extra::Bool(true)),
        ];
        let layout = detail_layout(&rows, "System Info");
        assert_eq!(
            names(&rows, &layout),
            vec![
                ("Left", vec![("Processor".into(), vec!["CPU".into()]), ("Processor".into(), vec!["Microcode".into()])]),
                ("Right", vec![("Graphics".into(), vec!["GPU".into()])]),
            ]
        );
    }

    #[test]
    fn a_tab_on_one_column_draws_as_one_column() {
        let rows = vec![row("a", "One", "Voltages"), row("b", "Two", "Voltages")];
        let layout = detail_layout(&rows, "Voltages");
        assert_eq!(layout.len(), 1);
        assert_eq!(layout[0].sections.len(), 2);
    }

    #[test]
    fn training_has_a_middle_column_only_when_a_row_asks_for_one() {
        let two = vec![Row::new("a", "RTT", "Training", "Left"), Row::new("b", "VREF", "Training", "Middle")];
        assert_eq!(detail_column_keys(&two, "Training"), &["Left", "Middle", "Right"]);
        let one = vec![Row::new("a", "RTT", "Training", "Left"), Row::new("b", "VREF", "Training", "Right")];
        assert_eq!(detail_column_keys(&one, "Training"), &["Left", "Right"]);
        // A Middle row with no middle column falls to the right; a tab whose
        // other rows all sit on the left is drawn as one column.
        let layout = detail_layout(
            &[Row::new("a", "RTT", "IMC", "Left"), Row::new("b", "X", "IMC", "Middle"), Row::new("c", "Y", "IMC", "Right")],
            "IMC",
        );
        assert_eq!(layout.iter().map(|c| c.key).collect::<Vec<_>>(), vec!["Left", "Right"]);
        assert_eq!(layout[1].sections.len(), 2);
        let layout = detail_layout(&[Row::new("a", "RTT", "IMC", "Left"), Row::new("b", "X", "IMC", "Middle")], "IMC");
        assert_eq!(layout.iter().map(|c| c.key).collect::<Vec<_>>(), vec!["Left"]);
    }

    #[test]
    fn rtl_blank_rows_stay_as_spacers() {
        let rows = vec![
            Row::new("RTL MC0 CHA R7", "RTL CHA", "RTL", "Left"),
            Row::new("", "RTL CHA", "RTL", "Left"),
            Row::new("RTL MC1 CHA R0", "RTL CHA", "RTL", "Left"),
        ];
        let layout = detail_layout(&rows, "RTL");
        assert_eq!(layout[0].sections[0].rows, vec![0, 1, 2]);
    }

    // --- tests/test_module_selector.py: the detail tables' module view.

    fn context_rows() -> Vec<Row> {
        vec![
            Row::new("tCL", "Primary", "Timings", "Left").with_sides(Side::value(Value::text("38")), Side::value(Value::text("40"))),
            Row::new("tRCD", "Primary", "Timings", "Left").with_sides(Side::value(Value::text("38")), Side::value(Value::text("38"))),
        ]
    }

    #[test]
    fn timings_and_training_share_the_compact_all_modules_view() {
        let backend = Backend { platform: Platform::Lga1700Ddr5, rows: context_rows(), unavailable: "N/A" };
        let values = Values::default();
        for (index, row) in backend.rows.iter().enumerate() {
            values.set(index, super::super::reader::read_row(&backend, row, None));
        }
        let palette = Palette::new(true);
        let labels = BTreeMap::new();
        let mut ctx = TabContext { backend: &backend, values: &values, palette: &palette, selected: None, channel_labels: &labels };
        assert_eq!(ctx.detail_channel_text(0), "38/40");
        assert_eq!(detail_channel_header(None, "A1", "B1"), "All modules");
        assert_eq!(ctx.detail_channel_text(1), "38");
        ctx.selected = Some(Channel::B);
        assert_eq!(ctx.detail_channel_text(0), "40");
        assert_eq!(ctx.detail_channel_header("A1", "B1"), "B1");
        ctx.selected = Some(Channel::A);
        assert_eq!(ctx.compact_value(0), "38");
        assert_eq!(ctx.detail_channel_header("A1", "B1"), "A1");
    }

    #[test]
    fn channel_headers_prefer_the_slot_names() {
        let backend = Backend {
            platform: Platform::Lga1700Ddr5,
            rows: vec![Row::new("tCL", "Primary", "Timings", "Left").with_sides(
                Side::value(Value::text("1")).named("ChA"),
                Side::value(Value::text("2")).named("ChB"),
            )],
            unavailable: "N/A",
        };
        let values = Values::default();
        let palette = Palette::new(false);
        let none = BTreeMap::new();
        let ctx = TabContext { backend: &backend, values: &values, palette: &palette, selected: None, channel_labels: &none };
        assert_eq!(ctx.channel_headers(Some(0), "A", "B"), ("ChA".into(), "ChB".into()));
        assert_eq!(ctx.channel_headers(None, "A1", "B1"), ("A1".into(), "B1".into()));
        let labels = BTreeMap::from([("a".to_string(), "A2".to_string())]);
        let ctx = TabContext { channel_labels: &labels, ..ctx };
        assert_eq!(ctx.channel_headers(Some(0), "A", "B"), ("A2".into(), "ChB".into()));
    }

    // --- ShadedTabTest

    #[test]
    fn system_info_alternates_starting_unshaded() {
        assert!(!shade_row(Some("System Info"), 0));
        assert!(shade_row(Some("System Info"), 1));
        assert!(!shade_row(Some("System Info"), 2));
    }

    #[test]
    fn a_tab_that_did_not_ask_for_banding_keeps_the_plain_background() {
        assert!(!SHADED_TABS.contains(&"Jedec"));
        assert!(!shade_row(Some("Jedec"), 1));
    }

    #[test]
    fn a_section_built_without_a_tab_name_is_not_shaded() {
        assert!(!shade_row(None, 1));
    }
}
