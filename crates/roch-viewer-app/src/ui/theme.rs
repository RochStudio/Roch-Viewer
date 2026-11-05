//! Colours, fonts and sizes shared by every window.
//!
//! Each colour is a (light, dark) pair, as the Python build's were: pure
//! shared surfaces and primary text in both modes, the brand red for what is
//! selected or what a block is, and a separate red for live readings.

use eframe::egui::text::{LayoutJob, TextFormat};
use eframe::egui::{self, Color32, FontData, FontDefinitions, FontFamily, FontId};
use std::sync::{Arc, RwLock};

use super::prefs::{self, Prefs};

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

/// The parts of the preferences every drawing call needs, held where a
/// free function can reach them. The defaults are the window as it was.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Look {
    /// Added to every text size.
    pub text_delta: f32,
    /// Added to every table row's height, on top of the text delta.
    pub row_delta: f32,
    /// Lay text out on Consolas' GDI-hinted 7px grid; only right for Consolas.
    pub consolas_spacing: bool,
    /// Headings in the bold face; off draws them in the regular one.
    pub bold_headings: bool,
    pub corner_radius: f32,
}

impl Look {
    pub const DEFAULT: Look =
        Look { text_delta: 0.0, row_delta: 0.0, consolas_spacing: true, bold_headings: true, corner_radius: 8.0 };

    pub fn from_prefs(prefs: &Prefs) -> Look {
        Look {
            text_delta: prefs.text_delta,
            row_delta: prefs.row_delta,
            consolas_spacing: prefs.font_family.eq_ignore_ascii_case(prefs::DEFAULT_FONT),
            bold_headings: prefs.bold_headings,
            corner_radius: prefs.corner_radius,
        }
    }
}

static LOOK: RwLock<Look> = RwLock::new(Look::DEFAULT);

pub fn look() -> Look {
    *LOOK.read().unwrap_or_else(|e| e.into_inner())
}

pub fn set_look(look: Look) {
    *LOOK.write().unwrap_or_else(|e| e.into_inner()) = look;
}

/// A table row's height: its base pitch, grown with the text and the chosen
/// spacing. Never so small a row overlaps the next.
pub fn pitch(base: f32) -> f32 {
    let look = look();
    (base + look.text_delta + look.row_delta).max(10.0)
}

/// How much bigger (or smaller) the text is than the window was laid out for,
/// across and down: the window sizes grow by this.
pub fn size_factor() -> (f32, f32) {
    let look = look();
    ((FONT_SIZE + look.text_delta) / FONT_SIZE, pitch(ROW_HEIGHT) / ROW_HEIGHT)
}

/// A corner radius from the preferences, scaled from the 8px the panels
/// were drawn with: `radius(6)` is a 6px corner at the default.
pub fn radius(at_default: u8) -> egui::CornerRadius {
    let scaled = at_default as f32 * look().corner_radius / 8.0;
    egui::CornerRadius::same(scaled.round().clamp(0.0, 255.0) as u8)
}

/// Consolas' advance, as a fraction of the em (1126 of 2048 units).
const CONSOLAS_ADVANCE: f32 = 1126.0 / 2048.0;

/// The default face's advance as a fraction of the em, as its bits. Consolas,
/// except where it is not installed: see [`install_fonts`].
static DEFAULT_ADVANCE: std::sync::atomic::AtomicU32 = std::sync::atomic::AtomicU32::new(0);

fn default_advance() -> f32 {
    match DEFAULT_ADVANCE.load(std::sync::atomic::Ordering::Relaxed) {
        0 => CONSOLAS_ADVANCE,
        bits => f32::from_bits(bits),
    }
}

/// What a Tk window drew each Consolas character as: GDI hints the advance
/// to a whole pixel, so a 12px Consolas steps 7px a character where the
/// outline says 6.6. Every column in the window was measured in those 7px
/// characters, so the text is laid out on them here too.
///
/// A stand-in face goes on the same grid: the grid is Consolas' whatever is being
/// drawn, and the spacing is whatever it takes this face's advance to reach it,
/// which is a little negative for the wider faces Linux has.
pub fn gdi_letter_spacing(size: f32) -> f32 {
    (size * CONSOLAS_ADVANCE).round() - size * default_advance()
}

/// A character's advance in a TrueType or OpenType font, as a fraction of the em.
///
/// Read from the font's own tables because the window's whole layout depends on
/// it, before any pass has run to measure it with.
pub fn font_advance(font: &[u8], character: char) -> Option<f32> {
    let u16_at = |at: usize| Some(u16::from_be_bytes([*font.get(at)?, *font.get(at + 1)?]));
    let u32_at = |at: usize| Some(u32::from_be_bytes(font.get(at..at + 4)?.try_into().ok()?));
    // A collection starts with a list of the fonts in it; the first will do.
    let start = if font.get(..4)? == b"ttcf" { u32_at(12)? as usize } else { 0 };
    let tables = u16_at(start + 4)? as usize;
    let table = |tag: &[u8; 4]| -> Option<usize> {
        (0..tables).find_map(|index| {
            let record = start + 12 + index * 16;
            (font.get(record..record + 4)? == tag).then(|| u32_at(record + 8))?.map(|offset| offset as usize)
        })
    };
    let units_per_em = u16_at(table(b"head")? + 18)? as f32;
    let metrics_count = u16_at(table(b"hhea")? + 34)? as usize;
    let horizontal = table(b"hmtx")?;
    let cmap = table(b"cmap")?;

    // The Unicode BMP map, format 4: the one every TrueType font carries.
    let code = character as u32;
    let glyph = (0..u16_at(cmap + 2)? as usize).find_map(|index| {
        let subtable = cmap + u32_at(cmap + 4 + index * 8 + 4)? as usize;
        if u16_at(subtable)? != 4 {
            return None;
        }
        let segments = u16_at(subtable + 6)? as usize / 2;
        let (ends, starts) = (subtable + 14, subtable + 16 + segments * 2);
        let (deltas, ranges) = (starts + segments * 2, starts + segments * 4);
        (0..segments).find_map(|segment| {
            let end = u16_at(ends + segment * 2)? as u32;
            let first = u16_at(starts + segment * 2)? as u32;
            if code > end || code < first {
                return None;
            }
            let delta = u16_at(deltas + segment * 2)? as u32;
            let range = u16_at(ranges + segment * 2)? as usize;
            let glyph = if range == 0 {
                code + delta
            } else {
                u16_at(ranges + segment * 2 + range + (code - first) as usize * 2)? as u32 + delta
            };
            Some(glyph & 0xFFFF)
        })
    })? as usize;
    // Past the last entry, every glyph has the last entry's advance.
    let advance = u16_at(horizontal + glyph.min(metrics_count.checked_sub(1)?) * 4)? as f32;
    (units_per_em > 0.0 && advance > 0.0).then_some(advance / units_per_em)
}

/// 18 on LGA1700 DDR5, whose Training is 43 lines to a column: at 19 it
/// needed more height than a 1080p screen leaves. Consolas at 12 stays clear
/// inside 18; it clipped at IMC's 17.
pub fn training_row_height(platform: Platform) -> f32 {
    pitch(if platform == Platform::Lga1700Ddr5 { 18.0 } else { 19.0 })
}

/// IMC carries the controller's full register set elsewhere, so it uses a
/// 17-pixel row pitch and a dedicated 11px face that keeps Consolas clear
/// inside it. On LGA1700 DDR5 the per-module rows moved to Training, and the
/// rest reads in the same face and pitch as every other tab.
pub fn imc_row_height(platform: Platform) -> f32 {
    pitch(if platform == Platform::Lga1700Ddr5 { ROW_HEIGHT } else { 17.0 })
}

pub fn imc_font_size(platform: Platform) -> f32 {
    if platform == Platform::Lga1700Ddr5 { FONT_SIZE } else { IMC_FONT_SIZE }
}

/// The compact table-row height used by a detail tab.
pub fn row_height_for_tab(platform: Platform, tab: &str) -> f32 {
    match tab {
        "Training" => training_row_height(platform),
        "IMC" => imc_row_height(platform),
        _ => pitch(ROW_HEIGHT),
    }
}

/// The text face a detail tab's rows use.
pub fn font_size_for_tab(platform: Platform, tab: &str) -> f32 {
    if tab == "IMC" { imc_font_size(platform) } else { FONT_SIZE }
}

pub const CONSOLAS: &str = "consolas";
pub const CONSOLAS_BOLD: &str = "consolas-bold";
pub const SYMBOL: &str = "symbol";

#[derive(Clone, Copy, Debug, PartialEq)]
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
    /// The window's own colours: the default preferences, which are the
    /// Python build's (light, dark) pairs.
    #[cfg_attr(not(test), allow(dead_code))]
    pub fn new(dark: bool) -> Self {
        Palette::from_prefs(&Prefs::default(), dark)
    }
}

impl Palette {
    /// The colours the preferences ask for, in the mode in use.
    pub fn from_prefs(prefs: &Prefs, dark: bool) -> Self {
        let c = prefs.colors(dark);
        let bg = c.background;
        let contrast = |color| prefs::apply_contrast(color, bg, prefs.contrast);
        Palette {
            dark,
            bg,
            border: contrast(c.border),
            text: contrast(c.text),
            value: contrast(c.value),
            tab_selected: c.accent,
            tab_hover: c.accent_hover,
            // White on the default red, in both modes; whichever of black and
            // white reads on an accent the user picked.
            tab_selected_text: prefs::text_on(c.accent),
            tab_unselected_hover: c.hover,
            disabled_text: contrast(c.dimmed),
            band: if prefs.row_shading { c.shading } else { bg },
            hairline: c.accent,
            brand: contrast(c.accent),
            brand_hover: c.accent_hover,
            close_hover: Color32::from_rgb(0xC4, 0x2B, 0x1C),
        }
    }
}

/// The text face at `size`, grown by the chosen text size.
pub fn font(size: f32) -> FontId {
    FontId::new((size + look().text_delta).max(6.0), FontFamily::Name(CONSOLAS.into()))
}

/// The heading face at `size`: bold unless the preferences say otherwise.
pub fn bold(size: f32) -> FontId {
    let family = if look().bold_headings { CONSOLAS_BOLD } else { CONSOLAS };
    FontId::new((size + look().text_delta).max(6.0), FontFamily::Name(family.into()))
}

pub fn symbol(size: f32) -> FontId {
    FontId::new(size, FontFamily::Name(SYMBOL.into()))
}

/// One run of text in a Consolas face, spaced the way the Tk window spaced
/// it (see [`gdi_letter_spacing`]).
pub fn job(text: impl Into<String>, font_id: FontId, color: Color32) -> LayoutJob {
    let extra_letter_spacing = match &font_id.family {
        FontFamily::Name(name) if look().consolas_spacing && (&**name == CONSOLAS || &**name == CONSOLAS_BOLD) => {
            gdi_letter_spacing(font_id.size)
        }
        _ => 0.0,
    };
    LayoutJob::single_section(text.into(), TextFormat { font_id, color, extra_letter_spacing, ..Default::default() })
}

#[cfg(windows)]
fn system_font(name: &str) -> Option<Vec<u8>> {
    let windir = std::env::var_os("WINDIR").unwrap_or_else(|| "C:\\Windows".into());
    std::fs::read(std::path::Path::new(&windir).join("Fonts").join(name)).ok()
}

/// The faces standing in for the Windows ones: regular, bold and symbols.
struct DefaultFaces {
    regular: Option<Vec<u8>>,
    bold: Option<Vec<u8>>,
    symbol: Option<Vec<u8>>,
}

#[cfg(windows)]
fn default_faces(chosen: bool) -> DefaultFaces {
    let fallback = |files: &[&str]| files.iter().find_map(|file| system_font(file));
    DefaultFaces {
        regular: fallback(&["consola.ttf", "cour.ttf", "arial.ttf"]),
        bold: if chosen { None } else { fallback(&["consolab.ttf", "courbd.ttf", "arialbd.ttf"]) },
        symbol: fallback(&["seguisym.ttf", "segoeui.ttf"]),
    }
}

/// Consolas where someone has installed it, and otherwise what fontconfig calls
/// the system's monospace. Under test, nothing from the machine at all, so the
/// layout tests read the same on every system.
#[cfg(not(windows))]
fn default_faces(chosen: bool) -> DefaultFaces {
    if cfg!(test) {
        return DefaultFaces { regular: None, bold: None, symbol: None };
    }
    let read = |path: Option<std::path::PathBuf>| path.and_then(|path| std::fs::read(path).ok());
    let installed = super::fonts::find_family(prefs::DEFAULT_FONT);
    let regular = match &installed {
        Some(family) => std::fs::read(&family.regular).ok(),
        None => read(super::fonts::matched_face("monospace:regular")),
    };
    let bold = if chosen {
        None
    } else {
        match &installed {
            Some(family) => family.bold.as_ref().and_then(|path| std::fs::read(path).ok()),
            None => read(super::fonts::matched_face("monospace:bold")),
        }
    };
    // U+2699, the gear; U+1F4F7, the camera. Every window also keeps egui's
    // built-in emoji font behind this, which has both.
    DefaultFaces { regular, bold, symbol: read(super::fonts::matched_face(":charset=2699")) }
}

/// The chosen family for everything -- Consolas unless the preferences pick
/// another, as the Python build used it for timing-tool alignment -- with
/// Segoe UI Symbol behind it for the title-bar glyphs (the camera, gear,
/// minus and cross are all in it). Every font comes from the Windows fonts
/// folder rather than the executable; when the family is missing it falls
/// back to Consolas, then Courier New, then Arial, so the window still draws.
///
/// On Linux nothing can be assumed: Consolas is a Windows font, the system's own
/// monospace stands in for it where it is missing, and egui's built-in fonts sit
/// behind everything, since there may be no fontconfig to ask. A stand-in is drawn
/// at the size that gives it Consolas' proportions and spaced onto Consolas' grid,
/// so the columns land where they do on Windows.
pub fn install_fonts(ctx: &egui::Context, family: &str) {
    let mut fonts = FontDefinitions::empty();
    let chosen = if family.eq_ignore_ascii_case(prefs::DEFAULT_FONT) { None } else { super::fonts::find_family(family) };
    let (regular, bold) = match &chosen {
        Some(found) => (std::fs::read(&found.regular).ok(), found.bold.as_ref().and_then(|p| std::fs::read(p).ok())),
        None => (None, None),
    };
    let faces = default_faces(chosen.is_some());
    let regular = regular.or(faces.regular);
    let bold = bold.or(faces.bold);

    // The stand-in's advance, from the regular face, or the built-in one's below.
    let mut advance = regular.as_deref().and_then(|font| font_advance(font, 'M'));
    let built_in_fallbacks = built_in_fonts(&mut fonts);
    if advance.is_none() {
        advance = fonts.font_data.get(BUILT_IN_MONOSPACE).and_then(|data| font_advance(data.font.as_ref().as_ref(), 'M'));
    }
    DEFAULT_ADVANCE.store(advance.filter(|_| chosen.is_none()).unwrap_or(CONSOLAS_ADVANCE).to_bits(), std::sync::atomic::Ordering::Relaxed);
    // Glyphs narrowed to the width Consolas would draw them, for a default face
    // whose advance is not Consolas'. Only the drawing: egui lays out at the
    // advance, which the letter spacing has already taken care of.
    let scale = if chosen.is_none() { advance.map_or(1.0, |advance| (CONSOLAS_ADVANCE / advance).min(1.0)) } else { 1.0 };

    let mut add_bytes = |key: &str, bytes: Option<Vec<u8>>| -> bool {
        match bytes {
            Some(bytes) => {
                let data = FontData::from_owned(bytes).tweak(egui::FontTweak { scale, ..Default::default() });
                fonts.font_data.insert(key.to_string(), Arc::new(data));
                true
            }
            None => false,
        }
    };
    let have_regular = add_bytes("Consolas", regular);
    let have_bold = add_bytes("Consolas Bold", bold);
    let have_symbol = add_bytes("Segoe UI Symbol", faces.symbol);

    let present = |list: &[(&str, bool)]| -> Vec<String> {
        list.iter()
            .filter(|(_, present)| *present)
            .map(|(font, _)| font.to_string())
            .chain(built_in_fallbacks.iter().cloned())
            .collect()
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

/// The built-in monospace, in egui's own font list.
const BUILT_IN_MONOSPACE: &str = "Hack";

/// egui's built-in fonts as a last resort, the fixed-width one first. On Windows
/// the executable does not carry them: the Windows fonts are always there.
#[cfg(not(windows))]
fn built_in_fonts(fonts: &mut FontDefinitions) -> Vec<String> {
    let mut names = Vec::new();
    for (name, data) in FontDefinitions::default().font_data {
        fonts.font_data.insert(name.clone(), data);
        names.push(name);
    }
    names.sort_by_key(|name| name != BUILT_IN_MONOSPACE);
    names
}

#[cfg(windows)]
fn built_in_fonts(_fonts: &mut FontDefinitions) -> Vec<String> {
    Vec::new()
}

/// Whether the desktop prefers dark, for the "follow the system" mode.
///
/// egui is told by the windowing library where it knows. On Wayland it often does
/// not, and the setting most desktops keep in step with their own is GNOME's.
pub fn system_is_dark(ctx: &egui::Context) -> bool {
    match ctx.system_theme() {
        Some(theme) => theme == egui::Theme::Dark,
        None => desktop_prefers_dark(),
    }
}

#[cfg(not(windows))]
fn desktop_prefers_dark() -> bool {
    static ANSWER: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
    *ANSWER.get_or_init(|| {
        let scheme = std::process::Command::new("gsettings")
            .args(["get", "org.gnome.desktop.interface", "color-scheme"])
            .output()
            .ok()
            .filter(|out| out.status.success())
            .map(|out| String::from_utf8_lossy(&out.stdout).to_lowercase());
        match scheme {
            Some(scheme) => scheme.contains("dark"),
            None => std::env::var("GTK_THEME").is_ok_and(|theme| theme.to_lowercase().contains("dark")),
        }
    })
}

#[cfg(windows)]
fn desktop_prefers_dark() -> bool {
    false
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

#[cfg(test)]
mod font_table_tests {
    use super::*;

    /// A minimal font: the four tables `font_advance` reads, with `M` at glyph 2.
    fn tiny_font(units_per_em: u16, advances: &[u16]) -> Vec<u8> {
        let mut head = vec![0u8; 54];
        head[18..20].copy_from_slice(&units_per_em.to_be_bytes());
        let mut hhea = vec![0u8; 36];
        hhea[34..36].copy_from_slice(&(advances.len() as u16).to_be_bytes());
        let hmtx: Vec<u8> = advances.iter().flat_map(|advance| [advance.to_be_bytes(), [0, 0]].concat()).collect();
        // cmap: one format 4 subtable mapping 'M' with a delta, and the end segment.
        let mut cmap = Vec::new();
        cmap.extend_from_slice(&[0, 0, 0, 1, 0, 3, 0, 1, 0, 0, 0, 12]);
        let delta = (2u16).wrapping_sub('M' as u16);
        for word in [4u16, 32, 0, 4, 0, 0, 0, 'M' as u16, 0xFFFF, 0, 'M' as u16, 0xFFFF, delta, 1, 0, 0] {
            cmap.extend_from_slice(&word.to_be_bytes());
        }
        let tables: [(&[u8; 4], Vec<u8>); 4] = [(b"cmap", cmap), (b"head", head), (b"hhea", hhea), (b"hmtx", hmtx)];
        let mut font = vec![0, 1, 0, 0];
        font.extend_from_slice(&(tables.len() as u16).to_be_bytes());
        font.extend_from_slice(&[0; 6]);
        let mut offset = 12 + tables.len() * 16;
        for (tag, data) in &tables {
            font.extend_from_slice(*tag);
            font.extend_from_slice(&[0; 4]);
            font.extend_from_slice(&(offset as u32).to_be_bytes());
            font.extend_from_slice(&(data.len() as u32).to_be_bytes());
            offset += data.len();
        }
        for (_, data) in &tables {
            font.extend_from_slice(data);
        }
        font
    }

    #[test]
    fn the_advance_comes_from_the_fonts_own_tables() {
        // A JetBrains Mono's shape: 600 of 1000.
        let font = tiny_font(1000, &[0, 0, 600]);
        assert_eq!(font_advance(&font, 'M'), Some(0.6));
    }

    #[test]
    fn a_glyph_past_the_metrics_has_the_last_advance() {
        // Monospaced fonts commonly store one advance and let it apply to all.
        let font = tiny_font(2048, &[1126]);
        assert_eq!(font_advance(&font, 'M'), Some(1126.0 / 2048.0));
    }

    #[test]
    fn a_character_the_font_lacks_is_no_answer() {
        let font = tiny_font(1000, &[0, 0, 600]);
        assert_eq!(font_advance(&font, 'Q'), None);
    }

    #[test]
    fn anything_that_is_not_a_font_is_no_answer() {
        assert_eq!(font_advance(b"", 'M'), None);
        assert_eq!(font_advance(b"not a font at all, just some text that runs on", 'M'), None);
        let font = tiny_font(1000, &[0, 0, 600]);
        assert_eq!(font_advance(&font[..40], 'M'), None, "cut off inside the table list");
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn rgb(hex: u32) -> Color32 {
        Color32::from_rgb((hex >> 16) as u8, (hex >> 8) as u8, hex as u8)
    }

    #[test]
    fn the_default_preferences_draw_the_python_colours() {
        // The pairs the window was drawn in before Settings existed.
        for (dark, bg, text, value, border, dimmed, hover) in [
            (false, 0xFFFFFF, 0x000000, 0xB91C1C, 0xD0D0D0, 0x62666D, 0xE6E6E6),
            (true, 0x000000, 0xFFFFFF, 0xFF5A5F, 0x383838, 0xA0A0A0, 0x222222),
        ] {
            let palette = Palette::new(dark);
            assert_eq!(palette.bg, rgb(bg));
            assert_eq!(palette.text, rgb(text));
            assert_eq!(palette.value, rgb(value));
            assert_eq!(palette.border, rgb(border));
            assert_eq!(palette.disabled_text, rgb(dimmed));
            assert_eq!(palette.tab_unselected_hover, rgb(hover));
            assert_eq!(palette.tab_selected, rgb(0xD0343A));
            assert_eq!(palette.tab_hover, rgb(0xE0383E));
            assert_eq!(palette.tab_selected_text, Color32::WHITE);
            // No shading by default: the band is the background.
            assert_eq!(palette.band, palette.bg);
        }
    }

    #[test]
    fn shading_and_contrast_come_from_the_preferences() {
        let prefs = Prefs { row_shading: true, contrast: 1.0, ..Prefs::default() };
        let palette = Palette::from_prefs(&prefs, false);
        assert_eq!(palette.band, prefs.light.shading);
        assert!(prefs::luminance(palette.value) < prefs::luminance(prefs.light.value));
        // A light accent gets dark text on the selected tab.
        let mut light_accent = Prefs::default();
        light_accent.light.accent = Color32::from_rgb(0xF0, 0xF0, 0x80);
        assert_eq!(Palette::from_prefs(&light_accent, false).tab_selected_text, Color32::BLACK);
    }

    #[test]
    fn the_default_look_changes_no_size() {
        assert_eq!(Look::from_prefs(&Prefs::default()), Look::DEFAULT);
        let defaults = Look::DEFAULT;
        assert_eq!(defaults.text_delta + defaults.row_delta, 0.0);
        assert!(defaults.consolas_spacing && defaults.bold_headings);
        assert_eq!(defaults.corner_radius, 8.0);
    }

    #[test]
    fn another_font_drops_the_consolas_grid() {
        let prefs = Prefs { font_family: "Cascadia Mono".into(), text_delta: 2.0, row_delta: 3.0, ..Prefs::default() };
        let look = Look::from_prefs(&prefs);
        assert!(!look.consolas_spacing);
        assert_eq!((look.text_delta, look.row_delta), (2.0, 3.0));
    }
}
