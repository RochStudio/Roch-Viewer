//! The Settings window: everything about how the viewer looks and behaves.
//!
//! Every control writes straight into the shared preferences and the main
//! window applies them on its next frame, so a change is seen as it is made;
//! there is no Apply button. The preferences are saved a moment later.

use std::collections::HashMap;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};

use eframe::egui::{self, RichText, Ui, vec2};

use super::fonts::{self, Family};
use super::popouts;
use super::prefs::{
    self, COLOR_ROLES, Colors, OPACITY_RANGE, Prefs, REFRESH_RANGE, RADIUS_RANGE, ROW_DELTA_RANGE, SCALE_RANGE,
    TEXT_DELTA_RANGE, ThemeMode,
};
use super::theme::{self, Palette};

pub const WINDOW_WIDTH: f32 = 640.0;
pub const WINDOW_HEIGHT: f32 = 600.0;
const NAV_WIDTH: f32 = 130.0;
const TEXT_SIZE: f32 = 13.0;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Page {
    Appearance,
    Colors,
    Text,
    Layout,
    Window,
    Behavior,
}

impl Page {
    const ALL: [Page; 6] = [Page::Appearance, Page::Colors, Page::Text, Page::Layout, Page::Window, Page::Behavior];

    fn name(self) -> &'static str {
        match self {
            Page::Appearance => "Appearance",
            Page::Colors => "Colours",
            Page::Text => "Text",
            Page::Layout => "Layout",
            Page::Window => "Window",
            Page::Behavior => "Behaviour",
        }
    }
}

/// What the Settings window asks the main window to do beyond a preference
/// change.
#[derive(Default)]
pub struct Requests {
    /// The Telemetry auto-open switch changed.
    pub auto_open: Option<bool>,
    /// Forget every saved window position.
    pub forget_positions: bool,
}

pub struct SettingsWindow {
    pub open: bool,
    pub palette: Palette,
    prefs: Arc<Mutex<Prefs>>,
    revision: Arc<AtomicU64>,
    pub requests: Requests,
    auto_open: bool,
    page: Page,
    fonts: Option<Vec<Family>>,
    font_filter: String,
    /// Which mode's colours the Colours page edits; the mode in use until
    /// the other is picked.
    editing_dark: Option<bool>,
    hex: HashMap<String, String>,
    confirm_reset: bool,
}

impl SettingsWindow {
    pub fn new(palette: Palette, prefs: Arc<Mutex<Prefs>>, revision: Arc<AtomicU64>, auto_open: bool) -> Self {
        SettingsWindow {
            open: true,
            palette,
            prefs,
            revision,
            requests: Requests::default(),
            auto_open,
            page: Page::Appearance,
            fonts: None,
            font_filter: String::new(),
            editing_dark: None,
            hex: HashMap::new(),
            confirm_reset: false,
        }
    }

    pub fn ui(&mut self, ui: &mut Ui) {
        if ui.ctx().input(|i| i.viewport().close_requested() || i.key_pressed(egui::Key::Escape)) {
            self.open = false;
        }
        let palette = self.palette;
        let mut prefs = self.prefs.lock().unwrap_or_else(|e| e.into_inner()).clone();
        let before = prefs.clone();

        let style = ui.style_mut();
        style.spacing.item_spacing = vec2(8.0, 6.0);
        style.spacing.button_padding = vec2(8.0, 3.0);
        style.spacing.slider_width = 220.0;
        style.spacing.interact_size.y = 22.0;
        for text_style in [egui::TextStyle::Body, egui::TextStyle::Button, egui::TextStyle::Monospace] {
            style.text_styles.insert(text_style, egui::FontId::new(TEXT_SIZE, egui::FontFamily::Name(theme::CONSOLAS.into())));
        }
        style.text_styles.insert(egui::TextStyle::Heading, egui::FontId::new(TEXT_SIZE + 3.0, egui::FontFamily::Name(theme::CONSOLAS_BOLD.into())));
        style.text_styles.insert(egui::TextStyle::Small, egui::FontId::new(TEXT_SIZE - 2.0, egui::FontFamily::Name(theme::CONSOLAS.into())));
        style.visuals.widgets.inactive.bg_fill = palette.tab_unselected_hover;
        style.visuals.widgets.inactive.weak_bg_fill = palette.tab_unselected_hover;
        style.visuals.widgets.hovered.weak_bg_fill = palette.border;
        style.visuals.widgets.hovered.bg_fill = palette.border;
        style.visuals.widgets.active.bg_fill = palette.tab_selected;
        style.visuals.selection.bg_fill = palette.tab_selected;
        style.visuals.slider_trailing_fill = true;
        // Text fields and colour swatches read as fields: a tinted well with
        // an outline, not text floating on the background.
        style.visuals.extreme_bg_color = palette.tab_unselected_hover;
        style.visuals.widgets.inactive.bg_stroke = egui::Stroke::new(1.0, palette.border);
        style.visuals.widgets.hovered.bg_stroke = egui::Stroke::new(1.0, palette.brand);

        egui::Panel::bottom("settings-footer")
            .frame(egui::Frame::new().fill(palette.bg).inner_margin(egui::Margin::symmetric(12, 8)))
            .show(ui, |ui| self.footer(ui, &mut prefs));
        egui::Panel::left("settings-nav")
            .exact_size(NAV_WIDTH)
            .resizable(false)
            .frame(egui::Frame::new().fill(palette.bg).inner_margin(egui::Margin::symmetric(8, 12)))
            .show(ui, |ui| self.nav(ui));
        egui::CentralPanel::default()
            .frame(egui::Frame::new().fill(palette.bg).inner_margin(egui::Margin { left: 14, right: 14, top: 12, bottom: 4 }))
            .show(ui, |ui| {
                egui::ScrollArea::vertical().auto_shrink([false, false]).show(ui, |ui| {
                    ui.heading(RichText::new(self.page.name()).color(palette.brand));
                    ui.add_space(6.0);
                    match self.page {
                        Page::Appearance => self.appearance(ui, &mut prefs),
                        Page::Colors => self.colors(ui, &mut prefs),
                        Page::Text => self.text(ui, &mut prefs),
                        Page::Layout => self.layout(ui, &mut prefs),
                        Page::Window => self.window(ui, &mut prefs),
                        Page::Behavior => self.behavior(ui, &mut prefs),
                    }
                });
            });

        if prefs != before {
            *self.prefs.lock().unwrap_or_else(|e| e.into_inner()) = prefs;
            self.revision.fetch_add(1, Ordering::SeqCst);
            ui.ctx().request_repaint_of(egui::ViewportId::ROOT);
        }
    }

    fn nav(&mut self, ui: &mut Ui) {
        let palette = self.palette;
        for page in Page::ALL {
            let selected = self.page == page;
            let text = RichText::new(page.name()).color(if selected { palette.tab_selected_text } else { palette.text });
            let button = egui::Button::new(text)
                .fill(if selected { palette.tab_selected } else { palette.bg })
                .stroke(egui::Stroke::NONE)
                .corner_radius(theme::radius(6))
                .min_size(vec2(NAV_WIDTH - 16.0, 26.0));
            if ui.add(button).clicked() {
                self.page = page;
            }
        }
    }

    fn footer(&mut self, ui: &mut Ui, prefs: &mut Prefs) {
        ui.horizontal(|ui| {
            if self.confirm_reset {
                ui.label(RichText::new("Reset every setting?").color(self.palette.brand));
                if ui.button("Reset").clicked() {
                    let positions = prefs.positions.clone();
                    *prefs = Prefs { positions, ..Prefs::default() };
                    self.hex.clear();
                    self.confirm_reset = false;
                }
                if ui.button("Cancel").clicked() {
                    self.confirm_reset = false;
                }
            } else if ui.button("Reset all").on_hover_text("Every setting back to how the viewer first opened").clicked() {
                self.confirm_reset = true;
            }
            if ui.button("Settings file").on_hover_text("Open the folder that holds settings.json").clicked() {
                if let Some(folder) = super::settings::Settings::path().parent() {
                    super::admin::shell_open(&folder.display().to_string());
                }
            }
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                if ui.button("Close").clicked() {
                    self.open = false;
                }
            });
        });
    }

    fn appearance(&mut self, ui: &mut Ui, prefs: &mut Prefs) {
        section(ui, &self.palette, "Theme");
        ui.horizontal(|ui| {
            for mode in ThemeMode::ALL {
                ui.radio_value(&mut prefs.theme, mode, mode.name());
            }
        });

        section(ui, &self.palette, "Colour scheme");
        ui.horizontal_wrapped(|ui| {
            for preset in prefs::presets() {
                let active = prefs.light == preset.light && prefs.dark == preset.dark;
                if ui.selectable_label(active, preset.name).clicked() {
                    prefs.light = preset.light;
                    prefs.dark = preset.dark;
                    self.hex.clear();
                }
            }
        });
        hint(ui, &self.palette, "A starting point: every colour can be changed on the Colours page.");

        section(ui, &self.palette, "Contrast");
        let mut percent = (prefs.contrast * 100.0).round();
        if ui.add(egui::Slider::new(&mut percent, -100.0..=100.0).step_by(5.0).fixed_decimals(0).suffix("%")).changed() {
            prefs.contrast = percent / 100.0;
        }
        hint(ui, &self.palette, "Above 0 pushes text and readings further from the background; below 0 softens them.");

        section(ui, &self.palette, "Rows");
        ui.checkbox(&mut prefs.row_shading, "Shade every other row");
        ui.horizontal(|ui| {
            ui.label("Corner roundness");
            ui.add(egui::Slider::new(&mut prefs.corner_radius, RADIUS_RANGE.0..=RADIUS_RANGE.1).step_by(1.0).fixed_decimals(0).suffix(" px"));
        });
    }

    fn colors(&mut self, ui: &mut Ui, prefs: &mut Prefs) {
        let palette = self.palette;
        let editing_dark = self.editing_dark.unwrap_or(palette.dark);
        ui.horizontal(|ui| {
            ui.label("Editing");
            let mut dark = editing_dark;
            ui.selectable_value(&mut dark, false, "Light mode");
            ui.selectable_value(&mut dark, true, "Dark mode");
            if dark != editing_dark {
                self.editing_dark = Some(dark);
                self.hex.clear();
            }
        });
        hint(ui, &palette, "Each mode keeps its own colours. Click a swatch to pick, or type a #RRGGBB value.");
        ui.add_space(6.0);
        let defaults = if editing_dark { Colors::DEFAULT_DARK } else { Colors::DEFAULT_LIGHT };
        egui::Grid::new("colour-roles").num_columns(4).spacing(vec2(12.0, 8.0)).show(ui, |ui| {
            for (role, label) in COLOR_ROLES {
                let colors = prefs.colors_mut(editing_dark);
                let mut color = colors.get(role);
                ui.label(*label);
                if egui::color_picker::color_edit_button_srgba(ui, &mut color, egui::color_picker::Alpha::Opaque).changed() {
                    colors.set(role, color);
                    self.hex.remove(*role);
                }
                let key = format!("{}-{role}", if editing_dark { "dark" } else { "light" });
                let text = self.hex.entry(key.clone()).or_insert_with(|| prefs::hex(color));
                let response = ui.add_sized([96.0, 22.0], egui::TextEdit::singleline(text).font(egui::TextStyle::Monospace).char_limit(7));
                if response.changed()
                    && let Some(parsed) = prefs::parse_hex(text)
                {
                    prefs.colors_mut(editing_dark).set(role, parsed);
                }
                if response.lost_focus() {
                    *text = prefs::hex(prefs.colors(editing_dark).get(role));
                }
                let default = defaults.get(role);
                if ui.add_enabled(color != default, egui::Button::new("Default")).clicked() {
                    prefs.colors_mut(editing_dark).set(role, default);
                    self.hex.remove(&key);
                }
                ui.end_row();
            }
        });
        ui.add_space(8.0);
        if ui.button(format!("Reset all {} colours", if editing_dark { "dark" } else { "light" })).clicked() {
            *prefs.colors_mut(editing_dark) = defaults;
            self.hex.clear();
        }
        if !prefs.row_shading {
            hint(ui, &palette, "Row shading is off: turn it on under Appearance to see the shading colour.");
        }
    }

    fn text(&mut self, ui: &mut Ui, prefs: &mut Prefs) {
        let palette = self.palette;
        section(ui, &palette, "Font");
        let families = self.fonts.get_or_insert_with(fonts::installed_families);
        ui.horizontal(|ui| {
            ui.label("Search");
            ui.add(egui::TextEdit::singleline(&mut self.font_filter).desired_width(200.0).hint_text("font name"));
            if ui.add_enabled(!prefs.font_family.eq_ignore_ascii_case(prefs::DEFAULT_FONT), egui::Button::new("Consolas (default)")).clicked() {
                prefs.font_family = prefs::DEFAULT_FONT.into();
            }
        });
        let filter = self.font_filter.to_lowercase();
        egui::Frame::new()
            .stroke(egui::Stroke::new(1.0, palette.border))
            .corner_radius(theme::radius(6))
            .inner_margin(egui::Margin::same(4))
            .show(ui, |ui| {
                egui::ScrollArea::vertical().id_salt("font-list").max_height(190.0).auto_shrink([false, true]).show(ui, |ui| {
                    for family in families.iter().filter(|f| filter.is_empty() || f.name.to_lowercase().contains(&filter)) {
                        let selected = family.name.eq_ignore_ascii_case(&prefs.font_family);
                        let label = if family.monospace { format!("{}  · fixed width", family.name) } else { family.name.clone() };
                        if ui.selectable_label(selected, label).clicked() {
                            prefs.font_family = family.name.clone();
                        }
                    }
                });
            });
        hint(ui, &palette, "Fixed-width fonts keep the columns most even. Consolas is laid out to match the original window exactly.");

        section(ui, &palette, "Size");
        ui.horizontal(|ui| {
            ui.label("Text size");
            let base = theme::FONT_SIZE;
            let mut size = base + prefs.text_delta;
            if ui
                .add(egui::Slider::new(&mut size, base + TEXT_DELTA_RANGE.0..=base + TEXT_DELTA_RANGE.1).step_by(0.5).fixed_decimals(1).suffix(" px"))
                .changed()
            {
                prefs.text_delta = size - base;
            }
        });
        hint(ui, &palette, "The windows grow with the text, and rows grow so it stays clear of the next one.");
        ui.checkbox(&mut prefs.bold_headings, "Bold headings, tabs and buttons");
    }

    fn layout(&mut self, ui: &mut Ui, prefs: &mut Prefs) {
        let palette = self.palette;
        section(ui, &palette, "Scale");
        let mut percent = (prefs.ui_scale * 100.0).round();
        if ui
            .add(egui::Slider::new(&mut percent, SCALE_RANGE.0 * 100.0..=SCALE_RANGE.1 * 100.0).step_by(5.0).fixed_decimals(0).suffix("%"))
            .changed()
        {
            prefs.ui_scale = percent / 100.0;
        }
        hint(ui, &palette, "Everything at once: text, buttons, spacing and the window itself.");

        section(ui, &palette, "Row spacing");
        ui.add(egui::Slider::new(&mut prefs.row_delta, ROW_DELTA_RANGE.0..=ROW_DELTA_RANGE.1).step_by(1.0).fixed_decimals(0).suffix(" px"));
        hint(ui, &palette, "Extra height for every table row; below 0 packs them tighter.");

        section(ui, &palette, "Footer");
        ui.checkbox(&mut prefs.show_footer_links, "Show the YouTube | X | Discord links");
    }

    fn window(&mut self, ui: &mut Ui, prefs: &mut Prefs) {
        let palette = self.palette;
        section(ui, &palette, "Position");
        ui.checkbox(&mut prefs.remember_positions, "Remember where each window was");
        ui.horizontal(|ui| {
            let saved = prefs.positions.len();
            ui.label(RichText::new(format!("{saved} saved")).color(palette.disabled_text));
            if ui.add_enabled(saved > 0, egui::Button::new("Forget saved positions")).clicked() {
                prefs.positions.clear();
                self.requests.forget_positions = true;
            }
        });

        section(ui, &palette, "On screen");
        ui.checkbox(&mut prefs.always_on_top, "Keep the windows above other programs");
        ui.horizontal(|ui| {
            ui.label("Opacity");
            let mut percent = (prefs.opacity * 100.0).round();
            if ui
                .add(egui::Slider::new(&mut percent, OPACITY_RANGE.0 * 100.0..=OPACITY_RANGE.1 * 100.0).step_by(5.0).fixed_decimals(0).suffix("%"))
                .changed()
            {
                prefs.opacity = percent / 100.0;
            }
        });

        section(ui, &palette, "Opening tab");
        ui.checkbox(&mut prefs.remember_tab, "Open on the tab I last used");
        if !prefs.remember_tab {
            hint(ui, &palette, "Otherwise the viewer opens on Summary.");
        }
    }

    fn behavior(&mut self, ui: &mut Ui, prefs: &mut Prefs) {
        let palette = self.palette;
        section(ui, &palette, "Live readings");
        ui.horizontal(|ui| {
            ui.label("Update every");
            let mut seconds = prefs.refresh_ms as f32 / 1000.0;
            if ui
                .add(
                    egui::Slider::new(&mut seconds, REFRESH_RANGE.0 as f32 / 1000.0..=REFRESH_RANGE.1 as f32 / 1000.0)
                        .step_by(0.25)
                        .fixed_decimals(2)
                        .suffix(" s"),
                )
                .changed()
            {
                prefs.refresh_ms = (seconds * 1000.0).round() as u32;
            }
        });
        hint(ui, &palette, "How often the main window reads its live rows again. Faster costs a little more processor time.");

        section(ui, &palette, "Telemetry");
        if ui.checkbox(&mut self.auto_open, "Open Telemetry when the viewer starts").changed() {
            self.requests.auto_open = Some(self.auto_open);
        }

        section(ui, &palette, "Screenshots");
        ui.checkbox(&mut prefs.screenshot_to_clipboard, "Copy each screenshot to the clipboard as well as saving it");
    }
}

fn section(ui: &mut Ui, palette: &Palette, title: &str) {
    ui.add_space(8.0);
    ui.label(RichText::new(title).strong().color(palette.text));
    let (rect, _) = ui.allocate_exact_size(vec2(ui.available_width(), 1.0), egui::Sense::hover());
    ui.painter().rect_filled(rect, 0.0, palette.border);
    ui.add_space(2.0);
}

fn hint(ui: &mut Ui, palette: &Palette, text: &str) {
    ui.label(RichText::new(text).small().color(palette.disabled_text));
}

pub fn viewport_builder(position: Option<(f32, f32)>) -> egui::ViewportBuilder {
    let builder = egui::ViewportBuilder::default()
        .with_title("Settings")
        .with_inner_size([WINDOW_WIDTH, WINDOW_HEIGHT])
        .with_min_inner_size([520.0, 420.0])
        .with_resizable(true);
    popouts::finish_builder(builder, position)
}
