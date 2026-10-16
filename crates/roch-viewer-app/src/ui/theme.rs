//! Colours, fonts and sizes shared by every window.
//!
//! Each colour is a (light, dark) pair, as the Python build's were: pure
//! shared surfaces and primary text in both modes, the brand red for what is
//! selected or what a block is, and a separate red for live readings.

use eframe::egui::{self, Color32, FontData, FontDefinitions, FontFamily, FontId};
use std::sync::Arc;

/// Tk measured fonts in points at 96 DPI; egui measures in logical pixels.
pub const fn pt(points: f32) -> f32 {
    points * 96.0 / 72.0
}

pub const FONT_SIZE: f32 = pt(12.0);
pub const TAB_FONT_SIZE: f32 = pt(13.0);
pub const IMC_FONT_SIZE: f32 = pt(11.0);
pub const TITLE_CLOSE_SIZE: f32 = pt(20.0);
pub const TITLE_MINIMIZE_SIZE: f32 = pt(18.0);
pub const TITLE_THEME_SIZE: f32 = pt(18.0);
pub const TITLE_SCREENSHOT_SIZE: f32 = pt(14.0);

pub const ROW_HEIGHT: f32 = 20.0;
pub const SECTION_GAP: f32 = 3.0;

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

fn system_font(name: &str) -> Option<Vec<u8>> {
    let windir = std::env::var_os("WINDIR").unwrap_or_else(|| "C:\\Windows".into());
    std::fs::read(std::path::Path::new(&windir).join("Fonts").join(name)).ok()
}

/// Consolas for everything, as the Python build used it for timing-tool
/// alignment, with Segoe UI Symbol for the title-bar glyphs. When a font is
/// missing the family falls back to egui's own, so the window still draws.
pub fn install_fonts(ctx: &egui::Context) {
    let mut fonts = FontDefinitions::default();
    let fallback_mono = fonts.families.get(&FontFamily::Monospace).cloned().unwrap_or_default();
    let fallback_prop = fonts.families.get(&FontFamily::Proportional).cloned().unwrap_or_default();

    let mut add = |key: &str, file: &str| -> bool {
        match system_font(file) {
            Some(bytes) => {
                fonts.font_data.insert(key.to_string(), Arc::new(FontData::from_owned(bytes)));
                true
            }
            None => false,
        }
    };
    let have_regular = add("Consolas", "consola.ttf");
    let have_bold = add("Consolas Bold", "consolab.ttf");
    let have_symbol = add("Segoe UI Symbol", "seguisym.ttf");
    let have_emoji = add("Segoe UI Emoji", "seguiemj.ttf");

    let mut family = |name: &str, primary: &[(&str, bool)], fallback: &[String]| {
        let mut list: Vec<String> = primary
            .iter()
            .filter(|(_, present)| *present)
            .map(|(font, _)| font.to_string())
            .collect();
        list.extend(fallback.iter().cloned());
        if have_symbol {
            list.push("Segoe UI Symbol".into());
        }
        if have_emoji {
            list.push("Segoe UI Emoji".into());
        }
        fonts.families.insert(FontFamily::Name(name.into()), list);
    };
    family(CONSOLAS, &[("Consolas", have_regular)], &fallback_mono);
    family(CONSOLAS_BOLD, &[("Consolas Bold", have_bold), ("Consolas", have_regular)], &fallback_mono);
    family(SYMBOL, &[("Segoe UI Symbol", have_symbol), ("Segoe UI Emoji", have_emoji)], &fallback_prop);

    // The default families too, so stray widgets match.
    if have_regular {
        for fam in [FontFamily::Monospace, FontFamily::Proportional] {
            fonts.families.entry(fam).or_default().insert(0, "Consolas".into());
        }
    }
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
