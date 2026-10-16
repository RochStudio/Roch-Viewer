//! The main window: its own title bar, the tab strip with the Telemetry and
//! Advanced tools, the driver notice, the active tab and the footer.
//!
//! The window draws its own top strip rather than wearing the Windows one,
//! so the chrome follows the app's own light/dark colours instead of sitting
//! in a band that ignores them. There is no maximize control: the layout is
//! fitted to its content and a maximized window only adds empty space.

use std::sync::Arc;

use eframe::egui::{self, Align, Color32, CornerRadius, Layout, RichText, Sense, Stroke, TextureHandle, Ui, Vec2};

use rochviewer::hardware;
use rochviewer::platform_profiles::Platform;
use rochviewer::timings::{Backend, load_active_backend};
use rochviewer::version::{APP_NAME, VERSION};

use super::admin;
use super::asset_path;
use super::popouts::{Popouts, Shared};
use super::reader::Reader;
use super::settings::Settings;
use super::tabs::{self, TabContext};
use super::theme::{self, Palette};

pub const TITLE_BAR_HEIGHT: f32 = 30.0;
pub const TITLE_BUTTON_WIDTH: f32 = 44.0;
pub const TITLE_THEME_WIDTH: f32 = 36.0;
pub const LOGO_SIZE: u32 = 24;
pub const LIGHT_MODE_ICON: &str = "☀";
pub const SCREENSHOT_ICON: &str = "\u{1F4F7}";

pub const TOOL_BUTTON_HEIGHT: f32 = 24.0;
pub const TOOL_BUTTON_WIDTH: f32 = 60.0;
pub const TOOL_BUTTON_GAP: f32 = 4.0;

pub const TWITTER_URL: &str = "https://x.com/MateoPCTech";
pub const TWITTER_LABEL: &str = "X";
pub const YOUTUBE_URL: &str = "https://www.youtube.com/@MateoPcTech";
pub const YOUTUBE_LABEL: &str = "YouTube";
pub const DISCORD_URL: &str = "https://discord.gg/KfzExpKQHB";
pub const DISCORD_LABEL: &str = "Discord";
pub const FOOTER_HEIGHT: f32 = 24.0;
pub const GRIP_SIZE: f32 = 14.0;
pub const DRIVER_NOTICE_HEIGHT: f32 = 34.0;

pub const MIN_WINDOW_WIDTH: f32 = 700.0;
/// 650 so AM5's Timings, Training and Voltages can be the 650 they were
/// sized to; every other preset is taller.
pub const MIN_WINDOW_HEIGHT: f32 = 650.0;

/// Each page gets enough room for its own column count and row density.
/// Training is the one wider page: on DDR5 its right column carries the
/// mode-register rows, worded as the reference tools word them.
///
/// System Info is 780 wide on every platform: its two halves came to the
/// whole of a 750px window with nothing to spare.
const TAB_WINDOW_SIZES: &[(&str, (f32, f32))] = &[
    // Larger than the other pages: the Summary's blocks are bordered panels.
    ("Summary", (775.0, 775.0)),
    ("System Info", (780.0, 750.0)),
    ("SPD", (750.0, 750.0)),
    ("Timings", (750.0, 750.0)),
    ("Training", (1010.0, 800.0)),
    ("IMC", (750.0, 1100.0)),
    ("RTL", (750.0, 654.0)),
    ("Voltages", (750.0, 654.0)),
];

/// LGA1700 DDR5, where the per-channel controller registers, MR2-MR4 and
/// Add/Dec tCWL joined Training: 43 lines to each of three columns. Held
/// under 960px tall, what a 1080p screen leaves once 120 is taken off.
const LGA1700_DDR5_TAB_WINDOW_SIZES: &[(&str, (f32, f32))] = &[
    ("Summary", (700.0, 750.0)),
    ("System Info", (775.0, 750.0)),
    ("SPD", (700.0, 816.0)),
    ("Timings", (700.0, 750.0)),
    ("Training", (975.0, 950.0)),
    ("IMC", (700.0, 750.0)),
    ("RTL", (700.0, 750.0)),
    ("Voltages", (700.0, 750.0)),
];

/// Each AM5 page at the least size that holds it, measured on the X870
/// bench.
const AM5_TAB_WINDOW_SIZES: &[(&str, (f32, f32))] = &[
    // The strip has a fourth row Intel's does not (Power Down, Refresh Mode).
    ("Summary", (700.0, 750.0)),
    // The OS edition on the left and the graphics names on the right need
    // 764px across, which 788 holds.
    ("System Info", (788.0, 750.0)),
    // The DDR5 page with organization, PMIC, SPD hub and raw card.
    ("SPD", (700.0, 816.0)),
    // The three equal columns need 718px, which a 700 window cuts off.
    ("Timings", (750.0, 650.0)),
    ("Training", (700.0, 650.0)),
    ("Voltages", (700.0, 650.0)),
];

/// The requested size of one main tab on `platform`, capped to what the
/// screen leaves.
pub fn window_size_for_tab(platform: Platform, tab: &str, screen_height: Option<f32>) -> (f32, f32) {
    let lookup = |table: &[(&str, (f32, f32))]| table.iter().find(|(name, _)| *name == tab).map(|(_, size)| *size);
    let platform_table = match platform {
        Platform::Lga1700Ddr5 => Some(LGA1700_DDR5_TAB_WINDOW_SIZES),
        Platform::Am5 => Some(AM5_TAB_WINDOW_SIZES),
        _ => None,
    };
    let summary = platform_table
        .and_then(|t| t.iter().find(|(n, _)| *n == "Summary").map(|(_, s)| *s))
        .unwrap_or((775.0, 775.0));
    let (width, mut height) = platform_table
        .and_then(lookup)
        .or_else(|| lookup(TAB_WINDOW_SIZES))
        .unwrap_or(summary);
    if let Some(screen) = screen_height {
        height = height.min(MIN_WINDOW_HEIGHT.max(screen - 120.0));
    }
    (width.max(MIN_WINDOW_WIDTH), height.max(MIN_WINDOW_HEIGHT))
}

/// The notice for a driver problem. Separate from the widget so it can be
/// checked without a display.
pub fn driver_notice_text(problem: &str, missing: bool) -> String {
    // The message opens with the sentence that matters and follows it with
    // the directories searched. It is split on ". " rather than "." because
    // the first sentence ends in a filename that has a dot of its own.
    let headline = problem.split(". ").next().unwrap_or("").trim().trim_end_matches('.');
    let advice = if missing {
        "Put it beside this program and restart."
    } else {
        "Restart as administrator, or check it is the 64-bit build."
    };
    format!(
        "{headline} — the readings below come from the memory controller and \
         are unavailable without it. {advice}"
    )
}

pub struct TimingApp {
    backend: Arc<Backend>,
    reader: Reader,
    tab_names: Vec<String>,
    current: usize,
    settings: Settings,
    palette: Palette,
    logo: Option<TextureHandle>,
    moon: Option<TextureHandle>,
    driver_notice: Option<String>,
    sized_for: Option<usize>,
    popouts: Popouts,
    started: bool,
    pending: Vec<Action>,
}

/// What a click asked for, carried out after the frame's layout so the
/// windows can borrow the whole app.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Action {
    Advanced,
    Telemetry,
    Screenshot,
    OpenScreenshotFolder,
}

impl TimingApp {
    pub fn new(cc: &eframe::CreationContext<'_>, settings: Settings) -> Self {
        theme::install_fonts(&cc.egui_ctx);
        let palette = Palette::new(settings.appearance_mode() == "Dark");
        theme::apply(&cc.egui_ctx, &palette);

        let backend = Arc::new(load_active_backend());
        let ctx = cc.egui_ctx.clone();
        let reader = Reader::start(backend.clone(), move || ctx.request_repaint());

        let mut tab_names = rochviewer::display_values::select_tab_names(&backend.rows);
        // SPD is a hardware-inventory page rather than a table row, so it
        // has no entry in the rows; it sits beside System Info.
        if !tab_names.iter().any(|t| t == "SPD") {
            let at = tab_names.iter().position(|t| t == "System Info").map_or(1, |i| i + 1);
            tab_names.insert(at.min(tab_names.len()), "SPD".into());
        }

        let driver_notice = hardware::driver_error().map(|problem| {
            let missing = hardware::driver_path::find_driver().is_none();
            driver_notice_text(&problem, missing)
        });

        let logo = asset_path::logo_image(LOGO_SIZE)
            .map(|img| cc.egui_ctx.load_texture("logo", img, egui::TextureOptions::LINEAR));
        let moon = asset_path::moon_image()
            .map(|img| cc.egui_ctx.load_texture("moon", img, egui::TextureOptions::LINEAR));

        TimingApp {
            backend,
            reader,
            tab_names,
            current: 0,
            settings,
            palette,
            logo,
            moon,
            driver_notice,
            sized_for: None,
            popouts: Popouts::new(),
            started: false,
            pending: Vec::new(),
        }
    }

    fn set_appearance(&mut self, ctx: &egui::Context, dark: bool) {
        self.palette = Palette::new(dark);
        theme::apply(ctx, &self.palette);
        self.settings
            .save("appearance_mode", serde_json::Value::from(if dark { "Dark" } else { "Light" }));
    }

    fn resize_for_current_tab(&mut self, ctx: &egui::Context) {
        if self.sized_for == Some(self.current) {
            return;
        }
        self.sized_for = Some(self.current);
        let screen = ctx.input(|i| i.viewport().monitor_size.map(|s| s.y));
        let (w, h) = window_size_for_tab(self.backend.platform, &self.tab_names[self.current], screen);
        ctx.send_viewport_cmd(egui::ViewportCommand::InnerSize(Vec2::new(w, h)));
    }

    fn title_bar(&mut self, ui: &mut Ui) {
        let palette = self.palette;
        let ctx = ui.ctx().clone();
        let bar = ui.allocate_ui_with_layout(
            Vec2::new(ui.available_width(), TITLE_BAR_HEIGHT),
            Layout::left_to_right(Align::Center),
            |ui| {
                ui.set_min_height(TITLE_BAR_HEIGHT);
                ui.add_space(8.0);
                if let Some(logo) = &self.logo {
                    ui.add(egui::Image::new(logo).fit_to_exact_size(Vec2::splat(LOGO_SIZE as f32)));
                }
                ui.add_space(6.0);
                ui.label(RichText::new(format!("{APP_NAME} {VERSION}")).font(theme::bold(theme::FONT_SIZE)).color(palette.text));

                ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                    if title_button(ui, &palette, "×", theme::TITLE_CLOSE_SIZE, TITLE_BUTTON_WIDTH, palette.close_hover).clicked() {
                        ctx.send_viewport_cmd(egui::ViewportCommand::Close);
                    }
                    if title_button(ui, &palette, "−", theme::TITLE_MINIMIZE_SIZE, TITLE_BUTTON_WIDTH, palette.tab_unselected_hover).clicked() {
                        ctx.send_viewport_cmd(egui::ViewportCommand::Minimized(true));
                    }
                    // The icon names the mode the button will switch to.
                    let toggled = if palette.dark {
                        title_button(ui, &palette, LIGHT_MODE_ICON, theme::TITLE_THEME_SIZE, TITLE_THEME_WIDTH, palette.tab_unselected_hover).clicked()
                    } else {
                        image_button(ui, &palette, self.moon.as_ref(), TITLE_THEME_WIDTH).clicked()
                    };
                    if toggled {
                        self.set_appearance(&ctx, !palette.dark);
                    }
                    // A picture of the window, saved and put on the clipboard.
                    if title_button(ui, &palette, SCREENSHOT_ICON, theme::TITLE_SCREENSHOT_SIZE, TITLE_THEME_WIDTH, palette.tab_unselected_hover).clicked() {
                        self.pending.push(Action::Screenshot);
                    }
                    // Where the last picture went, for a few seconds; a click
                    // opens the folder.
                    if let Some(status) = self.popouts.screenshot_status() {
                        let response = ui
                            .add(egui::Label::new(RichText::new(status).font(theme::font(theme::FONT_SIZE)).color(palette.text)).sense(Sense::click()))
                            .on_hover_cursor(egui::CursorIcon::PointingHand);
                        if response.clicked() {
                            self.pending.push(Action::OpenScreenshotFolder);
                        }
                        ui.ctx().request_repaint_after(std::time::Duration::from_millis(250));
                    }
                });
            },
        );
        // The strip, its title and the logo all drag.
        let drag = ui.interact(bar.response.rect, ui.scope_id().with("title-drag"), Sense::click_and_drag());
        if drag.drag_started() {
            ctx.send_viewport_cmd(egui::ViewportCommand::StartDrag);
        }
    }

    fn tab_strip(&mut self, ui: &mut Ui) {
        let palette = self.palette;
        ui.horizontal(|ui| {
            ui.set_min_height(36.0);
            ui.add_space(2.0);
            let mut clicked = None;
            for (i, name) in self.tab_names.iter().enumerate() {
                let selected = i == self.current;
                let galley = ui.painter().layout_no_wrap(name.clone(), theme::bold(theme::TAB_FONT_SIZE), palette.text);
                let size = Vec2::new(galley.size().x + 16.0, 26.0);
                let (rect, response) = ui.allocate_exact_size(size, Sense::click());
                let fill = if selected {
                    if response.hovered() { palette.tab_hover } else { palette.tab_selected }
                } else if response.hovered() {
                    palette.tab_unselected_hover
                } else {
                    palette.bg
                };
                ui.painter().rect(rect, CornerRadius::same(8), fill, Stroke::new(1.0, palette.border), egui::StrokeKind::Inside);
                let color = if selected { palette.tab_selected_text } else { palette.text };
                ui.painter().text(rect.center(), egui::Align2::CENTER_CENTER, name, theme::bold(theme::TAB_FONT_SIZE), color);
                if response.clicked() {
                    clicked = Some(i);
                }
            }
            if let Some(i) = clicked {
                self.current = i;
            }
            ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                ui.add_space(4.0);
                // Every row from the reading tabs in one searchable list.
                if tool_button(ui, &palette, "Advanced").clicked() {
                    self.pending.push(Action::Advanced);
                }
                ui.add_space(TOOL_BUTTON_GAP);
                // Everything that moves, in its own window: it polls and
                // keeps min/max/average, which does not belong on a tab that
                // reads settings.
                if tool_button(ui, &palette, "Telemetry").clicked() {
                    self.pending.push(Action::Telemetry);
                }
            });
        });
    }

    fn footer(&mut self, ui: &mut Ui) {
        let palette = self.palette;
        ui.horizontal(|ui| {
            ui.set_min_height(FOOTER_HEIGHT);
            ui.add_space(4.0);
            for (i, (label, url)) in [(YOUTUBE_LABEL, YOUTUBE_URL), (TWITTER_LABEL, TWITTER_URL), (DISCORD_LABEL, DISCORD_URL)]
                .into_iter()
                .enumerate()
            {
                if i > 0 {
                    ui.add_space(2.0);
                    ui.label(RichText::new("|").font(theme::font(theme::FONT_SIZE)).color(palette.text));
                    ui.add_space(2.0);
                }
                // Colour is the whole affordance here -- there is no button
                // edge to tell you it is clickable, so it lifts a shade under
                // the pointer.
                let id = ui.scope_id().with(label);
                let hovered = ui.ctx().memory(|m| m.data.get_temp::<bool>(id)).unwrap_or(false);
                let color = if hovered { palette.brand_hover } else { palette.brand };
                let response = ui
                    .add(egui::Label::new(RichText::new(label).font(theme::bold(theme::FONT_SIZE)).color(color)).sense(Sense::click()))
                    .on_hover_cursor(egui::CursorIcon::PointingHand);
                ui.ctx().memory_mut(|m| m.data.insert_temp(id, response.hovered()));
                if response.clicked() {
                    admin::shell_open(url);
                }
            }
            ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                // No glyph: the corner is the only handle, and the pointer
                // announces it.
                let (rect, response) = ui.allocate_exact_size(Vec2::splat(GRIP_SIZE), Sense::drag());
                let _ = rect;
                let response = response.on_hover_cursor(egui::CursorIcon::ResizeSouthEast);
                if response.drag_started() {
                    ui.ctx().send_viewport_cmd(egui::ViewportCommand::BeginResize(egui::ResizeDirection::SouthEast));
                }
            });
        });
    }

    fn notice(&self, ui: &mut Ui) {
        let Some(text) = &self.driver_notice else { return };
        let palette = self.palette;
        egui::Frame::new()
            .fill(palette.bg)
            .stroke(Stroke::new(1.0, palette.brand))
            .corner_radius(CornerRadius::same(6))
            .inner_margin(egui::Margin::symmetric(8, 4))
            .show(ui, |ui| {
                ui.set_min_height(DRIVER_NOTICE_HEIGHT - 8.0);
                ui.set_width(ui.available_width());
                ui.label(RichText::new(text).font(theme::bold(theme::FONT_SIZE)).color(palette.brand));
            });
    }
}

fn title_button(ui: &mut Ui, palette: &Palette, text: &str, size: f32, width: f32, hover: Color32) -> egui::Response {
    let (rect, response) = ui.allocate_exact_size(Vec2::new(width, TITLE_BAR_HEIGHT), Sense::click());
    if response.hovered() {
        ui.painter().rect_filled(rect, 0.0, hover);
    }
    ui.painter().text(rect.center(), egui::Align2::CENTER_CENTER, text, theme::symbol(size), palette.text);
    response
}

fn image_button(ui: &mut Ui, palette: &Palette, image: Option<&TextureHandle>, width: f32) -> egui::Response {
    let (rect, response) = ui.allocate_exact_size(Vec2::new(width, TITLE_BAR_HEIGHT), Sense::click());
    if response.hovered() {
        ui.painter().rect_filled(rect, 0.0, palette.tab_unselected_hover);
    }
    if let Some(texture) = image {
        let size = texture.size_vec2();
        let image_rect = egui::Rect::from_center_size(rect.center(), size);
        ui.painter().image(texture.id(), image_rect, egui::Rect::from_min_max(egui::pos2(0.0, 0.0), egui::pos2(1.0, 1.0)), Color32::WHITE);
    }
    response
}

fn tool_button(ui: &mut Ui, palette: &Palette, text: &str) -> egui::Response {
    let (rect, response) = ui.allocate_exact_size(Vec2::new(TOOL_BUTTON_WIDTH + 24.0, TOOL_BUTTON_HEIGHT), Sense::click());
    let fill = if response.hovered() { palette.tab_unselected_hover } else { palette.bg };
    ui.painter().rect(rect, CornerRadius::same(8), fill, Stroke::new(1.0, palette.border), egui::StrokeKind::Inside);
    ui.painter().text(rect.center(), egui::Align2::CENTER_CENTER, text, theme::bold(theme::TAB_FONT_SIZE), palette.text);
    response
}

impl eframe::App for TimingApp {
    fn ui(&mut self, ui: &mut Ui, _frame: &mut eframe::Frame) {
        let ctx = ui.ctx().clone();
        self.resize_for_current_tab(&ctx);
        let palette = self.palette;
        let frame = egui::Frame::new().fill(palette.bg);
        egui::Panel::top("title-bar").exact_size(TITLE_BAR_HEIGHT).frame(frame).show(ui, |ui| self.title_bar(ui));
        egui::Panel::bottom("footer").exact_size(FOOTER_HEIGHT).frame(frame).show(ui, |ui| self.footer(ui));
        egui::CentralPanel::default()
            .frame(frame.inner_margin(egui::Margin::same(4)))
            .show(ui, |ui| {
                self.notice(ui);
                self.tab_strip(ui);
                ui.add_space(4.0);
                let tab = self.tab_names[self.current].clone();
                let context = TabContext { rows: &self.backend.rows, values: &self.reader.values, palette: &self.palette };
                tabs::show_generic(ui, &context, &tab);
            });

        let actions: Vec<Action> = self.pending.drain(..).collect();
        let current_tab = self.tab_names[self.current].clone();
        let mut shared = Shared {
            backend: &self.backend,
            values: &self.reader.values,
            palette: &self.palette,
            settings: &mut self.settings,
            tab_names: &self.tab_names,
            current_tab: &current_tab,
        };
        if !self.started {
            self.started = true;
            self.popouts.startup(&ctx, &mut shared);
        }
        for action in actions {
            match action {
                Action::Advanced => self.popouts.open_advanced(&ctx, &mut shared),
                Action::Telemetry => self.popouts.open_telemetry(&ctx, &mut shared),
                Action::Screenshot => self.popouts.take_screenshot(&ctx, &mut shared),
                Action::OpenScreenshotFolder => self.popouts.open_screenshot_folder(),
            }
        }
        self.popouts.show(&ctx, &mut shared);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sizes_follow_the_platform_and_the_screen() {
        assert_eq!(window_size_for_tab(Platform::Lga1700Ddr4, "Summary", None), (775.0, 775.0));
        assert_eq!(window_size_for_tab(Platform::Am5, "Timings", None), (750.0, 650.0));
        assert_eq!(window_size_for_tab(Platform::Lga1700Ddr5, "Training", None), (975.0, 950.0));
        // IMC is 1100 tall on DDR4; a 1080p screen leaves 960.
        assert_eq!(window_size_for_tab(Platform::Lga1700Ddr4, "IMC", Some(1080.0)), (750.0, 960.0));
        // Never below the minimum, however small the screen.
        assert_eq!(window_size_for_tab(Platform::Am5, "Summary", Some(600.0)).1, MIN_WINDOW_HEIGHT);
        // An unknown tab takes the platform's Summary size.
        assert_eq!(window_size_for_tab(Platform::Am5, "Jedec", None), (700.0, 750.0));
    }

    #[test]
    fn driver_notice_keeps_the_first_sentence() {
        let problem = "inpoutx64.dll not found. It is a third-party component and is not distributed";
        assert_eq!(
            driver_notice_text(problem, true),
            "inpoutx64.dll not found — the readings below come from the memory controller and \
             are unavailable without it. Put it beside this program and restart."
        );
        assert!(driver_notice_text("Found x but could not load it.", false).ends_with("check it is the 64-bit build."));
    }
}
