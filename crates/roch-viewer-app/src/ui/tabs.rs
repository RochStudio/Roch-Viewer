//! Drawing a tab's rows.
//!
//! The generic layout every reading tab starts from: rows grouped by their
//! `column` into side-by-side columns, by `category` into sections inside a
//! column, in the order the profile listed them. Each section is a red
//! heading over name/value lines; a dual row adds a value column per
//! channel. Tab-specific layouts (Summary's panels, the three-column
//! Timings, Training's groups) build on these pieces.

use eframe::egui::{self, Align, Color32, Layout, RichText, Sense, Ui};

use rochviewer::row::Row;

use super::reader::Values;
use super::theme::{self, Palette};

/// The order columns are laid out in.
pub const COLUMN_ORDER: &[&str] = &["Left", "Middle", "Right"];

pub struct TabContext<'a> {
    pub rows: &'a [Row],
    pub values: &'a Values,
    pub palette: &'a Palette,
}

/// Indices of the rows on `tab`, grouped by column then by category, each
/// in first-seen order.
pub fn layout_for_tab(rows: &[Row], tab: &str) -> Vec<(String, Vec<(String, Vec<usize>)>)> {
    let mut columns: Vec<(String, Vec<(String, Vec<usize>)>)> = Vec::new();
    for (index, row) in rows.iter().enumerate() {
        if row.tab != tab || row.extra_bool("advanced_only") {
            continue;
        }
        let column = match columns.iter_mut().find(|(name, _)| *name == row.column) {
            Some(column) => column,
            None => {
                columns.push((row.column.clone(), Vec::new()));
                columns.last_mut().unwrap()
            }
        };
        match column.1.iter_mut().find(|(category, _)| *category == row.category) {
            Some(section) => section.1.push(index),
            None => column.1.push((row.category.clone(), vec![index])),
        }
    }
    columns.sort_by_key(|(name, _)| COLUMN_ORDER.iter().position(|c| c == name).unwrap_or(COLUMN_ORDER.len()));
    columns
}

/// A section heading in the brand red with a hairline under it.
pub fn section_heading(ui: &mut Ui, palette: &Palette, title: &str) {
    ui.add_space(theme::SECTION_GAP);
    ui.label(RichText::new(title).font(theme::bold(theme::FONT_SIZE)).color(palette.brand));
    let (rect, _) = ui.allocate_exact_size(egui::vec2(ui.available_width(), 1.0), Sense::hover());
    ui.painter().rect_filled(rect, 0.0, palette.hairline);
    ui.add_space(2.0);
}

/// One name/value line, banded on odd rows.
pub fn value_line(ui: &mut Ui, palette: &Palette, band: bool, name: &str, values: &[(String, Color32)]) {
    let height = theme::ROW_HEIGHT;
    let (rect, _) = ui.allocate_exact_size(egui::vec2(ui.available_width(), height), Sense::hover());
    if band {
        ui.painter().rect_filled(rect, 0.0, palette.band);
    }
    let mut child = ui.new_child(egui::UiBuilder::new().max_rect(rect.shrink2(egui::vec2(4.0, 0.0))).layout(Layout::left_to_right(Align::Center)));
    child.label(RichText::new(name).font(theme::font(theme::FONT_SIZE)).color(palette.text));
    child.with_layout(Layout::right_to_left(Align::Center), |ui| {
        for (i, (text, color)) in values.iter().enumerate().rev() {
            if i + 1 < values.len() {
                ui.add_space(12.0);
            }
            ui.label(RichText::new(text).font(theme::font(theme::FONT_SIZE)).color(*color));
        }
    });
}

fn row_values(ctx: &TabContext, index: usize) -> Vec<(String, Color32)> {
    let row = &ctx.rows[index];
    let color = if row.is_dimmed() { ctx.palette.disabled_text } else { ctx.palette.value };
    let text = ctx.values.text(index);
    if row.is_dual() {
        // The reader stores the pair as "A/B" when they differ.
        return text.split_once('/').map_or_else(
            || vec![(text.clone(), color)],
            |(a, b)| vec![(a.to_string(), color), (b.to_string(), color)],
        );
    }
    vec![(text, color)]
}

/// Draw one tab with the generic column/section layout.
pub fn show_generic(ui: &mut Ui, ctx: &TabContext, tab: &str) {
    let columns = layout_for_tab(ctx.rows, tab);
    if columns.is_empty() {
        ui.label(RichText::new("Nothing to show on this platform.").color(ctx.palette.disabled_text));
        return;
    }
    let count = columns.len();
    ui.columns(count, |uis| {
        for (ui, (_, sections)) in uis.iter_mut().zip(columns.iter()) {
            for (category, indices) in sections {
                section_heading(ui, ctx.palette, category);
                for (n, &index) in indices.iter().enumerate() {
                    let row = &ctx.rows[index];
                    if row.extra_bool("hide_when_blank") && ctx.values.text(index).trim() == rochviewer::display_values::EM_DASH {
                        continue;
                    }
                    value_line(ui, ctx.palette, n % 2 == 1, row.label(), &row_values(ctx, index));
                }
            }
        }
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rows_group_by_column_then_category_in_order() {
        let rows = vec![
            Row::new("tCL", "Primary", "Timings", "Left"),
            Row::new("tRFC", "Secondary", "Timings", "Right"),
            Row::new("tRCD", "Primary", "Timings", "Left"),
            Row::new("Mid", "Odd", "Timings", "Middle"),
            Row::new("CPU", "General", "System Info", "Left"),
        ];
        let layout = layout_for_tab(&rows, "Timings");
        let names: Vec<&str> = layout.iter().map(|(c, _)| c.as_str()).collect();
        assert_eq!(names, vec!["Left", "Middle", "Right"]);
        assert_eq!(layout[0].1, vec![("Primary".to_string(), vec![0, 2])]);
    }
}
