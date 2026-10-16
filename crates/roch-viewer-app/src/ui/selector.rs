//! The installed-module selector at the foot of Summary, Timings and
//! Training, and the drop-down it shares with the SPD page.
//!
//! One choice drives every module-aware tab: "All modules" shows a row's two
//! channels as one value when they match and an A1/B1 slash pair when they
//! do not; a single module collapses every table to that module.

use eframe::egui::{self, Color32, CursorIcon, Id, Popup, PopupCloseBehavior, Rect, Sense, Stroke, Ui, pos2, vec2};

use rochviewer::display_values::NOT_AVAILABLE;
use rochviewer::memory::dimm_inventory::{DimmModule, channel_of, rank_numeric, split_ic};
use rochviewer::row::Channel;

use super::draw::{self, Edge};
use super::theme::{self, Palette};

/// The selector's height inside its one-pixel brand border.
pub const SELECTOR_HEIGHT: f32 = 28.0;

/// Part number followed by size, rank, DRAM maker and DRAM die.
pub fn module_description(module: &DimmModule) -> String {
    let (dram_manufacturer, dram_die) = split_ic(Some(&module.ic));
    let part = if module.part_number.is_empty() { "Unknown" } else { &module.part_number };
    let capacity = if module.capacity.is_empty() { NOT_AVAILABLE } else { &module.capacity };
    format!("{part} ({capacity}, {}, {dram_manufacturer}, {dram_die})", rank_numeric(module.rank_count))
}

/// The installed modules per channel, named by the board's own slot labels:
/// `(slot, description)` pairs for channel A, then channel B.
///
/// Slots come from SMBIOS DeviceLocator rather than the position of the
/// record in the query. Position is a handle order, not a slot order: on the
/// MSI Z790-P the two modules arrive as records 1 and 3 while the board
/// calls them Controller0-DIMMA2 and Controller1-DIMMB2, so a table indexed
/// by position reported them as B1 and A1 -- the wrong slot numbers and,
/// worse, the channels the wrong way round.
pub fn memory_part_numbers(modules: &[DimmModule]) -> (Vec<(String, String)>, Vec<(String, String)>) {
    let mut a = Vec::new();
    let mut b = Vec::new();
    for module in modules {
        // The board did not name the socket; listing the module under a
        // guessed one is how this went wrong before.
        let Some(slot) = module.slot.clone().filter(|s| !s.is_empty()) else { continue };
        let entry = (slot.clone(), module_description(module));
        match channel_of(Some(&slot)).as_deref() {
            Some("A") => a.push(entry),
            Some("B") => b.push(entry),
            _ => {}
        }
    }
    a.sort();
    b.sort();
    (a, b)
}

/// Describe every installed stick while the combined view is active.
pub fn all_modules_label(channel_a: &[(String, String)], channel_b: &[(String, String)]) -> String {
    let modules: Vec<String> = channel_a
        .iter()
        .chain(channel_b)
        // Keep every identity field but reclaim enough horizontal space for
        // two populated modules in the fixed 700px window. Individual
        // drop-down choices retain the more relaxed spacing.
        .map(|(slot, detail)| format!("{slot}:{}", detail.replace(", ", ",")))
        .collect();
    if modules.is_empty() { "All modules".to_string() } else { modules.join(" | ") }
}

/// One channel of the bottom DIMM strip: slot in front, details behind it.
/// An absent channel draws nothing at all.
///
/// The Python window kept this for a strip under the tabs that 1.1.0 no
/// longer builds; the module selector names the same modules.
#[cfg_attr(not(test), allow(dead_code))]
pub fn dimm_strip_column(modules: &[(String, String)]) -> Vec<(String, String)> {
    modules.to_vec()
}

/// The physical-DIMM choices, built once from the board's slot labels.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ModuleChoices {
    /// "All modules" first, then "slot: description" per module.
    pub choices: Vec<(String, Option<Channel>)>,
    /// What the selector shows while all modules are selected.
    pub all_display: String,
}

impl ModuleChoices {
    pub fn new(channel_a: &[(String, String)], channel_b: &[(String, String)]) -> Self {
        let mut choices = vec![("All modules".to_string(), None)];
        for (channel, modules) in [(Channel::A, channel_a), (Channel::B, channel_b)] {
            for (slot, detail) in modules {
                choices.push((format!("{slot}: {detail}"), Some(channel)));
            }
        }
        ModuleChoices { choices, all_display: all_modules_label(channel_a, channel_b) }
    }

    pub fn from_modules(modules: &[DimmModule]) -> Self {
        let (a, b) = memory_part_numbers(modules);
        Self::new(&a, &b)
    }

    /// The channel a choice selects, or `None` for a choice that is not
    /// one of these (which changes nothing). The all-modules display text
    /// counts as "All modules".
    pub fn select(&self, choice: &str) -> Option<Option<Channel>> {
        let choice = if choice == self.all_display { "All modules" } else { choice };
        self.choices.iter().find(|(name, _)| name == choice).map(|(_, channel)| *channel)
    }

    /// What the selector shows for a selection.
    pub fn display(&self, selected: Option<Channel>) -> String {
        match selected {
            None => self.all_display.clone(),
            Some(channel) => self
                .choices
                .iter()
                .find(|(_, c)| *c == Some(channel))
                .map(|(name, _)| name.clone())
                .unwrap_or_else(|| self.all_display.clone()),
        }
    }

    pub fn names(&self) -> Vec<String> {
        self.choices.iter().map(|(name, _)| name.clone()).collect()
    }
}

/// Draw a drop-down selector in `rect` (its brand-red border included) and
/// return the choice clicked, if any.
pub fn selector(ui: &mut Ui, id: Id, rect: Rect, palette: &Palette, display: &str, choices: &[String]) -> Option<usize> {
    let painter = ui.painter();
    painter.rect_filled(rect, 0.0, palette.brand);
    let inner = rect.shrink(1.0);
    let response = ui.interact(inner, id, Sense::click()).on_hover_cursor(CursorIcon::PointingHand);
    let fill = if response.hovered() { palette.tab_unselected_hover } else { palette.bg };
    painter.rect_filled(inner, 0.0, palette.bg);
    // The arrow's button takes the hover shade, as CustomTkinter's does.
    let arrow = Rect::from_min_max(pos2(inner.right() - 28.0, inner.top()), inner.max);
    painter.rect_filled(arrow, 0.0, fill);
    let font = theme::font(theme::FONT_SIZE);
    let text_painter = painter.with_clip_rect(Rect::from_min_max(inner.min, pos2(arrow.left(), inner.bottom())));
    draw::text_in_row(&text_painter, inner, inner.left() + 4.0, Edge::Left, display, &font, palette.text);
    chevron(painter, arrow.center(), palette.text);

    // Drawn like the native menu the Tk option menu opened: a raised
    // three-pixel frame, 24px items with the text set in past the check
    // column, as wide as its longest choice. Below the selector when it
    // fits, above it otherwise -- a window cannot draw past its own edge.
    let text_width = choices.iter().map(|c| draw::text_width(painter, c, &font)).fold(0.0, f32::max);
    let menu_width = (text_width + 53.0).ceil();
    let mut chosen = None;
    Popup::menu(&response)
        .id(id.with("popup"))
        .gap(2.0)
        .close_behavior(PopupCloseBehavior::CloseOnClick)
        .frame(egui::Frame::new().fill(palette.bg).stroke(Stroke::new(1.0, Color32::from_gray(160))).inner_margin(1))
        .show(|ui| {
            let (menu, _) = ui.allocate_exact_size(vec2(menu_width - 2.0, choices.len() as f32 * 24.0 + 4.0), Sense::hover());
            ui.painter().rect_stroke(menu, 0.0, Stroke::new(2.0, Color32::from_gray(240)), egui::StrokeKind::Inside);
            for (index, choice) in choices.iter().enumerate() {
                let row = Rect::from_min_size(pos2(menu.left() + 2.0, menu.top() + 2.0 + 24.0 * index as f32), vec2(menu.width() - 4.0, 24.0));
                let item = ui.interact(row, id.with(("item", index)), Sense::click());
                if item.hovered() {
                    ui.painter().rect_filled(row, 0.0, palette.tab_unselected_hover);
                }
                draw::text_in_row(ui.painter(), row, row.left() + 22.0, Edge::Left, choice, &font, palette.text);
                if item.clicked() {
                    chosen = Some(index);
                }
            }
        });
    chosen
}

/// The drop-down arrow: a small bold chevron.
fn chevron(painter: &egui::Painter, center: egui::Pos2, color: Color32) {
    let half = 3.5;
    let points = vec![pos2(center.x - half, center.y - 1.5), pos2(center.x, center.y + 2.0), pos2(center.x + half, center.y - 1.5)];
    painter.add(egui::Shape::line(points, Stroke::new(2.0, color)));
}

#[cfg(test)]
mod tests {
    use super::*;

    fn module(slot: &str, part: &str) -> DimmModule {
        DimmModule {
            slot: Some(slot.into()),
            part_number: part.into(),
            capacity: "16GB".into(),
            rank_count: 1,
            ic: "SK hynix A-die".into(),
            ..DimmModule::default()
        }
    }

    // --- tests/test_module_selector.py

    #[test]
    fn module_description_lists_each_requested_identity_field() {
        let mut stick = module("A1", "TMXFL1680838KWK");
        stick.rank = "SR".into();
        assert_eq!(module_description(&stick), "TMXFL1680838KWK (16GB, 1R, SK hynix, A-die)");
    }

    #[test]
    fn all_modules_label_names_both_installed_sticks() {
        assert_eq!(
            all_modules_label(&[("A1".into(), "First stick".into())], &[("B1".into(), "Second stick".into())]),
            "A1:First stick | B1:Second stick"
        );
        assert_eq!(all_modules_label(&[], &[]), "All modules");
    }

    #[test]
    fn selection_switches_channel_and_unknown_changes_nothing() {
        let choices = ModuleChoices::new(&[("A1".into(), "stick".into())], &[("B1".into(), "stick".into())]);
        assert_eq!(choices.select("B1: stick"), Some(Some(Channel::B)));
        assert_eq!(choices.select("A1: stick"), Some(Some(Channel::A)));
        assert_eq!(choices.select("All modules"), Some(None));
        // The combined display text is "All modules" too.
        assert_eq!(choices.select(&choices.all_display), Some(None));
        assert_eq!(choices.select("unknown"), None);
        assert_eq!(choices.display(None), "A1:stick | B1:stick");
        assert_eq!(choices.display(Some(Channel::B)), "B1: stick");
    }

    #[test]
    fn modules_are_listed_by_their_slot_and_channel() {
        // Handle order is not slot order: B2 arrives first.
        let modules = vec![module("B2", "second"), module("A2", "first"), DimmModule { slot: None, ..module("", "lost") }];
        let (a, b) = memory_part_numbers(&modules);
        assert_eq!(a, vec![("A2".to_string(), "first (16GB, 1R, SK hynix, A-die)".to_string())]);
        assert_eq!(b[0].0, "B2");
        let choices = ModuleChoices::from_modules(&modules);
        assert_eq!(choices.names()[1], "A2: first (16GB, 1R, SK hynix, A-die)");
    }

    // --- tests/test_empty_dimm_strip.py

    #[test]
    fn empty_channel_creates_no_placeholder() {
        assert!(dimm_strip_column(&[]).is_empty());
    }
}
