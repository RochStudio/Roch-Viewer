//! The few things every tab draws: a line of text in a row, a bordered
//! panel, a measured width.
//!
//! The Tk window laid its tables out on grids of labels; here a tab works out
//! its columns from measured text widths and paints each row into a fixed
//! rect, which is what lets the columns line up exactly without any of the
//! alignment passes the grids needed.

use std::sync::Arc;

use eframe::egui::{Color32, CornerRadius, FontId, Galley, Painter, Pos2, Rect, Stroke, StrokeKind, pos2, vec2};

use super::theme::{self, Palette};

/// The Summary panels: rounded, a one-pixel border, and content inset far
/// enough to clear the corner.
pub const PANEL_RADIUS: u8 = 8;
pub const PANEL_PADX: f32 = 6.0;
pub const PANEL_PADY: f32 = 4.0;
pub const PANEL_GAP: f32 = 6.0;
/// Between two column panels on the detail tabs: with each panel's inset
/// either side, one column's text ends 25px before the next one's starts.
pub const PANEL_COLUMN_SPACING: f32 = 25.0 - 2.0 * PANEL_PADX;

/// Breathing room after the longest entry in a column, before the column
/// beside it: the name column against its first value, and one table column
/// against the next. Shorter entries in the same column get more, which is
/// what a shared column means.
pub const COLUMN_GAP: f32 = 25.0;
pub const TRAINING_COLUMN_GAP: f32 = 25.0;
/// One consistent gutter between whole table columns on every tab.
pub const DETAIL_COLUMN_GAP: f32 = 25.0;

pub fn galley(painter: &Painter, text: &str, font: &FontId, color: Color32) -> Arc<Galley> {
    painter.layout_job(theme::job(text, font.clone(), color))
}

/// How wide `text` draws in `font`. Empty text takes no width.
pub fn text_width(painter: &Painter, text: &str, font: &FontId) -> f32 {
    if text.is_empty() {
        return 0.0;
    }
    galley(painter, text, font, Color32::WHITE).size().x.round()
}

/// Which edge of its cell a text is drawn against.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Edge {
    Left,
    Right,
    Center,
}

/// Draw `text` in the row `row`, against `x` on the given edge, centred
/// vertically as a Tk label centres its text.
pub fn text_in_row(painter: &Painter, row: Rect, x: f32, edge: Edge, text: &str, font: &FontId, color: Color32) -> Rect {
    if text.is_empty() {
        return Rect::from_min_size(pos2(x, row.top()), vec2(0.0, row.height()));
    }
    let galley = galley(painter, text, font, color);
    let size = galley.size();
    let left = match edge {
        Edge::Left => x,
        Edge::Right => x - size.x,
        Edge::Center => x - size.x / 2.0,
    };
    // Tk centres a label's text on its line box, which for Consolas puts it
    // two pixels lower than centring the glyph row does.
    let top = row.top() + ((row.height() - size.y) / 2.0).round() + 2.0;
    let pos = Pos2::new(left.round(), top);
    painter.galley(pos, galley, color);
    Rect::from_min_size(pos, size)
}

/// A bordered, rounded panel: the background every table column sits on.
pub fn panel(painter: &Painter, rect: Rect, palette: &Palette) {
    painter.rect(rect, CornerRadius::same(PANEL_RADIUS), palette.bg, Stroke::new(1.0, palette.border), StrokeKind::Inside);
}

/// One table row's background. The band role is the background itself since
/// 1.1.0 aligned the shared theme, so this paints nothing visible; it stays
/// so a band colour, if one comes back, has one place to go.
pub fn band(painter: &Painter, rect: Rect, palette: &Palette, shaded: bool) {
    if shaded && palette.band != palette.bg {
        painter.rect_filled(rect, 0.0, palette.band);
    }
}

/// The rect of row `n` of a table whose first row starts at `top`.
pub fn row_rect(left: f32, right: f32, top: f32, pitch: f32, n: usize) -> Rect {
    Rect::from_min_max(pos2(left, top + pitch * n as f32), pos2(right, top + pitch * (n + 1) as f32))
}
