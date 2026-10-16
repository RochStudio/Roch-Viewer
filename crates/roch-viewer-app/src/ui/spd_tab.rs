//! The SPD tab: per-slot module identity and the SPD timing profiles.
//!
//! The modules are read off the UI thread the first time the page is opened
//! (or when something else, such as Advanced, asks for them): an SMBus walk
//! of every SPD takes long enough to be felt. Until then the page shows its
//! labels with dashes, and the selector says what it is waiting for.

use std::collections::BTreeMap;
use std::sync::{Arc, Mutex};

use eframe::egui::{self, Id, Rect, Ui, pos2};

use rochviewer::display_values::EM_DASH;
use rochviewer::memory::spd_profiles::{SpdRecord, SpdValue, read_spd_modules};
use rochviewer::platform_profiles::Platform;
use rochviewer::row::Row;

use super::draw::{self, COLUMN_GAP, Edge, PANEL_COLUMN_SPACING, PANEL_GAP, PANEL_PADX, PANEL_PADY};
use super::layout::{spd_tab_fields, spd_tab_profile_fields};
use super::reader::Values;
use super::selector::{self, SELECTOR_HEIGHT};
use super::theme::{self, Palette};

/// Where the read stands.
#[derive(Clone, Debug, Default)]
pub enum SpdState {
    #[default]
    NotStarted,
    Loading,
    Loaded(Vec<SpdRecord>),
}

/// The SPD page's state, shared with the thread that reads it.
#[derive(Clone, Default)]
pub struct SpdTab {
    state: Arc<Mutex<SpdState>>,
    /// The module the page's own selector shows.
    selected: usize,
}

/// The rows the SPD page moved from System Info: the whole system's channel
/// layout and capacity, keyed by the module field they stand in for.
pub const SPD_SYSTEM_ROWS: &[(&str, &str)] = &[("Channels", "system_channels"), ("Memory Capacity", "system_capacity")];

/// The selector's text before and during the read, and when nothing reads.
pub const SPD_NOT_READ: &str = "Open SPD tab to read modules";
pub const SPD_READING: &str = "Reading SPD…";
pub const SPD_NONE: &str = "No readable SPD modules";

impl SpdTab {
    pub fn state(&self) -> SpdState {
        self.state.lock().map(|s| s.clone()).unwrap_or_default()
    }

    /// The modules once read, for anything else that lists them: the
    /// Advanced window's SPD section reads these.
    #[allow(dead_code)]
    pub fn modules(&self) -> Vec<SpdRecord> {
        match self.state() {
            SpdState::Loaded(modules) => modules,
            _ => Vec::new(),
        }
    }

    /// Read SPD once off the UI thread; later calls do nothing.
    pub fn load(&self, ctx: &egui::Context) {
        {
            let Ok(mut state) = self.state.lock() else { return };
            if !matches!(*state, SpdState::NotStarted) {
                return;
            }
            *state = SpdState::Loading;
        }
        let state = self.state.clone();
        let ctx = ctx.clone();
        let spawned = std::thread::Builder::new().name("SPD reader".into()).spawn(move || {
            let modules = std::panic::catch_unwind(|| read_spd_modules(None, false))
                .unwrap_or_else(|_| Ok(Vec::new()))
                .unwrap_or_else(|error| {
                    eprintln!("SPD tab unavailable: {error}");
                    Vec::new()
                });
            if let Ok(mut state) = state.lock() {
                *state = SpdState::Loaded(modules);
            }
            ctx.request_repaint();
        });
        if spawned.is_err() {
            if let Ok(mut state) = self.state.lock() {
                *state = SpdState::Loaded(Vec::new());
            }
        }
    }

    /// The selector's choices: "slot: part" per module.
    pub fn choices(modules: &[SpdRecord]) -> Vec<String> {
        modules
            .iter()
            .enumerate()
            .map(|(index, module)| {
                let slot = module.text("slot").filter(|s| !s.is_empty()).unwrap_or_else(|| format!("DIMM {}", index + 1));
                let part = module.text("part_number").filter(|s| !s.is_empty()).unwrap_or_else(|| "Unknown module".into());
                format!("{slot}: {part}")
            })
            .collect()
    }
}

/// The system-wide values the SPD page shows, from their rows.
pub fn spd_system_values(rows: &[Row], values: &Values) -> BTreeMap<&'static str, String> {
    let mut found = BTreeMap::new();
    for (index, row) in rows.iter().enumerate() {
        if row.tab != "SPD" {
            continue;
        }
        if let Some((_, key)) = SPD_SYSTEM_ROWS.iter().find(|(name, _)| *name == row.name) {
            found.insert(*key, values.text(index));
        }
    }
    found
}

/// One SPD field as the page shows it; Advanced reads it the same way.
///
/// The system-wide values moved here from System Info win over the module's
/// own, the SPD address reads in hex, and nothing reads as a dash.
pub fn spd_field_text(module: &SpdRecord, key: &str, system: &BTreeMap<&'static str, String>) -> String {
    if let Some(value) = system.get(key) {
        return if value.is_empty() { EM_DASH.to_string() } else { value.clone() };
    }
    match module.get(key) {
        Some(SpdValue::Int(address)) if key == "address" => format!("0x{address:02X}"),
        Some(value) => value.display().filter(|s| !s.is_empty()).unwrap_or_else(|| EM_DASH.to_string()),
        None => EM_DASH.to_string(),
    }
}

/// One profile cell: the value, or nothing.
pub fn spd_profile_text(module: &SpdRecord, profile: usize, key: &str) -> String {
    module.profiles.get(profile).and_then(|p| p.get(key)).and_then(SpdValue::display).unwrap_or_default()
}

/// Draw the page into `area` (the content frame; its selector sits at the
/// foot of it).
pub fn show(ui: &mut Ui, spd: &mut SpdTab, rows: &[Row], values: &Values, palette: &Palette, platform: Platform, area: Rect) {
    let state = spd.state();
    let fields = spd_tab_fields(platform);
    let profile_fields = spd_tab_profile_fields(platform);
    let font = theme::font(theme::FONT_SIZE);
    let bold = theme::bold(theme::FONT_SIZE);
    let pitch = theme::ROW_HEIGHT;
    let painter = ui.painter_at(area);
    let width = |text: &str, font: &egui::FontId| draw::text_width(&painter, text, font);

    let modules = match &state {
        SpdState::Loaded(modules) => modules.clone(),
        _ => Vec::new(),
    };
    if spd.selected >= modules.len() {
        spd.selected = 0;
    }
    let module = modules.get(spd.selected);
    // Before the read every value is a dash; after it, the system values
    // fill in even when no module answered.
    let system = if matches!(state, SpdState::Loaded(_)) { spd_system_values(rows, values) } else { BTreeMap::new() };
    let field_text = |key: &str| match module {
        Some(module) => spd_field_text(module, key, &system),
        None => system.get(key).filter(|v| !v.is_empty()).cloned().unwrap_or_else(|| EM_DASH.to_string()),
    };

    // The two info halves: names, the 25px gap, values ending on one edge.
    let per_column = fields.len().div_ceil(2);
    let halves: Vec<&[(&str, &str)]> = vec![&fields[..per_column.min(fields.len())], &fields[per_column.min(fields.len())..]];
    let info_height = 2.0 * PANEL_PADY + pitch * (per_column + 1) as f32;
    let mut left = area.left();
    for (column, half) in halves.iter().enumerate() {
        let texts: Vec<(&str, String)> = half.iter().map(|(label, key)| (*label, field_text(key))).collect();
        let names_w = texts.iter().map(|(label, _)| width(label, &font)).fold(0.0, f32::max);
        let values_w = texts.iter().map(|(_, value)| width(value, &font)).fold(0.0, f32::max);
        let edge = names_w + COLUMN_GAP + values_w;
        // The first half ends on its values; the second takes the rest.
        // The first half's trailing filler cell is an empty label, which
        // took a pixel in the Tk grid.
        let right = if column == 0 { left + PANEL_PADX + edge + 1.0 + PANEL_PADX } else { area.right() };
        let panel = Rect::from_min_max(pos2(left, area.top()), pos2(right, area.top() + info_height));
        draw::panel(&painter, panel, palette);
        let x = left + PANEL_PADX;
        let top = area.top() + PANEL_PADY;
        let heading = draw::row_rect(x, right - PANEL_PADX, top, pitch, 0);
        draw::text_in_row(&painter, heading, x, Edge::Left, if column == 0 { "MODULE" } else { "IDENTITY" }, &bold, palette.value);
        for (n, (label, value)) in texts.iter().enumerate() {
            let rect = draw::row_rect(x, right - PANEL_PADX, top, pitch, n + 1);
            draw::text_in_row(&painter, rect, x, Edge::Left, label, &font, palette.text);
            draw::text_in_row(&painter, rect, x + edge, Edge::Right, value, &font, palette.value);
        }
        left = right + PANEL_COLUMN_SPACING;
    }

    // The timings table and the space below it share one panel, which runs
    // down to the module selector.
    let selector_rect = Rect::from_min_max(pos2(area.left(), area.bottom() - 1.0 - SELECTOR_HEIGHT - 2.0), pos2(area.right(), area.bottom() - 1.0));
    let table_top = area.top() + info_height + PANEL_GAP;
    let table = Rect::from_min_max(pos2(area.left(), table_top), pos2(area.right(), selector_rect.top() - 3.0));
    draw::panel(&painter, table, palette);
    let x = area.left() + PANEL_PADX;
    let top = table_top + PANEL_PADY;
    let profiles = module.map_or(0, |m| m.profiles.len().min(5));
    let title = "TIMINGS TABLE";
    let names_w = profile_fields.iter().map(|(label, _)| width(label, &font)).fold(width(title, &bold), f32::max);
    // Each profile column is its widest entry plus the 25px gap every table
    // keeps; values are right-aligned, so the gap sits on the column's left.
    // Columns past the module's profiles take no width.
    let mut edges = Vec::new();
    let mut edge = x + names_w;
    for profile in 0..profiles {
        let module = module.expect("profiles come from a module");
        let header = spd_profile_text(module, profile, "name");
        let widest = profile_fields
            .iter()
            .map(|(_, key)| width(&spd_profile_text(module, profile, key), &font))
            .fold(width(&header, &bold), f32::max);
        edge += widest + COLUMN_GAP;
        edges.push(edge);
    }
    let row_painter = painter.with_clip_rect(table.shrink(1.0));
    let heading = draw::row_rect(x, table.right() - PANEL_PADX, top, pitch, 0);
    draw::text_in_row(&row_painter, heading, x, Edge::Left, title, &bold, palette.value);
    if let Some(module) = module {
        for (profile, edge) in edges.iter().enumerate() {
            draw::text_in_row(&row_painter, heading, *edge, Edge::Right, &spd_profile_text(module, profile, "name"), &bold, palette.text);
        }
    }
    for (n, (label, key)) in profile_fields.iter().enumerate() {
        let rect = draw::row_rect(x, table.right() - PANEL_PADX, top, pitch, n + 1);
        draw::text_in_row(&row_painter, rect, x, Edge::Left, label, &font, palette.text);
        if let Some(module) = module {
            for (profile, edge) in edges.iter().enumerate() {
                draw::text_in_row(&row_painter, rect, *edge, Edge::Right, &spd_profile_text(module, profile, key), &font, palette.value);
            }
        }
    }

    let (display, choices) = match &state {
        SpdState::NotStarted => (SPD_NOT_READ.to_string(), vec![SPD_NOT_READ.to_string()]),
        SpdState::Loading => (SPD_READING.to_string(), vec![SPD_READING.to_string()]),
        SpdState::Loaded(modules) if modules.is_empty() => (SPD_NONE.to_string(), vec![SPD_NONE.to_string()]),
        SpdState::Loaded(modules) => {
            let choices = SpdTab::choices(modules);
            (choices[spd.selected].clone(), choices)
        }
    };
    if let Some(index) = selector::selector(ui, Id::unique("spd-selector"), selector_rect, palette, &display, &choices) {
        if index < modules.len() {
            spd.selected = index;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use rochviewer::memory::spd_profiles::SpdProfile;

    fn module() -> SpdRecord {
        let mut record = SpdRecord::default();
        record.set("slot", "A1");
        record.set("part_number", "TMXFL1680838KWK");
        record.set("address", 0x50i64);
        record.set("capacity", "16 GB");
        record.set("dram_die", "");
        let mut profile = SpdProfile::new();
        profile.insert("name", SpdValue::text("XMP-8000"));
        profile.insert("cl", SpdValue::Int(38));
        profile.insert("trrd_l", SpdValue::None);
        record.profiles.push(profile);
        record
    }

    #[test]
    fn spd_field_text_reads_hex_dashes_and_system_values() {
        let module = module();
        let mut system = BTreeMap::new();
        assert_eq!(spd_field_text(&module, "address", &system), "0x50");
        assert_eq!(spd_field_text(&module, "capacity", &system), "16 GB");
        assert_eq!(spd_field_text(&module, "dram_die", &system), EM_DASH);
        assert_eq!(spd_field_text(&module, "pmic", &system), EM_DASH);
        system.insert("system_capacity", "32 GB".to_string());
        assert_eq!(spd_field_text(&module, "system_capacity", &system), "32 GB");
    }

    #[test]
    fn profile_cells_show_nothing_for_a_missing_value() {
        let module = module();
        assert_eq!(spd_profile_text(&module, 0, "name"), "XMP-8000");
        assert_eq!(spd_profile_text(&module, 0, "cl"), "38");
        assert_eq!(spd_profile_text(&module, 0, "trrd_l"), "");
        assert_eq!(spd_profile_text(&module, 3, "cl"), "");
    }

    #[test]
    fn choices_name_the_slot_and_part() {
        let mut unnamed = SpdRecord::default();
        unnamed.set("part_number", "");
        assert_eq!(SpdTab::choices(&[module(), unnamed]), vec!["A1: TMXFL1680838KWK", "DIMM 2: Unknown module"]);
    }

    // --- tests/test_window_fit.py: test_spd_resolves_the_moved_system_memory_values

    #[test]
    fn spd_resolves_the_moved_system_memory_values() {
        let rows = vec![
            Row::new("Memory Capacity", "System", "SPD", "FullWidth").with_value("32 GB"),
            Row::new("Channels", "System", "SPD", "FullWidth").with_value("Dual Channel"),
            Row::new("Channels", "System", "System Info", "Left").with_value("ignored"),
        ];
        let values = Values::default();
        let backend = rochviewer::timings::Backend { platform: Platform::Lga1700Ddr5, rows, unavailable: "N/A" };
        for (index, row) in backend.rows.iter().enumerate() {
            values.set(index, super::super::reader::read_row(&backend, row, None));
        }
        let system = spd_system_values(&backend.rows, &values);
        assert_eq!(system.get("system_capacity").map(String::as_str), Some("32 GB"));
        assert_eq!(system.get("system_channels").map(String::as_str), Some("Dual Channel"));
    }
}
