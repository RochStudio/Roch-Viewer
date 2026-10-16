//! The main window: its own title bar, the tab strip with the Telemetry and
//! Advanced tools, the driver notice, the active tab and the footer.
//!
//! The window draws its own top strip rather than wearing the Windows one,
//! so the chrome follows the app's own light/dark colours instead of sitting
//! in a band that ignores them. There is no maximize control: the layout is
//! fitted to its content and a maximized window only adds empty space.
//!
//! Everything is placed on the pixel grid the Tk window used -- its frames,
//! paddings and insets are spelled out in [`Chrome`] -- so a tab starts,
//! and its selector and footer sit, where they always did.

use std::collections::BTreeMap;
use std::sync::Arc;

use eframe::egui::{self, Color32, CornerRadius, Id, Rect, Sense, Stroke, TextureHandle, Ui, Vec2, pos2, vec2};

use rochviewer::hardware;
use rochviewer::memory::dimm_inventory::{read_modules, read_modules_with_spd_ic};
use rochviewer::platform_profiles::Platform;
use rochviewer::row::Channel;
use rochviewer::timings::{Backend, load_active_backend};
use rochviewer::version::{APP_NAME, VERSION};

use super::admin;
use super::asset_path;
use super::draw::{self, Edge};
use super::fixture;
use super::layout::channel_slot_labels;
use super::popouts::{Popouts, Shared};
use super::reader::Reader;
use super::selector::{self, ModuleChoices, SELECTOR_HEIGHT};
use super::settings::Settings;
use super::spd_tab::{self, SpdTab};
use super::summary::{self, SummaryPlan};
use super::tabs::{self, TabContext};
use super::theme::{self, Palette};

pub const TITLE_BAR_HEIGHT: f32 = 30.0;
pub const TITLE_BUTTON_WIDTH: f32 = 44.0;
pub const TITLE_THEME_WIDTH: f32 = 36.0;
pub const LOGO_SIZE: u32 = 24;
pub const LIGHT_MODE_ICON: &str = "☀";
#[allow(dead_code)]
pub const DARK_MODE_IMAGE: &str = "assets/moon-solid-black-18.png";
pub const SCREENSHOT_ICON: &str = "\u{1F4F7}";

/// The compact window has room for the tab list and the two utility buttons
/// on one line, so the tools sit on the tab strip's own baseline.
pub const TOOL_BUTTON_HEIGHT: f32 = 24.0;
pub const TOOL_BUTTON_WIDTH: f32 = 60.0;
pub const TOOL_BUTTON_GAP: f32 = 4.0;

pub const TWITTER_URL: &str = "https://x.com/MateoPCTech";
#[allow(dead_code)]
pub const TWITTER_HANDLE: &str = "@MateoPCTech";
pub const TWITTER_LABEL: &str = "X";
pub const YOUTUBE_URL: &str = "https://www.youtube.com/@MateoPcTech";
pub const YOUTUBE_LABEL: &str = "YouTube";
pub const DISCORD_URL: &str = "https://discord.gg/KfzExpKQHB";
pub const DISCORD_LABEL: &str = "Discord";
pub const FOOTER_HEIGHT: f32 = 24.0;
pub const GRIP_SIZE: f32 = 14.0;

/// Shown only when the driver is missing, which is the state every new user
/// starts in: it is not distributed with this project, so the first run has
/// no way to read a register.
///
/// Without this the window comes up looking healthy -- the CPU, the board,
/// the BIOS and the module identity all arrive over WMI and need no driver
/// at all -- while every register row reads N/A. There is nothing on screen
/// to say why, so the honest conclusions available to a user are "my
/// hardware is not supported" or "this is broken", and neither is true.
pub const DRIVER_NOTICE_HEIGHT: f32 = 34.0;

pub const MIN_WINDOW_WIDTH: f32 = 700.0;
/// 650 so AM5's Timings, Training and Voltages can be the 650 they were
/// sized to; every other preset is taller.
pub const MIN_WINDOW_HEIGHT: f32 = 650.0;

/// Tabs drawn without a scrollbar, because the window is sized to fit them
/// whole. Anything taller than the window would be cut off unseen here
/// rather than reachable, so a tab only belongs on this list while its
/// content clears the viewport. Every main tab fits at its requested height,
/// including the tall Training table, so none gives up width to a scrollbar
/// gutter.
#[allow(dead_code)]
pub const UNSCROLLED_TABS: &[&str] = &["Summary", "System Info", "SPD", "Timings", "Training", "IMC", "RTL", "Voltages"];

/// The tabs that carry the module selector at their foot.
pub const MODULE_SELECTOR_TABS: &[&str] = &["Summary", "Timings", "Training"];

/// Each page gets enough room for its own column count and row density.
/// Training is the one wider page: on DDR5 its right column carries the
/// mode-register rows, worded as the reference tools word them ("Package
/// Output Driver Test Mode", "Timer Stops at 2048th clocks"), and at 750
/// that column ran 260px past the window's edge.
///
/// System Info is 780 wide on every platform: its two halves came to the
/// whole of a 750px window with nothing to spare, so its right-hand values
/// ran against the border on a display whose font renders a little wider.
pub const TAB_WINDOW_SIZES: &[(&str, (f32, f32))] = &[
    // Larger than the other pages: the Summary's blocks are bordered panels,
    // which cost 17px of height over the rules that divided them, and the
    // width gives the three columns room to breathe.
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
/// Add/Dec tCWL joined Training: 43 lines to each of three columns, with
/// Refresh's names widening the middle one. Held under 960px tall, what a
/// 1080p screen leaves once window_size_for_tab takes its 120 off -- at 1000
/// the last row was cut off there, with no scrollbar to reach it. IMC gave
/// up the same rows, and shares one size with the other pages that are not
/// wide tables. Training's three column panels need 954px since the
/// capability rows left it; 975 is the size chosen for them.
pub const LGA1700_DDR5_TAB_WINDOW_SIZES: &[(&str, (f32, f32))] = &[
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
pub const AM5_TAB_WINDOW_SIZES: &[(&str, (f32, f32))] = &[
    // The strip has a fourth row Intel's does not (Power Down, Refresh
    // Mode); 750 tall holds it with the DIMM rails closing the middle column
    // rather than lengthening the third.
    ("Summary", (700.0, 750.0)),
    // Two halves: the OS edition on the left and the graphics names on the
    // right need 764px across, which 788 holds. The power limits in the
    // right column make the content 630px tall; 747 is the least.
    ("System Info", (788.0, 750.0)),
    // The DDR5 page, with organization, PMIC, SPD hub and raw card, and
    // tCCD_L_WR, tWTR_L and tWTR_S in the profile table: 699px of content.
    // 700 wide like the Summary. LGA1700 DDR5 draws the same page and takes
    // the same height.
    ("SPD", (700.0, 816.0)),
    // PHY closes the middle column, so the tallest column is the first at
    // 22 rows, and 650 -- the window minimum -- holds it with rows to spare.
    // 750 wide: the three equal columns need 718px, which a 700 window cuts
    // off.
    ("Timings", (750.0, 650.0)),
    // With the preambles worded briefly and FGR off the tab, Training's
    // columns need 450px across and fit the 650 minimum with rows spare; 700
    // wide matches the Summary and SPD.
    ("Training", (700.0, 650.0)),
    // 700 like the Summary, now the "reopen to update" note that was its
    // widest line is gone: the columns need 441px.
    ("Voltages", (700.0, 650.0)),
];

/// Every tab's size on `platform`: the base table with the platform's own
/// sizes laid over it.
pub fn tab_window_sizes(platform: Platform) -> Vec<(&'static str, (f32, f32))> {
    let overlay: &[(&str, (f32, f32))] = match platform {
        Platform::Lga1700Ddr5 => LGA1700_DDR5_TAB_WINDOW_SIZES,
        Platform::Am5 => AM5_TAB_WINDOW_SIZES,
        _ => &[],
    };
    TAB_WINDOW_SIZES
        .iter()
        .map(|(tab, size)| (*tab, overlay.iter().find(|(t, _)| t == tab).map_or(*size, |(_, s)| *s)))
        .collect()
}

/// The requested size of one main tab on `platform`, capped to what the
/// screen leaves. An unknown tab takes the Summary's size.
pub fn window_size_for_tab(platform: Platform, tab: &str, screen_height: Option<f32>) -> (f32, f32) {
    let sizes = tab_window_sizes(platform);
    let summary = sizes.iter().find(|(t, _)| *t == "Summary").map(|(_, s)| *s).unwrap_or((775.0, 775.0));
    let (width, mut height) = sizes.iter().find(|(t, _)| *t == tab).map_or(summary, |(_, s)| *s);
    if let Some(screen) = screen_height {
        height = height.min(MIN_WINDOW_HEIGHT.max(screen - 120.0));
    }
    (width.max(MIN_WINDOW_WIDTH), height.max(MIN_WINDOW_HEIGHT))
}

/// Where the window opens: centred across, and down from the middle by the
/// tallest tab's height, so shorter tabs then grow downward from the same
/// title-bar position when selected.
pub fn startup_position(platform: Platform, screen: Vec2) -> egui::Pos2 {
    let (width, _) = window_size_for_tab(platform, "Summary", Some(screen.y));
    let tallest = tab_window_sizes(platform)
        .iter()
        .map(|(tab, _)| window_size_for_tab(platform, tab, Some(screen.y)).1)
        .fold(0.0, f32::max);
    pos2(((screen.x - width) / 2.0).max(0.0).floor(), ((screen.y - tallest) / 2.0).max(0.0).floor())
}

/// The notice for a driver problem, or `None` when there is not one.
/// Separate from the widget so it can be checked without a display.
pub fn driver_notice_text(problem: Option<&str>, missing: bool) -> Option<String> {
    let problem = problem.filter(|p| !p.is_empty())?;
    // The message opens with the sentence that matters and follows it with
    // the directories searched -- more than one line can hold. It is split
    // on ". " rather than "." because the first sentence ends in a filename
    // that has a dot of its own, and splitting there left the notice reading
    // "inpoutx64".
    let headline = problem.split(". ").next().unwrap_or("").trim().trim_end_matches('.');
    let advice = if missing {
        "Put it beside this program and restart."
    } else {
        "Restart as administrator, or check it is the 64-bit build."
    };
    Some(format!(
        "{headline} — the readings below come from the memory controller and \
         are unavailable without it. {advice}"
    ))
}

/// The Tk window's frames as rects: the title bar, the main frame inside the
/// root's padding, the footer packed to its foot, the notice, the tab view
/// and the tab page inside the tab view's rounded inset.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Chrome {
    pub title_bar: Rect,
    pub footer: Rect,
    pub notice: Option<Rect>,
    pub tabview: Rect,
    pub page: Rect,
}

impl Chrome {
    pub fn new(window: Rect, notice: bool) -> Self {
        let title_bar = Rect::from_min_size(window.min, vec2(window.width(), TITLE_BAR_HEIGHT));
        // The main frame: 2px in from the sides, 2 below the title bar and 4
        // above the window's foot.
        let main = Rect::from_min_max(pos2(window.left() + 2.0, title_bar.bottom() + 2.0), pos2(window.right() - 2.0, window.bottom() - 4.0));
        // Packed first, so it claims the bottom of the frame and the tabs
        // take what is left.
        let footer =
            Rect::from_min_max(pos2(main.left() + 4.0, main.bottom() - 1.0 - FOOTER_HEIGHT), pos2(main.right() - 4.0, main.bottom() - 1.0));
        let notice = notice.then(|| {
            Rect::from_min_size(pos2(main.left() + 4.0, main.top() + 4.0), vec2(main.width() - 8.0, DRIVER_NOTICE_HEIGHT))
        });
        let top = notice.map_or(main.top(), |n| n.bottom());
        let tabview = Rect::from_min_max(pos2(main.left() + 2.0, top + 2.0), pos2(main.right() - 2.0, footer.top() - 2.0));
        let page = Rect::from_min_max(pos2(tabview.left() + 8.0, tabview.top() + 44.0), pos2(tabview.right() - 8.0, tabview.bottom() - 8.0));
        Chrome { title_bar, footer, notice, tabview, page }
    }

    /// The module selector at the foot of a tab page, border included.
    pub fn selector(&self) -> Rect {
        Rect::from_min_max(pos2(self.page.left(), self.page.bottom() - 1.0 - SELECTOR_HEIGHT - 2.0), pos2(self.page.right(), self.page.bottom() - 1.0))
    }

    /// Where a tab's own content goes: the page less the selector below it
    /// and the 2px (Summary: 0px) margin the content frame keeps.
    pub fn content(&self, tab: &str) -> Rect {
        let bottom = if MODULE_SELECTOR_TABS.contains(&tab) { self.selector().top() - 3.0 } else { self.page.bottom() };
        // Summary is sized close to its content, so the 1px margin above and
        // below is left off rather than spent.
        let pady = if tab == "Summary" { 0.0 } else { 1.0 };
        Rect::from_min_max(pos2(self.page.left() + 2.0, self.page.top() + pady), pos2(self.page.right() - 2.0, bottom - pady))
    }
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
    positioned: bool,
    popouts: Popouts,
    started: bool,
    pending: Vec<Action>,
    /// The module selection every module-aware tab shows.
    selected: Option<Channel>,
    choices: ModuleChoices,
    channel_labels: BTreeMap<String, String>,
    summary: SummaryPlan,
    spd: SpdTab,
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

/// The tabs the window offers for these rows.
pub fn tab_names_for(rows: &[rochviewer::row::Row]) -> Vec<String> {
    let mut tab_names = rochviewer::display_values::select_tab_names(rows);
    // SPD is a hardware-inventory page rather than a timing-profile row, so
    // it has no synthetic entry in the table. Keep it beside System Info,
    // where the physical module selector and profile table belong.
    if !tab_names.iter().any(|t| t == "SPD") {
        let at = tab_names.iter().position(|t| t == "System Info").map_or(1, |i| i + 1);
        tab_names.insert(at.min(tab_names.len()), "SPD".into());
    }
    tab_names
}

/// The rows on `tab` that are re-taken each time it is opened: those that
/// carry a `refresh`.
pub fn reread_rows(rows: &[rochviewer::row::Row], tab: &str) -> Vec<usize> {
    rows.iter()
        .enumerate()
        .filter(|(_, row)| row.tab == tab && row.refresh.is_some() && !row.extra_bool("diagnostic"))
        .map(|(i, _)| i)
        .collect()
}

impl TimingApp {
    pub fn new(cc: &eframe::CreationContext<'_>, settings: Settings) -> Self {
        theme::install_fonts(&cc.egui_ctx);
        let palette = Palette::new(settings.appearance_mode() == "Dark");
        theme::apply(&cc.egui_ctx, &palette);

        let backend = Arc::new(fixture::backend_from_env().unwrap_or_else(load_active_backend));
        let ctx = cc.egui_ctx.clone();
        let reader = Reader::start(backend.clone(), move || ctx.request_repaint());
        let tab_names = tab_names_for(&backend.rows);

        let driver_notice = driver_notice_text(hardware::driver_error().as_deref(), hardware::driver_path::find_driver().is_none());

        let logo = asset_path::logo_image(LOGO_SIZE).map(|img| cc.egui_ctx.load_texture("logo", img, egui::TextureOptions::LINEAR));
        let moon = asset_path::moon_image().map(|img| cc.egui_ctx.load_texture("moon", img, egui::TextureOptions::LINEAR));

        // The installed modules, read once: the selector's choices with the
        // DRAM maker and die the modules' own SPD reports, and the slot
        // names for the channel columns.
        //
        // On a thread of its own: WMI wants COM in the multithreaded
        // apartment, and the window's thread is already a single-threaded
        // one (the windowing layer initialises OLE there).
        let (choices, channel_labels) = std::thread::Builder::new()
            .name("module inventory".into())
            .spawn(|| (ModuleChoices::from_modules(&read_modules_with_spd_ic(false)), channel_slot_labels(&read_modules(false))))
            .ok()
            .and_then(|thread| thread.join().ok())
            .unwrap_or_else(|| (ModuleChoices::new(&[], &[]), BTreeMap::new()));
        let summary = summary::summary_plan(&backend.rows, backend.platform);

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
            positioned: false,
            popouts: Popouts::new(),
            started: false,
            pending: Vec::new(),
            selected: None,
            choices,
            channel_labels,
            summary,
            spd: SpdTab::default(),
        }
    }

    fn set_appearance(&mut self, ctx: &egui::Context, dark: bool) {
        self.palette = Palette::new(dark);
        theme::apply(ctx, &self.palette);
        self.settings.save("appearance_mode", serde_json::Value::from(if dark { "Dark" } else { "Light" }));
    }

    fn current_tab(&self) -> &str {
        &self.tab_names[self.current]
    }

    /// Apply the active page's size, below the current top-left corner.
    fn resize_for_current_tab(&mut self, ctx: &egui::Context) {
        let screen = ctx.input(|i| i.viewport().monitor_size);
        if !self.positioned
            && let Some(screen) = screen
        {
            self.positioned = true;
            ctx.send_viewport_cmd(egui::ViewportCommand::OuterPosition(startup_position(self.backend.platform, screen)));
        }
        if self.sized_for == Some(self.current) {
            return;
        }
        self.sized_for = Some(self.current);
        let (w, h) = window_size_for_tab(self.backend.platform, self.current_tab(), screen.map(|s| s.y));
        ctx.send_viewport_cmd(egui::ViewportCommand::InnerSize(Vec2::new(w, h)));
    }

    /// A tab was chosen: its size, its SPD read, and a fresh read of its
    /// snapshot rows.
    fn select_tab(&mut self, ctx: &egui::Context, index: usize) {
        if index == self.current {
            return;
        }
        self.current = index;
        let tab = self.current_tab().to_string();
        if tab == "SPD" {
            self.spd.load(ctx);
        }
        // A row may carry `refresh`, which drops its cached reading; every
        // such row on the tab is refreshed and re-read each time the tab is
        // opened. Rows without it -- and every Intel row -- are untouched.
        let reread = reread_rows(&self.backend.rows, &tab);
        if !reread.is_empty() {
            self.reader.reread(reread);
        }
    }

    /// Select one physical DIMM across every module-aware tab.
    fn select_module(&mut self, channel: Option<Channel>) {
        if self.selected != channel {
            self.selected = channel;
            self.reader.select_channel(channel);
        }
    }

    fn title_bar(&mut self, ui: &mut Ui, rect: Rect) {
        let palette = self.palette;
        let ctx = ui.ctx().clone();
        let painter = ui.painter().clone();
        // The strip, its title and the logo all drag: registered first, so
        // the buttons on it take their own clicks.
        let drag = ui.interact(rect, Id::unique("title-drag"), Sense::click_and_drag());
        if drag.drag_started() {
            ctx.send_viewport_cmd(egui::ViewportCommand::StartDrag);
        }
        if let Some(logo) = &self.logo {
            let logo_rect = Rect::from_min_size(pos2(rect.left() + 8.0, rect.top() + 3.0), Vec2::splat(LOGO_SIZE as f32));
            painter.image(logo.id(), logo_rect, Rect::from_min_max(pos2(0.0, 0.0), pos2(1.0, 1.0)), Color32::WHITE);
        }
        // The window's own name, in the plain text colour rather than the
        // brand red. The logo beside it already carries the brand, and the
        // red is what the tab strip and the section headings use to mark
        // what is selected or what a block is.
        draw::text_in_row(&painter, rect, rect.left() + 38.0, Edge::Left, &format!("{APP_NAME} {VERSION}"), &theme::bold(theme::FONT_SIZE), palette.text);

        let mut right = rect.right();
        let mut button = |ui: &mut Ui, width: f32, id: &str| {
            let r = Rect::from_min_max(pos2(right - width, rect.top()), pos2(right, rect.bottom()));
            right -= width;
            (r, ui.interact(r, Id::unique(id), Sense::click()))
        };
        let (close, response) = button(ui, TITLE_BUTTON_WIDTH, "title-close");
        title_glyph(&painter, close, &response, palette.close_hover, "×", theme::TITLE_CLOSE_SIZE, palette.text);
        if response.clicked() {
            ctx.send_viewport_cmd(egui::ViewportCommand::Close);
        }
        let (minimize, response) = button(ui, TITLE_BUTTON_WIDTH, "title-minimize");
        title_glyph(&painter, minimize, &response, palette.tab_unselected_hover, "−", theme::TITLE_MINIMIZE_SIZE, palette.text);
        if response.clicked() {
            ctx.send_viewport_cmd(egui::ViewportCommand::Minimized(true));
        }
        // The icon names the mode the button will switch to: the white sun
        // in Dark, the solid moon in Light.
        let (theme_rect, response) = button(ui, TITLE_THEME_WIDTH, "title-theme");
        if palette.dark {
            title_glyph(&painter, theme_rect, &response, palette.tab_unselected_hover, LIGHT_MODE_ICON, theme::TITLE_THEME_SIZE, palette.text);
        } else {
            if response.hovered() {
                painter.rect_filled(theme_rect, 0.0, palette.tab_unselected_hover);
            }
            if let Some(moon) = &self.moon {
                let image = Rect::from_center_size(theme_rect.center(), moon.size_vec2());
                painter.image(moon.id(), image, Rect::from_min_max(pos2(0.0, 0.0), pos2(1.0, 1.0)), Color32::WHITE);
            }
        }
        if response.clicked() {
            self.set_appearance(&ctx, !palette.dark);
        }
        // A picture of the window, saved and put on the clipboard. In the
        // title bar rather than beside Telemetry and Advanced because the tab
        // strip has no room to spare on the 700px presets.
        let (shot, response) = button(ui, TITLE_THEME_WIDTH, "title-screenshot");
        title_glyph(&painter, shot, &response, palette.tab_unselected_hover, SCREENSHOT_ICON, theme::TITLE_SCREENSHOT_SIZE, palette.text);
        if response.clicked() {
            self.pending.push(Action::Screenshot);
        }
        // Where the last picture went, for a few seconds; a click opens the
        // folder.
        if let Some(status) = self.popouts.screenshot_status() {
            let font = theme::font(theme::FONT_SIZE);
            let width = draw::text_width(&painter, status, &font);
            let r = Rect::from_min_max(pos2(right - 4.0 - width, rect.top()), pos2(right - 4.0, rect.bottom()));
            draw::text_in_row(&painter, r, r.left(), Edge::Left, status, &font, palette.text);
            let response = ui.interact(r, Id::unique("screenshot-status"), Sense::click()).on_hover_cursor(egui::CursorIcon::PointingHand);
            if response.clicked() {
                self.pending.push(Action::OpenScreenshotFolder);
            }
            ui.ctx().request_repaint_after(std::time::Duration::from_millis(250));
        }
    }

    fn tab_strip(&mut self, ui: &mut Ui, chrome: &Chrome) {
        let palette = self.palette;
        let painter = ui.painter().clone();
        let font = theme::bold(theme::TAB_FONT_SIZE);
        // The segmented button sits on the tab page's left edge, one pixel
        // in for its border; each tab is its name and 8px either side.
        let top = chrome.tabview.top() + 11.0;
        let mut x = chrome.page.left() + 1.0;
        let mut clicked = None;
        for (i, name) in self.tab_names.iter().enumerate() {
            let width = draw::text_width(&painter, name, &font) + 16.0;
            let rect = Rect::from_min_size(pos2(x, top), vec2(width, 24.0));
            x = rect.right() + 2.0;
            let response = ui.interact(rect, Id::unique(("tab", i)), Sense::click());
            let selected = i == self.current;
            let fill = match (selected, response.hovered()) {
                (true, true) => palette.tab_hover,
                (true, false) => palette.tab_selected,
                (false, true) => palette.tab_unselected_hover,
                (false, false) => palette.bg,
            };
            painter.rect_filled(rect, CornerRadius::same(6), fill);
            // White on the selected red tab in both modes.
            let color = if selected { palette.tab_selected_text } else { palette.text };
            draw::text_in_row(&painter, rect, rect.center().x, Edge::Center, name, &font, color);
            if response.clicked() {
                clicked = Some(i);
            }
        }
        if let Some(i) = clicked {
            self.select_tab(ui.ctx(), i);
        }

        // Four pixels from the right keeps the two tools clear of the tab
        // strip even on Summary's 700px preset.
        let mut right = chrome.tabview.right() - 4.0;
        let tool_top = chrome.tabview.top() + 10.0;
        // Every row from the reading tabs in one searchable list. The tabs
        // are laid out for reading a set at a glance, which is the wrong
        // shape when you already know the name of the field you want.
        if tool_button(ui, &palette, "Advanced", &mut right, tool_top).clicked() {
            self.pending.push(Action::Advanced);
        }
        right -= TOOL_BUTTON_GAP;
        // Everything that moves, in its own window: it polls and keeps
        // min/max/average, which does not belong on a tab that reads
        // settings.
        if tool_button(ui, &palette, "Telemetry", &mut right, tool_top).clicked() {
            self.pending.push(Action::Telemetry);
        }
    }

    fn footer(&mut self, ui: &mut Ui, rect: Rect) {
        let palette = self.palette;
        let painter = ui.painter().clone();
        let bold = theme::bold(theme::FONT_SIZE);
        let font = theme::font(theme::FONT_SIZE);
        let mut x = rect.left() + 4.0;
        for (i, (label, url)) in [(YOUTUBE_LABEL, YOUTUBE_URL), (TWITTER_LABEL, TWITTER_URL), (DISCORD_LABEL, DISCORD_URL)].into_iter().enumerate() {
            if i > 0 {
                x += 2.0;
                let sep = draw::text_in_row(&painter, rect, x, Edge::Left, "|", &font, palette.text);
                x = sep.right() + 2.0 + 3.0;
            }
            let width = draw::text_width(&painter, label, &bold);
            let link = Rect::from_min_max(pos2(x, rect.top()), pos2(x + width, rect.bottom()));
            // Colour is the whole affordance here -- there is no button edge
            // to tell you it is clickable, so it lifts a shade under the
            // pointer.
            let response = ui.interact(link, Id::unique(("footer-link", i)), Sense::click()).on_hover_cursor(egui::CursorIcon::PointingHand);
            let color = if response.hovered() { palette.brand_hover } else { palette.brand };
            draw::text_in_row(&painter, link, x, Edge::Left, label, &bold, color);
            if response.clicked() {
                admin::shell_open(url);
            }
            x = link.right() + 3.0;
        }
        // No glyph: the corner is the only handle -- there is no native
        // border to drag -- and the pointer announces it.
        let grip = Rect::from_min_max(pos2(rect.right() - 2.0 - GRIP_SIZE, rect.top()), pos2(rect.right() - 2.0, rect.bottom()));
        let response = ui.interact(grip, Id::unique("resize-grip"), Sense::drag()).on_hover_cursor(egui::CursorIcon::ResizeSouthEast);
        if response.drag_started() {
            ui.ctx().send_viewport_cmd(egui::ViewportCommand::BeginResize(egui::ResizeDirection::SouthEast));
        }
    }

    /// Say why every reading is empty, when that is why.
    fn notice(&self, ui: &mut Ui, rect: Rect) {
        let Some(text) = &self.driver_notice else { return };
        let palette = self.palette;
        let painter = ui.painter();
        painter.rect(rect, CornerRadius::same(6), palette.bg, Stroke::new(1.0, palette.brand), egui::StrokeKind::Inside);
        let job = {
            let mut job = theme::job(text.clone(), theme::bold(theme::FONT_SIZE), palette.brand);
            job.wrap.max_width = rect.width() - 16.0;
            job
        };
        let galley = painter.layout_job(job);
        let y = rect.center().y - galley.size().y / 2.0;
        painter.galley(pos2(rect.left() + 8.0, y.round()), galley, palette.brand);
    }

    /// The module selector at the foot of Summary, Timings and Training.
    fn module_selector(&mut self, ui: &mut Ui, chrome: &Chrome) {
        let tab = self.current_tab().to_string();
        let display = self.choices.display(self.selected);
        let names = self.choices.names();
        if let Some(index) = selector::selector(ui, Id::unique(("module-selector", tab)), chrome.selector(), &self.palette, &display, &names)
            && let Some(channel) = self.choices.select(&names[index])
        {
            self.select_module(channel);
        }
    }

    fn tab_body(&mut self, ui: &mut Ui, chrome: &Chrome) {
        let tab = self.current_tab().to_string();
        let area = chrome.content(&tab);
        if tab == "SPD" {
            spd_tab::show(ui, &mut self.spd, &self.backend.rows, &self.reader.values, &self.palette, self.backend.platform, area);
            return;
        }
        let context = TabContext {
            backend: &self.backend,
            values: &self.reader.values,
            palette: &self.palette,
            selected: self.selected,
            channel_labels: &self.channel_labels,
        };
        if tab == "Summary" {
            summary::show(ui, &context, &self.summary, area);
        } else {
            tabs::show_detail(ui, &context, &tab, area);
        }
        if MODULE_SELECTOR_TABS.contains(&tab.as_str()) {
            self.module_selector(ui, chrome);
        }
    }
}

fn title_glyph(painter: &egui::Painter, rect: Rect, response: &egui::Response, hover: Color32, text: &str, size: f32, color: Color32) {
    if response.hovered() {
        painter.rect_filled(rect, 0.0, hover);
    }
    // A pixel below centre, where Tk's button set the glyph.
    painter.text(rect.center() + vec2(0.0, 1.0), egui::Align2::CENTER_CENTER, text, theme::symbol(size), color);
}

/// One of the two tools on the tab strip, laid right to left from `right`.
fn tool_button(ui: &mut Ui, palette: &Palette, text: &str, right: &mut f32, top: f32) -> egui::Response {
    let font = theme::bold(theme::TAB_FONT_SIZE);
    let width = (draw::text_width(ui.painter(), text, &font) + 18.0).max(TOOL_BUTTON_WIDTH);
    let rect = Rect::from_min_max(pos2(*right - width, top), pos2(*right, top + TOOL_BUTTON_HEIGHT));
    *right = rect.left();
    let response = ui.interact(rect, Id::unique(("tool", text)), Sense::click());
    let fill = if response.hovered() { palette.tab_unselected_hover } else { palette.bg };
    ui.painter().rect(rect, CornerRadius::same(8), fill, Stroke::new(1.0, palette.border), egui::StrokeKind::Inside);
    draw::text_in_row(ui.painter(), rect, rect.center().x, Edge::Center, text, &font, palette.text);
    response
}

impl eframe::App for TimingApp {
    fn ui(&mut self, ui: &mut Ui, _frame: &mut eframe::Frame) {
        let ctx = ui.ctx().clone();
        self.resize_for_current_tab(&ctx);
        let palette = self.palette;
        egui::CentralPanel::default().frame(egui::Frame::new().fill(palette.bg)).show(ui, |ui| {
            // An undecorated window on Windows keeps a one-pixel frame
            // above its client area, which pushes everything a pixel below
            // where the Tk window drew it. Laying out from the window's own
            // top puts it back.
            let (inner, outer) = ui.ctx().input(|i| (i.viewport().inner_rect, i.viewport().outer_rect));
            let frame = match (inner, outer) {
                (Some(inner), Some(outer)) => (inner.top() - outer.top()).clamp(0.0, 2.0),
                _ => 0.0,
            };
            let client = ui.max_rect();
            let window = Rect::from_min_max(pos2(client.left(), client.top() - frame), pos2(client.right(), client.bottom() - frame));
            let chrome = Chrome::new(window, self.driver_notice.is_some());
            self.title_bar(ui, chrome.title_bar);
            if let Some(notice) = chrome.notice {
                self.notice(ui, notice);
            }
            self.tab_strip(ui, &chrome);
            self.tab_body(ui, &chrome);
            self.footer(ui, chrome.footer);
        });

        let actions: Vec<Action> = self.pending.drain(..).collect();
        let current_tab = self.tab_names[self.current].clone();
        if actions.contains(&Action::Advanced) && self.tab_names.iter().any(|t| t == "SPD") {
            // The SPD rows in Advanced fill in from the SPD tab's read; start
            // it if the tab has not been opened yet.
            self.spd.load(&ctx);
        }
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
    use rochviewer::hardware::driver_path::missing_message;

    // --- tests/test_window_fit.py: ChromeTest

    #[test]
    fn sizes_follow_the_platform_and_the_screen() {
        assert_eq!(window_size_for_tab(Platform::Lga1700Ddr4, "Summary", None), (775.0, 775.0));
        assert_eq!(window_size_for_tab(Platform::Am5, "Timings", None), (750.0, 650.0));
        assert_eq!(window_size_for_tab(Platform::Lga1700Ddr5, "Training", None), (975.0, 950.0));
        // An unknown tab takes the platform's Summary size.
        assert_eq!(window_size_for_tab(Platform::Am5, "Jedec", None), (700.0, 750.0));
    }

    #[test]
    fn summary_owns_the_startup_size() {
        for platform in [Platform::Lga1700Ddr4, Platform::Lga1700Ddr5, Platform::Am5, Platform::Lga1851] {
            let sizes = tab_window_sizes(platform);
            let summary = sizes.iter().find(|(t, _)| *t == "Summary").unwrap().1;
            assert_eq!(window_size_for_tab(platform, "Summary", None), summary);
        }
        assert!(UNSCROLLED_TABS.contains(&"Summary"));
        assert!(UNSCROLLED_TABS.contains(&"System Info"));
    }

    #[test]
    fn each_main_tab_has_its_own_size() {
        let base: Vec<&str> = TAB_WINDOW_SIZES.iter().map(|(t, _)| *t).collect();
        assert_eq!(base, vec!["Summary", "System Info", "SPD", "Timings", "Training", "IMC", "RTL", "Voltages"]);
        // AM5 has no IMC or RTL page of its own, so those keep the base size.
        let am5 = tab_window_sizes(Platform::Am5);
        assert_eq!(am5.iter().find(|(t, _)| *t == "IMC").unwrap().1, (750.0, 1100.0));
        assert_eq!(am5.iter().find(|(t, _)| *t == "Training").unwrap().1, (700.0, 650.0));
        let ddr5 = tab_window_sizes(Platform::Lga1700Ddr5);
        assert_eq!(ddr5, LGA1700_DDR5_TAB_WINDOW_SIZES.to_vec());
        assert_eq!(tab_window_sizes(Platform::Lga1700Ddr4), TAB_WINDOW_SIZES.to_vec());
    }

    #[test]
    fn every_size_is_within_the_window_minimum() {
        for sizes in [TAB_WINDOW_SIZES, LGA1700_DDR5_TAB_WINDOW_SIZES, AM5_TAB_WINDOW_SIZES] {
            for (tab, (width, height)) in sizes {
                assert!(*width >= MIN_WINDOW_WIDTH, "{tab}");
                assert!(*height >= MIN_WINDOW_HEIGHT, "{tab}");
            }
        }
    }

    #[test]
    fn ddr5_training_fits_a_1080p_screen_uncapped() {
        let (_, height) = LGA1700_DDR5_TAB_WINDOW_SIZES.iter().find(|(t, _)| *t == "Training").unwrap().1;
        assert!(height <= 1080.0 - 120.0);
        assert_eq!(window_size_for_tab(Platform::Lga1700Ddr5, "Training", Some(1080.0)).1, height);
    }

    #[test]
    fn a_short_screen_caps_each_tab_height() {
        let width = TAB_WINDOW_SIZES.iter().find(|(t, _)| *t == "IMC").unwrap().1.0;
        assert_eq!(window_size_for_tab(Platform::Lga1700Ddr4, "IMC", Some(800.0)), (width, 680.0));
        // IMC is 1100 tall on DDR4; a 1080p screen leaves 960.
        assert_eq!(window_size_for_tab(Platform::Lga1700Ddr4, "IMC", Some(1080.0)), (750.0, 960.0));
        // Never below the minimum, however small the screen.
        assert_eq!(window_size_for_tab(Platform::Am5, "Summary", Some(600.0)).1, MIN_WINDOW_HEIGHT);
    }

    #[test]
    fn the_window_opens_where_the_tallest_tab_fits() {
        // 1920x1080: Training is the tallest LGA1700 DDR5 tab at 950.
        assert_eq!(startup_position(Platform::Lga1700Ddr5, vec2(1920.0, 1080.0)), pos2(610.0, 65.0));
    }

    #[test]
    fn the_title_bar_asks_for_a_size_the_icon_actually_stores() {
        assert_eq!(LOGO_SIZE, 24);
        assert!((LOGO_SIZE as f32) < TITLE_BAR_HEIGHT);
        let widths: Vec<u32> = asset_path::ico_png_entries(asset_path::ICON_ICO).iter().map(|(w, _)| *w).collect();
        assert!(widths.contains(&LOGO_SIZE));
        assert_eq!(asset_path::choose_icon_size(&widths, LOGO_SIZE), Some(LOGO_SIZE));
    }

    #[test]
    fn theme_toggle_icons() {
        assert_eq!(LIGHT_MODE_ICON, "☀");
        assert_eq!(DARK_MODE_IMAGE, "assets/moon-solid-black-18.png");
    }

    #[test]
    fn chrome_lays_out_on_the_tk_grid() {
        // Measured from the 700x750 Python window.
        let chrome = Chrome::new(Rect::from_min_size(pos2(0.0, 0.0), vec2(700.0, 750.0)), false);
        assert_eq!(chrome.footer, Rect::from_min_max(pos2(6.0, 721.0), pos2(694.0, 745.0)));
        assert_eq!(chrome.page.min, pos2(12.0, 78.0));
        assert_eq!(chrome.selector(), Rect::from_min_max(pos2(12.0, 680.0), pos2(688.0, 710.0)));
        assert_eq!(chrome.content("Summary").min, pos2(14.0, 78.0));
        assert_eq!(chrome.content("Timings"), Rect::from_min_max(pos2(14.0, 79.0), pos2(686.0, 676.0)));
        assert_eq!(chrome.content("IMC").max, pos2(686.0, 710.0));
        // The notice pushes the tabs down by its height and padding.
        let with_notice = Chrome::new(Rect::from_min_size(pos2(0.0, 0.0), vec2(700.0, 750.0)), true);
        assert_eq!(with_notice.page.top(), 78.0 + 4.0 + DRIVER_NOTICE_HEIGHT);
    }

    #[test]
    fn spd_sits_beside_system_info() {
        let rows = vec![
            rochviewer::row::Row::new("CPU", "Processor", "System Info", "Left"),
            rochviewer::row::Row::new("tCL", "Primary", "Timings", "Left"),
            rochviewer::row::Row::new("Fan", "Fans", "Sensors", "Left"),
        ];
        assert_eq!(tab_names_for(&rows), vec!["Summary", "System Info", "SPD", "Timings"]);
    }

    // --- tests/test_driver_notice.py

    fn notice(problem: &str) -> Option<String> {
        driver_notice_text(Some(problem), true)
    }

    #[test]
    fn a_working_driver_gets_no_notice() {
        assert_eq!(driver_notice_text(None, true), None);
        assert_eq!(driver_notice_text(Some(""), true), None);
    }

    #[test]
    fn the_real_missing_message_keeps_its_filename() {
        // The bug this guards: the first sentence ends in "inpoutx64.dll", so
        // splitting on "." cut the headline down to "inpoutx64".
        assert!(notice(&missing_message()).unwrap().contains("inpoutx64.dll"));
    }

    #[test]
    fn it_says_the_readings_are_the_thing_affected() {
        // A notice that names a missing DLL without connecting it to the
        // empty rows leaves the two looking unrelated.
        assert!(notice(&missing_message()).unwrap().contains("readings below"));
    }

    #[test]
    fn a_missing_driver_is_told_where_to_put_it() {
        assert!(notice(&missing_message()).unwrap().contains("beside this program"));
    }

    #[test]
    fn a_driver_that_will_not_load_gets_different_advice() {
        // It is already beside the program, so telling them to put it there
        // would be the one instruction that cannot help.
        let text = driver_notice_text(
            Some(
                "Found C:/app/inpoutx64.dll but could not load it. It may be the wrong architecture, or the process \
                 may not be running as administrator (126).",
            ),
            false,
        )
        .unwrap();
        assert!(!text.contains("beside this program"));
        assert!(text.contains("administrator"));
    }

    #[test]
    fn the_directory_list_is_left_out() {
        // missing_message names every directory searched, over several
        // lines. A one-line strip cannot hold them and should not try.
        assert!(!notice(&missing_message()).unwrap().contains('\n'));
    }

    #[test]
    fn it_is_one_line_of_reasonable_length() {
        let text = notice(&missing_message()).unwrap();
        assert!(text.chars().count() < 200, "{text}");
    }

    #[test]
    fn driver_notice_keeps_the_first_sentence() {
        let problem = "inpoutx64.dll not found. It is a third-party component and is not distributed";
        assert_eq!(
            notice(problem).unwrap(),
            "inpoutx64.dll not found — the readings below come from the memory controller and \
             are unavailable without it. Put it beside this program and restart."
        );
    }
}
