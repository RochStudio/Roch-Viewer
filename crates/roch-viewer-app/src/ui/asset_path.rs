//! The icon and the small images the chrome draws.
//!
//! Both are compiled into the executable, so a release build needs nothing
//! beside it but the driver.

use eframe::egui::{ColorImage, IconData};

pub const ICON_ICO: &[u8] = include_bytes!("../../../../icon.ico");
pub const MOON_PNG: &[u8] = include_bytes!("../../../../assets/moon-solid-black-18.png");

/// The 8-byte PNG signature.
pub const PNG_MAGIC: [u8; 8] = [0x89, 0x50, 0x4E, 0x47, 0x0D, 0x0A, 0x1A, 0x0A];

/// The PNG entries of an .ico file by width (0 in the directory means 256).
pub fn ico_png_entries(data: &[u8]) -> Vec<(u32, &[u8])> {
    let mut entries = Vec::new();
    if data.len() < 6 {
        return entries;
    }
    let count = u16::from_le_bytes([data[4], data[5]]) as usize;
    for index in 0..count {
        let at = 6 + index * 16;
        if at + 16 > data.len() {
            break;
        }
        let width = data[at] as u32;
        let length = u32::from_le_bytes(data[at + 8..at + 12].try_into().unwrap()) as usize;
        let offset = u32::from_le_bytes(data[at + 12..at + 16].try_into().unwrap()) as usize;
        let Some(blob) = data.get(offset..offset + length) else { continue };
        if !blob.starts_with(&PNG_MAGIC) {
            continue;
        }
        entries.push((if width == 0 { 256 } else { width }, blob));
    }
    entries
}

/// Which stored icon size to draw at `size` px: the smallest entry at or
/// above it, falling back to the largest below.
pub fn choose_icon_size(widths: &[u32], size: u32) -> Option<u32> {
    widths
        .iter()
        .copied()
        .filter(|w| *w >= size)
        .min()
        .or_else(|| widths.iter().copied().max())
}

fn decode_png(blob: &[u8]) -> Option<image::RgbaImage> {
    image::load_from_memory_with_format(blob, image::ImageFormat::Png)
        .ok()
        .map(|img| img.to_rgba8())
}

/// The app icon at roughly `size` px.
pub fn logo_image(size: u32) -> Option<ColorImage> {
    let entries = ico_png_entries(ICON_ICO);
    let widths: Vec<u32> = entries.iter().map(|(w, _)| *w).collect();
    let chosen = choose_icon_size(&widths, size)?;
    let blob = entries.iter().find(|(w, _)| *w == chosen)?.1;
    let rgba = decode_png(blob)?;
    let size = [rgba.width() as usize, rgba.height() as usize];
    Some(ColorImage::from_rgba_unmultiplied(size, rgba.as_raw()))
}

/// The window and taskbar icon.
pub fn window_icon() -> Option<IconData> {
    let entries = ico_png_entries(ICON_ICO);
    let widths: Vec<u32> = entries.iter().map(|(w, _)| *w).collect();
    let chosen = choose_icon_size(&widths, 64)?;
    let rgba = decode_png(entries.iter().find(|(w, _)| *w == chosen)?.1)?;
    Some(IconData { width: rgba.width(), height: rgba.height(), rgba: rgba.into_raw() })
}

/// The solid black crescent the Light-mode toggle shows.
pub fn moon_image() -> Option<ColorImage> {
    let rgba = decode_png(MOON_PNG)?;
    let size = [rgba.width() as usize, rgba.height() as usize];
    Some(ColorImage::from_rgba_unmultiplied(size, rgba.as_raw()))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ties_break_upward_and_small_sets_fall_back_to_the_largest() {
        assert_eq!(choose_icon_size(&[16, 24, 32], 20), Some(24));
        assert_eq!(choose_icon_size(&[16, 24], 24), Some(24));
        assert_eq!(choose_icon_size(&[16, 24], 48), Some(24));
        assert_eq!(choose_icon_size(&[], 24), None);
    }

    #[test]
    fn the_shipped_icon_has_png_entries_and_decodes() {
        assert!(!ico_png_entries(ICON_ICO).is_empty());
        assert!(logo_image(24).is_some());
        assert!(window_icon().is_some());
        assert!(moon_image().is_some());
    }
}
