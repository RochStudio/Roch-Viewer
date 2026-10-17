//! Everything the Settings window can change, and how it is saved.
//!
//! The defaults reproduce the window exactly as it was before Settings
//! existed: Consolas at its own size, the Python build's colours, no row
//! shading, no extra spacing, 100% scale. Preferences live under one
//! `preferences` key in the same settings file as the appearance mode, which
//! is also still written so an older build keeps the chosen theme.

use std::collections::BTreeMap;

use eframe::egui::Color32;
use serde_json::{Map, Value, json};

/// Light, Dark, or whatever Windows is set to.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ThemeMode {
    Light,
    Dark,
    System,
}

impl ThemeMode {
    pub const ALL: [ThemeMode; 3] = [ThemeMode::Light, ThemeMode::Dark, ThemeMode::System];

    pub fn name(self) -> &'static str {
        match self {
            ThemeMode::Light => "Light",
            ThemeMode::Dark => "Dark",
            ThemeMode::System => "Follow Windows",
        }
    }

    fn key(self) -> &'static str {
        match self {
            ThemeMode::Light => "light",
            ThemeMode::Dark => "dark",
            ThemeMode::System => "system",
        }
    }

    fn from_key(key: &str) -> Option<Self> {
        match key.to_ascii_lowercase().as_str() {
            "light" => Some(ThemeMode::Light),
            "dark" => Some(ThemeMode::Dark),
            "system" => Some(ThemeMode::System),
            _ => None,
        }
    }

    /// Whether the window is dark, given what Windows itself is set to.
    pub fn is_dark(self, system_dark: bool) -> bool {
        match self {
            ThemeMode::Light => false,
            ThemeMode::Dark => true,
            ThemeMode::System => system_dark,
        }
    }
}

/// The colours one mode is drawn in.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Colors {
    /// Window and panel background.
    pub background: Color32,
    /// Names, headings in the chrome, ordinary text.
    pub text: Color32,
    /// Every reading.
    pub value: Color32,
    /// The selected tab, section headings, links: the brand colour.
    pub accent: Color32,
    /// The accent under the pointer.
    pub accent_hover: Color32,
    /// Panel and button outlines.
    pub border: Color32,
    /// Readings that are not in effect, and secondary text.
    pub dimmed: Color32,
    /// Every other table row, when row shading is on.
    pub shading: Color32,
    /// An unselected tab or button under the pointer.
    pub hover: Color32,
}

/// The editable colour roles, in the order the Settings window lists them.
pub const COLOR_ROLES: &[(&str, &str)] = &[
    ("background", "Background"),
    ("text", "Text"),
    ("value", "Values"),
    ("accent", "Accent"),
    ("accent_hover", "Accent (hover)"),
    ("border", "Borders"),
    ("dimmed", "Dimmed text"),
    ("shading", "Row shading"),
    ("hover", "Hover"),
];

const fn rgb(hex: u32) -> Color32 {
    Color32::from_rgb((hex >> 16) as u8, (hex >> 8) as u8, hex as u8)
}

impl Colors {
    /// The Python build's light theme.
    pub const DEFAULT_LIGHT: Colors = Colors {
        background: rgb(0xFFFFFF),
        text: rgb(0x000000),
        value: rgb(0xB91C1C),
        accent: rgb(0xD0343A),
        accent_hover: rgb(0xE0383E),
        border: rgb(0xD0D0D0),
        dimmed: rgb(0x62666D),
        shading: rgb(0xF0F0F0),
        hover: rgb(0xE6E6E6),
    };

    /// The Python build's dark theme.
    pub const DEFAULT_DARK: Colors = Colors {
        background: rgb(0x000000),
        text: rgb(0xFFFFFF),
        value: rgb(0xFF5A5F),
        accent: rgb(0xD0343A),
        accent_hover: rgb(0xE0383E),
        border: rgb(0x383838),
        dimmed: rgb(0xA0A0A0),
        shading: rgb(0x161616),
        hover: rgb(0x222222),
    };

    pub fn get(&self, role: &str) -> Color32 {
        match role {
            "background" => self.background,
            "text" => self.text,
            "value" => self.value,
            "accent" => self.accent,
            "accent_hover" => self.accent_hover,
            "border" => self.border,
            "dimmed" => self.dimmed,
            "shading" => self.shading,
            _ => self.hover,
        }
    }

    pub fn set(&mut self, role: &str, color: Color32) {
        let slot = match role {
            "background" => &mut self.background,
            "text" => &mut self.text,
            "value" => &mut self.value,
            "accent" => &mut self.accent,
            "accent_hover" => &mut self.accent_hover,
            "border" => &mut self.border,
            "dimmed" => &mut self.dimmed,
            "shading" => &mut self.shading,
            _ => &mut self.hover,
        };
        *slot = color;
    }

    fn to_json(self) -> Value {
        let mut map = Map::new();
        for (role, _) in COLOR_ROLES {
            map.insert(role.to_string(), Value::from(hex(self.get(role))));
        }
        Value::Object(map)
    }

    fn from_json(value: Option<&Value>, fallback: Colors) -> Colors {
        let mut colors = fallback;
        if let Some(Value::Object(map)) = value {
            for (role, _) in COLOR_ROLES {
                if let Some(color) = map.get(*role).and_then(Value::as_str).and_then(parse_hex) {
                    colors.set(role, color);
                }
            }
        }
        colors
    }
}

/// `#RRGGBB`.
pub fn hex(color: Color32) -> String {
    format!("#{:02X}{:02X}{:02X}", color.r(), color.g(), color.b())
}

/// `#RRGGBB` or `RRGGBB`, any case.
pub fn parse_hex(text: &str) -> Option<Color32> {
    let digits = text.trim().trim_start_matches('#');
    if digits.len() != 6 || !digits.chars().all(|c| c.is_ascii_hexdigit()) {
        return None;
    }
    u32::from_str_radix(digits, 16).ok().map(rgb)
}

/// A named pair of colour sets, one per mode.
pub struct Preset {
    pub name: &'static str,
    pub light: Colors,
    pub dark: Colors,
}

/// Starting points for the colours; every role can be changed after.
pub fn presets() -> Vec<Preset> {
    let tint = |base: Colors, accent: u32, hover: u32, value: u32| Colors {
        accent: rgb(accent),
        accent_hover: rgb(hover),
        value: rgb(value),
        ..base
    };
    vec![
        Preset { name: "Roch (default)", light: Colors::DEFAULT_LIGHT, dark: Colors::DEFAULT_DARK },
        Preset {
            name: "High contrast",
            light: Colors {
                background: rgb(0xFFFFFF),
                text: rgb(0x000000),
                value: rgb(0x8B0000),
                accent: rgb(0x0000C0),
                accent_hover: rgb(0x0000FF),
                border: rgb(0x000000),
                dimmed: rgb(0x404040),
                shading: rgb(0xE8E8E8),
                hover: rgb(0xD0D0D0),
            },
            dark: Colors {
                background: rgb(0x000000),
                text: rgb(0xFFFFFF),
                value: rgb(0xFFFF00),
                accent: rgb(0x00FFFF),
                accent_hover: rgb(0x80FFFF),
                border: rgb(0xFFFFFF),
                dimmed: rgb(0xC0C0C0),
                shading: rgb(0x202020),
                hover: rgb(0x303030),
            },
        },
        Preset {
            name: "Graphite",
            light: Colors {
                background: rgb(0xF3F3F3),
                text: rgb(0x1E1E1E),
                value: rgb(0x9E2A2B),
                accent: rgb(0x4A4A4A),
                accent_hover: rgb(0x5E5E5E),
                border: rgb(0xC8C8C8),
                dimmed: rgb(0x6E6E6E),
                shading: rgb(0xE9E9E9),
                hover: rgb(0xDDDDDD),
            },
            dark: Colors {
                background: rgb(0x1E1E1E),
                text: rgb(0xE6E6E6),
                value: rgb(0xFF7B72),
                accent: rgb(0x5A5A5A),
                accent_hover: rgb(0x6E6E6E),
                border: rgb(0x3C3C3C),
                dimmed: rgb(0x9A9A9A),
                shading: rgb(0x262626),
                hover: rgb(0x2E2E2E),
            },
        },
        Preset {
            name: "Ocean",
            light: tint(Colors::DEFAULT_LIGHT, 0x1F6FEB, 0x388BFD, 0x0B5CAD),
            dark: Colors { background: rgb(0x0D1117), shading: rgb(0x161B22), hover: rgb(0x21262D), border: rgb(0x30363D), ..tint(Colors::DEFAULT_DARK, 0x1F6FEB, 0x388BFD, 0x58A6FF) },
        },
        Preset {
            name: "Forest",
            light: tint(Colors::DEFAULT_LIGHT, 0x2E7D32, 0x388E3C, 0x1B5E20),
            dark: Colors { background: rgb(0x0B140D), shading: rgb(0x122016), hover: rgb(0x1B2C1F), border: rgb(0x2A3D2E), ..tint(Colors::DEFAULT_DARK, 0x2E7D32, 0x43A047, 0x7EE787) },
        },
        Preset {
            name: "Amber",
            light: tint(Colors::DEFAULT_LIGHT, 0xB45309, 0xD97706, 0x92400E),
            dark: Colors { background: rgb(0x120D05), shading: rgb(0x1C140A), hover: rgb(0x2A1E0E), border: rgb(0x3A2A14), ..tint(Colors::DEFAULT_DARK, 0xB45309, 0xD97706, 0xFFB000) },
        },
        Preset {
            name: "Purple",
            light: tint(Colors::DEFAULT_LIGHT, 0x7C3AED, 0x8B5CF6, 0x6D28D9),
            dark: Colors { background: rgb(0x0F0B16), shading: rgb(0x17111F), hover: rgb(0x221A2E), border: rgb(0x32284A), ..tint(Colors::DEFAULT_DARK, 0x7C3AED, 0x8B5CF6, 0xC4B5FD) },
        },
    ]
}

/// Where the windows were last, in screen pixels.
pub const WINDOW_KEYS: &[&str] = &["main", "advanced", "telemetry", "settings"];

#[derive(Clone, Debug, PartialEq)]
pub struct Prefs {
    pub theme: ThemeMode,
    pub light: Colors,
    pub dark: Colors,
    /// -1 (softer) to +1 (harder): pushes text and readings away from the
    /// background, or toward it.
    pub contrast: f32,
    pub row_shading: bool,

    pub font_family: String,
    /// Added to every text size, in pixels.
    pub text_delta: f32,
    /// Added to every table row's height, in pixels.
    pub row_delta: f32,
    pub bold_headings: bool,

    /// Everything, chrome included: 1.0 is 100%.
    pub ui_scale: f32,
    pub corner_radius: f32,

    pub remember_positions: bool,
    pub positions: BTreeMap<String, (i32, i32)>,
    pub remember_tab: bool,
    pub last_tab: String,
    pub always_on_top: bool,
    /// 0.3 to 1.0.
    pub opacity: f32,

    /// How often live readings are taken again, in milliseconds.
    pub refresh_ms: u32,
    pub show_footer_links: bool,
    pub screenshot_to_clipboard: bool,
}

pub const DEFAULT_FONT: &str = "Consolas";

impl Default for Prefs {
    fn default() -> Self {
        Prefs {
            theme: ThemeMode::Light,
            light: Colors::DEFAULT_LIGHT,
            dark: Colors::DEFAULT_DARK,
            contrast: 0.0,
            row_shading: false,
            font_family: DEFAULT_FONT.into(),
            text_delta: 0.0,
            row_delta: 0.0,
            bold_headings: true,
            ui_scale: 1.0,
            corner_radius: 8.0,
            remember_positions: true,
            positions: BTreeMap::new(),
            remember_tab: false,
            last_tab: "Summary".into(),
            always_on_top: false,
            opacity: 1.0,
            refresh_ms: 1000,
            show_footer_links: true,
            screenshot_to_clipboard: true,
        }
    }
}

pub const TEXT_DELTA_RANGE: (f32, f32) = (-3.0, 8.0);
pub const ROW_DELTA_RANGE: (f32, f32) = (-3.0, 10.0);
pub const SCALE_RANGE: (f32, f32) = (0.75, 2.0);
pub const RADIUS_RANGE: (f32, f32) = (0.0, 14.0);
pub const OPACITY_RANGE: (f32, f32) = (0.3, 1.0);
pub const REFRESH_RANGE: (u32, u32) = (250, 5000);

fn number(map: &Map<String, Value>, key: &str, default: f32, range: (f32, f32)) -> f32 {
    map.get(key)
        .and_then(Value::as_f64)
        .map(|v| v as f32)
        .filter(|v| v.is_finite())
        .map_or(default, |v| v.clamp(range.0, range.1))
}

fn flag(map: &Map<String, Value>, key: &str, default: bool) -> bool {
    map.get(key).and_then(Value::as_bool).unwrap_or(default)
}

impl Prefs {
    /// What a settings file holds: the `preferences` object, and failing
    /// that the old `appearance_mode`, so an upgrade keeps the theme.
    pub fn from_settings(settings: &Map<String, Value>) -> Prefs {
        let defaults = Prefs::default();
        let legacy_theme = settings
            .get("appearance_mode")
            .and_then(Value::as_str)
            .and_then(ThemeMode::from_key)
            .unwrap_or(defaults.theme);
        let Some(Value::Object(map)) = settings.get("preferences") else {
            return Prefs { theme: legacy_theme, ..defaults };
        };
        let mut positions = BTreeMap::new();
        if let Some(Value::Object(saved)) = map.get("positions") {
            for key in WINDOW_KEYS {
                if let Some(Value::Array(xy)) = saved.get(*key)
                    && let (Some(x), Some(y)) = (xy.first().and_then(Value::as_i64), xy.get(1).and_then(Value::as_i64))
                {
                    positions.insert(key.to_string(), (x as i32, y as i32));
                }
            }
        }
        Prefs {
            theme: map.get("theme").and_then(Value::as_str).and_then(ThemeMode::from_key).unwrap_or(legacy_theme),
            light: Colors::from_json(map.get("light"), defaults.light),
            dark: Colors::from_json(map.get("dark"), defaults.dark),
            contrast: number(map, "contrast", defaults.contrast, (-1.0, 1.0)),
            row_shading: flag(map, "row_shading", defaults.row_shading),
            font_family: map
                .get("font_family")
                .and_then(Value::as_str)
                .filter(|s| !s.trim().is_empty())
                .unwrap_or(DEFAULT_FONT)
                .to_string(),
            text_delta: number(map, "text_delta", defaults.text_delta, TEXT_DELTA_RANGE),
            row_delta: number(map, "row_delta", defaults.row_delta, ROW_DELTA_RANGE),
            bold_headings: flag(map, "bold_headings", defaults.bold_headings),
            ui_scale: number(map, "ui_scale", defaults.ui_scale, SCALE_RANGE),
            corner_radius: number(map, "corner_radius", defaults.corner_radius, RADIUS_RANGE),
            remember_positions: flag(map, "remember_positions", defaults.remember_positions),
            positions,
            remember_tab: flag(map, "remember_tab", defaults.remember_tab),
            last_tab: map.get("last_tab").and_then(Value::as_str).unwrap_or("Summary").to_string(),
            always_on_top: flag(map, "always_on_top", defaults.always_on_top),
            opacity: number(map, "opacity", defaults.opacity, OPACITY_RANGE),
            refresh_ms: map
                .get("refresh_ms")
                .and_then(Value::as_u64)
                .map_or(defaults.refresh_ms, |v| (v as u32).clamp(REFRESH_RANGE.0, REFRESH_RANGE.1)),
            show_footer_links: flag(map, "show_footer_links", defaults.show_footer_links),
            screenshot_to_clipboard: flag(map, "screenshot_to_clipboard", defaults.screenshot_to_clipboard),
        }
    }

    pub fn to_json(&self) -> Value {
        let positions: Map<String, Value> =
            self.positions.iter().map(|(k, (x, y))| (k.clone(), json!([x, y]))).collect();
        json!({
            "theme": self.theme.key(),
            "light": self.light.to_json(),
            "dark": self.dark.to_json(),
            "contrast": self.contrast,
            "row_shading": self.row_shading,
            "font_family": self.font_family,
            "text_delta": self.text_delta,
            "row_delta": self.row_delta,
            "bold_headings": self.bold_headings,
            "ui_scale": self.ui_scale,
            "corner_radius": self.corner_radius,
            "remember_positions": self.remember_positions,
            "positions": positions,
            "remember_tab": self.remember_tab,
            "last_tab": self.last_tab,
            "always_on_top": self.always_on_top,
            "opacity": self.opacity,
            "refresh_ms": self.refresh_ms,
            "show_footer_links": self.show_footer_links,
            "screenshot_to_clipboard": self.screenshot_to_clipboard,
        })
    }

    /// The old key, kept current for builds that only know it.
    pub fn legacy_appearance_mode(&self, system_dark: bool) -> &'static str {
        if self.theme.is_dark(system_dark) { "Dark" } else { "Light" }
    }

    /// The colours for the mode in use.
    pub fn colors(&self, dark: bool) -> &Colors {
        if dark { &self.dark } else { &self.light }
    }

    pub fn colors_mut(&mut self, dark: bool) -> &mut Colors {
        if dark { &mut self.dark } else { &mut self.light }
    }
}

/// Blend two colours: `t` of the way from `a` to `b`.
pub fn mix(a: Color32, b: Color32, t: f32) -> Color32 {
    let t = t.clamp(0.0, 1.0);
    let lerp = |x: u8, y: u8| (x as f32 + (y as f32 - x as f32) * t).round() as u8;
    Color32::from_rgb(lerp(a.r(), b.r()), lerp(a.g(), b.g()), lerp(a.b(), b.b()))
}

/// Relative luminance, 0 (black) to 1 (white).
pub fn luminance(color: Color32) -> f32 {
    let channel = |c: u8| {
        let c = c as f32 / 255.0;
        if c <= 0.03928 { c / 12.92 } else { ((c + 0.055) / 1.055).powf(2.4) }
    };
    0.2126 * channel(color.r()) + 0.7152 * channel(color.g()) + 0.0722 * channel(color.b())
}

/// Move a foreground colour away from the background (`amount` > 0) toward
/// the extreme that contrasts with it most, or toward the background
/// (`amount` < 0).
pub fn apply_contrast(foreground: Color32, background: Color32, amount: f32) -> Color32 {
    if amount > 0.0 {
        let target = if luminance(background) > 0.5 { Color32::BLACK } else { Color32::WHITE };
        mix(foreground, target, amount * 0.85)
    } else if amount < 0.0 {
        mix(foreground, background, -amount * 0.6)
    } else {
        foreground
    }
}

/// Black or white, whichever reads better on `background`.
pub fn text_on(background: Color32) -> Color32 {
    if luminance(background) > 0.45 { Color32::BLACK } else { Color32::WHITE }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn defaults_are_the_python_look() {
        let prefs = Prefs::default();
        assert_eq!(prefs.light.background, Color32::WHITE);
        assert_eq!(prefs.dark.value, rgb(0xFF5A5F));
        assert_eq!(prefs.font_family, "Consolas");
        assert_eq!((prefs.text_delta, prefs.row_delta, prefs.ui_scale), (0.0, 0.0, 1.0));
        assert!(!prefs.row_shading);
    }

    #[test]
    fn the_old_appearance_key_still_picks_the_theme() {
        let mut settings = Map::new();
        settings.insert("appearance_mode".into(), Value::from("Dark"));
        assert_eq!(Prefs::from_settings(&settings).theme, ThemeMode::Dark);
    }

    #[test]
    fn round_trip_keeps_everything() {
        let mut prefs = Prefs {
            theme: ThemeMode::System,
            contrast: 0.5,
            row_shading: true,
            font_family: "Cascadia Mono".into(),
            text_delta: 2.0,
            row_delta: 3.0,
            ui_scale: 1.25,
            remember_tab: true,
            last_tab: "Timings".into(),
            always_on_top: true,
            opacity: 0.8,
            refresh_ms: 500,
            ..Prefs::default()
        };
        prefs.dark.accent = rgb(0x123456);
        prefs.positions.insert("main".into(), (-1200, 40));
        let mut settings = Map::new();
        settings.insert("preferences".into(), prefs.to_json());
        assert_eq!(Prefs::from_settings(&settings), prefs);
    }

    #[test]
    fn bad_values_are_clamped_or_ignored() {
        let mut settings = Map::new();
        settings.insert(
            "preferences".into(),
            json!({"ui_scale": 9.0, "text_delta": "big", "light": {"text": "#zzzzzz", "value": "00ff00"}, "refresh_ms": 1}),
        );
        let prefs = Prefs::from_settings(&settings);
        assert_eq!(prefs.ui_scale, SCALE_RANGE.1);
        assert_eq!(prefs.text_delta, 0.0);
        assert_eq!(prefs.light.text, Color32::BLACK);
        assert_eq!(prefs.light.value, rgb(0x00FF00));
        assert_eq!(prefs.refresh_ms, REFRESH_RANGE.0);
    }

    #[test]
    fn hex_round_trip() {
        assert_eq!(hex(rgb(0xD0343A)), "#D0343A");
        assert_eq!(parse_hex("#d0343a"), Some(rgb(0xD0343A)));
        assert_eq!(parse_hex("12345"), None);
    }

    #[test]
    fn contrast_moves_toward_and_away_from_the_background() {
        let gray = rgb(0x808080);
        assert!(luminance(apply_contrast(gray, Color32::WHITE, 1.0)) < luminance(gray));
        assert!(luminance(apply_contrast(gray, Color32::BLACK, 1.0)) > luminance(gray));
        assert!(luminance(apply_contrast(Color32::BLACK, Color32::WHITE, -1.0)) > 0.0);
        assert_eq!(apply_contrast(gray, Color32::WHITE, 0.0), gray);
    }

}
