//! Colours, fonts and sizes shared by every window.
//!
//! Each colour is a (light, dark) pair, as the Python build's were: pure
//! shared surfaces and primary text in both modes, the brand red for what is
//! selected or what a block is, and a separate red for live readings.

use eframe::egui::text::{LayoutJob, TextFormat};
use eframe::egui::{self, Color32, FontData, FontDefinitions, FontFamily, FontId};
use std::sync::Arc;

use rochviewer::platform_profiles::Platform;

// CustomTkinter gives font sizes in pixels, not points: ("Consolas", 12) is a
// 12px em. egui's sizes are pixels too (one em), so the numbers carry over.
pub const FONT_SIZE: f32 = 12.0;
pub const TAB_FONT_SIZE: f32 = 13.0;
pub const IMC_FONT_SIZE: f32 = 11.0;
pub const TITLE_CLOSE_SIZE: f32 = 20.0;
pub const TITLE_MINIMIZE_SIZE: f32 = 18.0;
pub const TITLE_THEME_SIZE: f32 = 18.0;
pub const TITLE_SCREENSHOT_SIZE: f32 = 14.0;

pub const ROW_HEIGHT: f32 = 20.0;
/// The gap under a blocked section. Every tab is continuous now, so none
/// draws it; kept as the Python's constant.
#[allow(dead_code)]
pub const SECTION_GAP: f32 = 3.0;

/// Consolas' advance, as a fraction of the em (1126 of 2048 units).
const CONSOLAS_ADVANCE: f32 = 1126.0 / 2048.0;

/// What a Tk window drew each Consolas character as: GDI hints the advance
/// to a whole pixel, so a 12px Consolas steps 7px a character where the
/// outline says 6.6. Every column in the window was measured in those 7px
/// characters, so the text is laid out on them here too.
pub fn gdi_letter_spacing(size: f32) -> f32 {
    let advance = size * CONSOLAS_ADVANCE;
    advance.round() - advance
}

/// 18 on LGA1700 DDR5, whose Training is 43 lines to a column: at 19 it
/// needed more height than a 1080p screen leaves. Consolas at 12 stays clear
/// inside 18; it clipped at IMC's 17.
pub fn training_row_height(platform: Platform) -> f32 {
    if platform == Platform::Lga1700Ddr5 { 18.0 } else { 19.0 }
}

/// IMC carries the controller's full register set elsewhere, so it uses a
/// 17-pixel row pitch and a dedicated 11px face that keeps Consolas clear
/// inside it. On LGA1700 DDR5 the per-module rows moved to Training, and the
/// rest reads in the same face and pitch as every other tab.
pub fn imc_row_height(platform: Platform) -> f32 {
    if platform == Platform::Lga1700Ddr5 { ROW_HEIGHT } else { 17.0 }
}

pub fn imc_font_size(platform: Platform) -> f32 {
    if platform == Platform::Lga1700Ddr5 { FONT_SIZE } else { IMC_FONT_SIZE }
}

/// The compact table-row height used by a detail tab.
pub fn row_height_for_tab(platform: Platform, tab: &str) -> f32 {
    match tab {
        "Training" => training_row_height(platform),
        "IMC" => imc_row_height(platform),
        _ => ROW_HEIGHT,
    }
}

/// The text face a detail tab's rows use.
pub fn font_size_for_tab(platform: Platform, tab: &str) -> f32 {
    if tab == "IMC" { imc_font_size(platform) } else { FONT_SIZE }
}

pub const CONSOLAS: &str = "consolas";
pub const CONSOLAS_BOLD: &str = "consolas-bold";
pub const SYMBOL: &str = "symbol";

fn pair(light: u32, dark: u32, is_dark: bool) -> Color32 {
    let rgb = if is_dark { dark } else { light };
    Color32::from_rgb((rgb >> 16) as u8, (rgb >> 8) as u8, rgb as u8)
}

#[derive(Clone, Copy, Debug)]
pub struct Palette {
    pub dark: bool,
    pub bg: Color32,
    pub border: Color32,
    pub text: Color32,
    pub value: Color32,
    pub tab_selected: Color32,
    pub tab_hover: Color32,
    pub tab_selected_text: Color32,
    pub tab_unselected_hover: Color32,
    pub disabled_text: Color32,
    /// Odd-row band. The same as the background since 1.1.0 aligned the
    /// shared theme; kept as its own role so a tab can still band.
    pub band: Color32,
    #[allow(dead_code)]
    pub hairline: Color32,
    pub brand: Color32,
    pub brand_hover: Color32,
    pub close_hover: Color32,
}

impl Palette {
    pub fn new(dark: bool) -> Self {
        let tab_selected = pair(0xD0343A, 0xD0343A, dark);
        let tab_hover = pair(0xE0383E, 0xE0383E, dark);
        Palette {
            dark,
            bg: pair(0xFFFFFF, 0x000000, dark),
            border: pair(0xD0D0D0, 0x383838, dark),
            text: pair(0x000000, 0xFFFFFF, dark),
            value: pair(0xB91C1C, 0xFF5A5F, dark),
            tab_selected,
            tab_hover,
            tab_selected_text: pair(0xFFFFFF, 0xFFFFFF, dark),
            tab_unselected_hover: pair(0xE6E6E6, 0x222222, dark),
            disabled_text: pair(0x62666D, 0xA0A0A0, dark),
            band: pair(0xFFFFFF, 0x000000, dark),
            hairline: tab_selected,
            brand: tab_selected,
            brand_hover: tab_hover,
            close_hover: Color32::from_rgb(0xC4, 0x2B, 0x1C),
        }
    }
}

pub fn font(size: f32) -> FontId {
    FontId::new(size, FontFamily::Name(CONSOLAS.into()))
}

pub fn bold(size: f32) -> FontId {
    FontId::new(size, FontFamily::Name(CONSOLAS_BOLD.into()))
}

pub fn symbol(size: f32) -> FontId {
    FontId::new(size, FontFamily::Name(SYMBOL.into()))
}

/// One run of text in a Consolas face, spaced the way the Tk window spaced
/// it (see [`gdi_letter_spacing`]).
pub fn job(text: impl Into<String>, font_id: FontId, color: Color32) -> LayoutJob {
    let extra_letter_spacing = match &font_id.family {
        FontFamily::Name(name) if &**name == CONSOLAS || &**name == CONSOLAS_BOLD => gdi_letter_spacing(font_id.size),
        _ => 0.0,
    };
    LayoutJob::single_section(text.into(), TextFormat { font_id, color, extra_letter_spacing, ..Default::default() })
}

fn system_font(name: &str) -> Option<Vec<u8>> {
    let windir = std::env::var_os("WINDIR").unwrap_or_else(|| "C:\\Windows".into());
    std::fs::read(std::path::Path::new(&windir).join("Fonts").join(name)).ok()
}

/// Consolas for everything, as the Python build used it for timing-tool
/// alignment, with Segoe UI Symbol for the title-bar glyphs (the camera,
/// sun, minus and cross are all in it). Every font comes from the Windows
/// fonts folder rather than the executable; when Consolas is missing the
/// family falls back to Courier New, then Arial, so the window still draws.
pub fn install_fonts(ctx: &egui::Context) {
    let mut fonts = FontDefinitions::empty();
    let mut add = |key: &str, files: &[&str]| -> bool {
        match files.iter().find_map(|file| system_font(file)) {
            Some(bytes) => {
                fonts.font_data.insert(key.to_string(), Arc::new(FontData::from_owned(bytes)));
                true
            }
            None => false,
        }
    };
    let have_regular = add("Consolas", &["consola.ttf", "cour.ttf", "arial.ttf"]);
    let have_bold = add("Consolas Bold", &["consolab.ttf", "courbd.ttf", "arialbd.ttf"]);
    let have_symbol = add("Segoe UI Symbol", &["seguisym.ttf", "segoeui.ttf"]);

    let present = |list: &[(&str, bool)]| -> Vec<String> {
        list.iter().filter(|(_, present)| *present).map(|(font, _)| font.to_string()).collect()
    };
    let regular = present(&[("Consolas", have_regular), ("Segoe UI Symbol", have_symbol)]);
    let bold = present(&[("Consolas Bold", have_bold), ("Consolas", have_regular), ("Segoe UI Symbol", have_symbol)]);
    let symbol = present(&[("Segoe UI Symbol", have_symbol), ("Consolas", have_regular)]);
    fonts.families.insert(FontFamily::Name(CONSOLAS.into()), regular.clone());
    fonts.families.insert(FontFamily::Name(CONSOLAS_BOLD.into()), bold);
    fonts.families.insert(FontFamily::Name(SYMBOL.into()), symbol);
    // The default families too, so stray widgets match.
    fonts.families.insert(FontFamily::Monospace, regular.clone());
    fonts.families.insert(FontFamily::Proportional, regular);
    ctx.set_fonts(fonts);
}

/// Apply the palette to egui's own visuals.
pub fn apply(ctx: &egui::Context, palette: &Palette) {
    let mut visuals = if palette.dark { egui::Visuals::dark() } else { egui::Visuals::light() };
    visuals.panel_fill = palette.bg;
    visuals.window_fill = palette.bg;
    visuals.extreme_bg_color = palette.bg;
    visuals.override_text_color = Some(palette.text);
    visuals.selection.bg_fill = palette.tab_selected;
    visuals.selection.stroke.color = palette.tab_selected_text;
    visuals.widgets.noninteractive.bg_stroke.color = palette.border;
    visuals.widgets.hovered.weak_bg_fill = palette.tab_unselected_hover;
    visuals.widgets.inactive.weak_bg_fill = palette.bg;
    visuals.hyperlink_color = palette.brand;
    ctx.set_visuals(visuals);
    ctx.global_style_mut(|style| {
        style.text_styles.insert(egui::TextStyle::Body, font(FONT_SIZE));
        style.text_styles.insert(egui::TextStyle::Button, bold(FONT_SIZE));
        style.text_styles.insert(egui::TextStyle::Monospace, font(FONT_SIZE));
        style.text_styles.insert(egui::TextStyle::Heading, bold(FONT_SIZE));
        style.spacing.item_spacing = egui::vec2(0.0, 0.0);
    });
}
