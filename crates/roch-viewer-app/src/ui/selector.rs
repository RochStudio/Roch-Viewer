//! The installed-module selector at the foot of Summary, Timings and
//! Training, and the drop-down it shares with the SPD page.
//!
//! One choice drives every module-aware tab: "All modules" shows a row's two
//! channels as one value when they match and an A1/B1 slash pair when they
//! do not; a single module collapses every table to that module.

use eframe::egui::{self, Color32, CursorIcon, Id, Rect, Sense, Stroke, Ui, pos2, vec2};

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

/// One menu item's height, and the raised frame around the items.
const MENU_ITEM_HEIGHT: f32 = 24.0;
const MENU_FRAME: f32 = 3.0;
/// Between the selector's foot and the menu, as the Tk menu opened.
const MENU_GAP: f32 = 2.0;
/// A click that lands just after the menu closed itself (it lost focus to
/// that very click) closes it rather than opening it again.
const REOPEN_GUARD: f64 = 0.3;
/// The menu window's title: never shown, but how it is found to be squared.
const MENU_TITLE: &str = "Roch Viewer menu";

/// The drop-down's size for these choices: as wide as its longest choice
/// past the check column, 24px an item, inside the raised frame.
pub fn menu_size(text_width: f32, choices: usize) -> egui::Vec2 {
    vec2((text_width + 53.0).ceil(), choices as f32 * MENU_ITEM_HEIGHT + 2.0 * MENU_FRAME)
}

/// Where the menu window goes, in screen points: under the selector, as the
/// native menu the Tk option menu opened did -- outside the window when the
/// selector is at its foot -- or above it when the screen ends first.
pub fn menu_position(window_origin: egui::Pos2, selector: Rect, menu: egui::Vec2, screen_height: f32) -> egui::Pos2 {
    let below = window_origin + vec2(selector.left() + 1.0, selector.bottom() + MENU_GAP);
    if below.y + menu.y <= screen_height {
        below
    } else {
        window_origin + vec2(selector.left() + 1.0, selector.top() - MENU_GAP - menu.y)
    }
}

/// Paint the menu's items into `menu` and return the one clicked.
fn menu_items(ui: &mut Ui, id: Id, menu: Rect, palette: &Palette, font: &egui::FontId, choices: &[String]) -> Option<usize> {
    let painter = ui.painter().clone();
    painter.rect_filled(menu, 0.0, palette.bg);
    // The raised frame of a native menu: a grey outer line, a light inner.
    painter.rect_stroke(menu, 0.0, Stroke::new(1.0, Color32::from_gray(160)), egui::StrokeKind::Inside);
    painter.rect_stroke(menu.shrink(1.0), 0.0, Stroke::new(2.0, Color32::from_gray(240)), egui::StrokeKind::Inside);
    let mut chosen = None;
    for (index, choice) in choices.iter().enumerate() {
        let row = Rect::from_min_size(
            pos2(menu.left() + MENU_FRAME, menu.top() + MENU_FRAME + MENU_ITEM_HEIGHT * index as f32),
            vec2(menu.width() - 2.0 * MENU_FRAME, MENU_ITEM_HEIGHT),
        );
        let item = ui.interact(row, id.with(("item", index)), Sense::click());
        if item.hovered() {
            painter.rect_filled(row, 0.0, palette.tab_unselected_hover);
        }
        draw::text_in_row(&painter, row, row.left() + 21.0, Edge::Left, choice, font, palette.text);
        if item.clicked() {
            chosen = Some(index);
        }
    }
    chosen
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

    // The Tk option menu opened a native menu: its own window, so it drops
    // below the selector and past the window's foot. A borderless viewport
    // of its own does the same; it closes on a choice, on Escape, or when
    // it loses focus to a click anywhere else.
    let text_width = choices.iter().map(|c| draw::text_width(painter, c, &font)).fold(0.0, f32::max);
    let size = menu_size(text_width, choices.len());
    let ctx = ui.ctx().clone();
    let now = ctx.input(|i| i.time);
    let open_id = id.with("open");
    let closed_id = id.with("closed-at");
    let mut open = ctx.data(|d| d.get_temp::<bool>(open_id)).unwrap_or(false);
    let closed_at = ctx.data(|d| d.get_temp::<f64>(closed_id)).unwrap_or(f64::NEG_INFINITY);
    if response.clicked() {
        open = !open && now - closed_at > REOPEN_GUARD;
    }
    let mut chosen = None;
    let (origin, screen) = ctx.input(|i| (i.viewport().inner_rect.map(|r| r.min), i.viewport().monitor_size));
    if open && let Some(origin) = origin {
        let position = menu_position(origin, rect, size, screen.map_or(f32::INFINITY, |s| s.y));
        let builder = egui::ViewportBuilder::default()
            .with_title(MENU_TITLE)
            .with_decorations(false)
            .with_resizable(false)
            .with_taskbar(false)
            .with_always_on_top()
            .with_position(position)
            .with_inner_size(size);
        let menu_id = id.with("menu");
        let palette = *palette;
        let owner_title = format!("{} {}", rochviewer::version::APP_NAME, rochviewer::version::VERSION);
        let (picked, close) = ctx.show_viewport_immediate(egui::ViewportId::from_hash_of(menu_id), builder, |ui, _class| {
            let full = ui.max_rect();
            let picked = menu_items(ui, menu_id, Rect::from_min_size(full.min, size), &palette, &font, choices);
            let (escape, focused) = ui.input(|i| (i.key_pressed(egui::Key::Escape), i.viewport().focused));
            let squared = menu_id.with("squared");
            if !ui.data(|d| d.get_temp::<bool>(squared)).unwrap_or(false)
                && let Some(hwnd) = super::popouts::own_window_titled(MENU_TITLE)
            {
                super::popouts::square_menu_window(hwnd);
                // Owned by the main window, as a menu is: when it closes,
                // Windows hands the focus back to the window it came from
                // rather than to whatever was behind it.
                if let Some(owner) = super::popouts::own_window_titled(&owner_title) {
                    super::popouts::set_owner(hwnd, owner);
                }
                ui.data_mut(|d| d.insert_temp(squared, true));
            }
            let seen = menu_id.with("focused-once");
            let was_focused = ui.data(|d| d.get_temp::<bool>(seen)).unwrap_or(false);
            if focused == Some(true) && !was_focused {
                ui.data_mut(|d| d.insert_temp(seen, true));
            }
            (picked, escape || (was_focused && focused == Some(false)))
        });
        if picked.is_some() || close {
            open = false;
            ctx.data_mut(|d| {
                d.insert_temp(closed_id, now);
                d.remove::<bool>(menu_id.with("focused-once"));
                d.remove::<bool>(menu_id.with("squared"));
            });
            ctx.request_repaint();
        }
        chosen = picked;
    } else if open {
        // No window position to place it by: fall back on a menu inside the
        // window.
        let mut close = false;
        egui::Area::new(id.with("menu-area")).order(egui::Order::Foreground).fixed_pos(pos2(rect.left() + 1.0, rect.top() - MENU_GAP - size.y)).show(&ctx, |ui| {
            let (menu, _) = ui.allocate_exact_size(size, Sense::hover());
            chosen = menu_items(ui, id.with("menu"), menu, palette, &font, choices);
            close = ui.input(|i| i.key_pressed(egui::Key::Escape));
        });
        if chosen.is_some() || close {
            open = false;
        }
    }
    ctx.data_mut(|d| d.insert_temp(open_id, open));
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

    #[test]
    fn the_menu_drops_below_the_selector_past_the_windows_foot() {
        // Measured off the Tk window: the selector at y 680..710 of a window
        // at (930, 245) opened its menu at (943, 957), 382 x 78 with three
        // choices. Inside the window there was no room below, so the egui
        // popup had opened above the selector instead.
        let selector = Rect::from_min_max(pos2(12.0, 680.0), pos2(688.0, 710.0));
        let size = menu_size(329.0, 3);
        assert_eq!(size, vec2(382.0, 78.0));
        assert_eq!(menu_position(pos2(930.0, 245.0), selector, size, 1440.0), pos2(943.0, 957.0));
    }

    #[test]
    fn the_menu_opens_upward_only_when_the_screen_ends() {
        let selector = Rect::from_min_max(pos2(12.0, 680.0), pos2(688.0, 710.0));
        let size = menu_size(329.0, 3);
        let at = menu_position(pos2(930.0, 400.0), selector, size, 1080.0);
        assert_eq!(at, pos2(943.0, 400.0 + 680.0 - 2.0 - 78.0));
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
