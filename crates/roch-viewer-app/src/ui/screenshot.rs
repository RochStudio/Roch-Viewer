//! Save a picture of the window, the way timings get shared.
//!
//! The Python build captured the window through GDI and encoded the PNG by
//! hand; here egui hands over the rendered frame itself
//! (`ViewportCommand::Screenshot`), which is the window exactly as drawn and
//! needs no capture of the screen. The pixels are kept as top-down BGRA, the
//! order GDI produced, so the PNG and the clipboard copy are built from the
//! same bytes the Python built them from, and everything but the capture
//! itself can be checked without a display.

use std::path::{Path, PathBuf};

use eframe::egui::ColorImage;

/// Whether a screenshot is put on the clipboard as well as saved; a
/// preference, on by default as it always was.
pub static COPY_TO_CLIPBOARD: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(true);

pub const FOLDER_NAME: &str = "Roch Viewer";

/// A wall-clock moment, local time, as the file name spells it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct LocalTime {
    pub year: u16,
    pub month: u16,
    pub day: u16,
    pub hour: u16,
    pub minute: u16,
    pub second: u16,
}

impl LocalTime {
    /// The current local time, as Windows keeps it.
    pub fn now() -> Self {
        // SAFETY: no arguments; returns a SYSTEMTIME by value.
        let t = unsafe { windows::Win32::System::SystemInformation::GetLocalTime() };
        LocalTime { year: t.wYear, month: t.wMonth, day: t.wDay, hour: t.wHour, minute: t.wMinute, second: t.wSecond }
    }
}

/// RochViewer-Summary-20260928-153012.png: the tab, then the moment.
pub fn screenshot_filename(tab_name: &str, when: LocalTime) -> String {
    let tab: String = tab_name.chars().filter(|ch| ch.is_alphanumeric()).collect();
    let tab = if tab.is_empty() { "Window".to_string() } else { tab };
    format!(
        "RochViewer-{tab}-{:04}{:02}{:02}-{:02}{:02}{:02}.png",
        when.year, when.month, when.day, when.hour, when.minute, when.second
    )
}

/// The user's Pictures folder, wherever it has been moved to.
///
/// Asked of the shell rather than built from the profile path: Pictures is
/// often redirected into OneDrive, and ~/Pictures is then an empty folder
/// nobody looks in.
pub fn pictures_folder() -> PathBuf {
    use windows::Win32::System::Com::CoTaskMemFree;
    use windows::Win32::UI::Shell::{FOLDERID_Pictures, KF_FLAG_DEFAULT, SHGetKnownFolderPath};

    // SAFETY: the shell allocates the returned string, and the caller frees it.
    unsafe {
        if let Ok(path) = SHGetKnownFolderPath(&FOLDERID_Pictures, KF_FLAG_DEFAULT, None) {
            let text = path.to_string().unwrap_or_default();
            CoTaskMemFree(Some(path.0 as *const _));
            if !text.is_empty() {
                return PathBuf::from(text);
            }
        }
    }
    home_directory().join("Pictures")
}

fn home_directory() -> PathBuf {
    std::env::var_os("USERPROFILE").map(PathBuf::from).unwrap_or_default()
}

pub fn default_folder() -> PathBuf {
    pictures_folder().join(FOLDER_NAME)
}

/// Encode top-down 32-bit BGRA pixels as an RGB PNG.
pub fn encode_png(width: u32, height: u32, bgra: &[u8]) -> Result<Vec<u8>, String> {
    let expected = width as usize * height as usize * 4;
    if bgra.len() != expected {
        return Err(format!("expected {expected} bytes of pixels, got {}", bgra.len()));
    }
    // BGRA to RGB. The alpha a capture hands back is not meaningful for a
    // window, so it is dropped rather than written as a transparent image.
    let mut rgb = Vec::with_capacity(width as usize * height as usize * 3);
    for [b, g, r, _] in bgra.as_chunks::<4>().0 {
        rgb.extend_from_slice(&[*r, *g, *b]);
    }
    let mut out = Vec::new();
    {
        let mut encoder = png::Encoder::new(&mut out, width, height);
        encoder.set_color(png::ColorType::Rgb);
        encoder.set_depth(png::BitDepth::Eight);
        // Every scanline led by filter type 0, as the hand-written encoder
        // wrote it.
        encoder.set_filter(png::Filter::NoFilter);
        let mut writer = encoder.write_header().map_err(|e| e.to_string())?;
        writer.write_image_data(&rgb).map_err(|e| e.to_string())?;
        writer.finish().map_err(|e| e.to_string())?;
    }
    Ok(out)
}

/// Write the PNG and return its full path.
pub fn save_png(folder: &Path, name: &str, width: u32, height: u32, bgra: &[u8]) -> Result<PathBuf, String> {
    std::fs::create_dir_all(folder).map_err(|e| e.to_string())?;
    let path = folder.join(name);
    let data = encode_png(width, height, bgra)?;
    std::fs::write(&path, data).map_err(|e| e.to_string())?;
    Ok(path)
}

/// The rendered frame as top-down BGRA, cropped to `crop` (x, y, width,
/// height in pixels) where that lies inside it.
pub fn image_bgra(image: &ColorImage, crop: Option<[usize; 4]>) -> (u32, u32, Vec<u8>) {
    let [full_w, full_h] = image.size;
    let [x0, y0, w, h] = crop.unwrap_or([0, 0, full_w, full_h]);
    let x0 = x0.min(full_w);
    let y0 = y0.min(full_h);
    let w = w.min(full_w - x0);
    let h = h.min(full_h - y0);
    let mut bgra = Vec::with_capacity(w * h * 4);
    for y in y0..y0 + h {
        for pixel in &image.pixels[y * full_w + x0..y * full_w + x0 + w] {
            let [r, g, b, _] = pixel.to_srgba_unmultiplied();
            bgra.extend_from_slice(&[b, g, r, 255]);
        }
    }
    (w as u32, h as u32, bgra)
}

pub const CF_DIB: u32 = 8;
const BITMAPINFOHEADER_SIZE: u32 = 40;

fn bitmap_header(width: i32, height: i32) -> Vec<u8> {
    let mut header = Vec::with_capacity(BITMAPINFOHEADER_SIZE as usize);
    header.extend_from_slice(&BITMAPINFOHEADER_SIZE.to_le_bytes()); // biSize
    header.extend_from_slice(&width.to_le_bytes()); // biWidth
    header.extend_from_slice(&height.to_le_bytes()); // biHeight
    header.extend_from_slice(&1u16.to_le_bytes()); // biPlanes
    header.extend_from_slice(&32u16.to_le_bytes()); // biBitCount
    header.extend_from_slice(&0u32.to_le_bytes()); // biCompression: BI_RGB
    header.extend_from_slice(&[0u8; 20]); // biSizeImage .. biClrImportant
    header
}

/// A CF_DIB clipboard payload: header, then the rows bottom to top.
pub fn dib_bytes(width: u32, height: u32, bgra: &[u8]) -> Vec<u8> {
    let mut out = bitmap_header(width as i32, height as i32);
    let stride = width as usize * 4;
    for row in (0..height as usize).rev() {
        out.extend_from_slice(&bgra[row * stride..(row + 1) * stride]);
    }
    out
}

/// Put the image on the clipboard, for pasting straight into a post.
pub fn copy_to_clipboard(width: u32, height: u32, bgra: &[u8]) -> Result<(), String> {
    use windows::Win32::Foundation::{GlobalFree, HANDLE};
    use windows::Win32::System::DataExchange::{CloseClipboard, EmptyClipboard, OpenClipboard, SetClipboardData};
    use windows::Win32::System::Memory::{GMEM_MOVEABLE, GlobalAlloc, GlobalLock, GlobalUnlock};

    let payload = dib_bytes(width, height, bgra);
    // SAFETY: a moveable global block sized for the payload, locked only for
    // the copy; ownership passes to the clipboard once SetClipboardData
    // accepts it and is freed here on every path where it does not.
    unsafe {
        let handle = GlobalAlloc(GMEM_MOVEABLE, payload.len()).map_err(|_| "GlobalAlloc failed".to_string())?;
        let pointer = GlobalLock(handle);
        if pointer.is_null() {
            let _ = GlobalFree(Some(handle));
            return Err("GlobalLock failed".into());
        }
        std::ptr::copy_nonoverlapping(payload.as_ptr(), pointer as *mut u8, payload.len());
        // Reports an "error" when the lock count reaches zero, which is the
        // expected outcome here.
        let _ = GlobalUnlock(handle);
        if OpenClipboard(None).is_err() {
            let _ = GlobalFree(Some(handle));
            return Err("the clipboard is in use".into());
        }
        let result = (|| {
            let _ = EmptyClipboard();
            // Once SetClipboardData succeeds the clipboard owns the memory;
            // it is only freed here when the handover failed.
            if SetClipboardData(CF_DIB, Some(HANDLE(handle.0))).is_err() {
                let _ = GlobalFree(Some(handle));
                return Err("SetClipboardData failed".to_string());
            }
            Ok(())
        })();
        let _ = CloseClipboard();
        result
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The (width, height, RGB rows) of a PNG this module wrote.
    fn decode_png(data: &[u8]) -> (u32, u32, Vec<Vec<u8>>) {
        assert_eq!(&data[..8], b"\x89PNG\r\n\x1a\n");
        let decoder = png::Decoder::new(std::io::Cursor::new(data.to_vec()));
        let mut reader = decoder.read_info().unwrap();
        let info = reader.info();
        assert_eq!((info.bit_depth, info.color_type), (png::BitDepth::Eight, png::ColorType::Rgb));
        let (width, height) = (info.width, info.height);
        let mut buffer = vec![0; reader.output_buffer_size().unwrap()];
        reader.next_frame(&mut buffer).unwrap();
        let stride = width as usize * 3;
        let rows = (0..height as usize).map(|r| buffer[r * stride..(r + 1) * stride].to_vec()).collect();
        (width, height, rows)
    }

    #[test]
    fn pixels_survive_the_round_trip_as_rgb() {
        // Two by two, BGRA: red, green / blue, white.
        let bgra = [0, 0, 255, 255, 0, 255, 0, 255, 255, 0, 0, 255, 255, 255, 255, 255];
        let (width, height, rows) = decode_png(&encode_png(2, 2, &bgra).unwrap());
        assert_eq!((width, height), (2, 2));
        assert_eq!(rows[0], vec![255, 0, 0, 0, 255, 0]);
        assert_eq!(rows[1], vec![0, 0, 255, 255, 255, 255]);
    }

    #[test]
    fn a_short_buffer_is_refused() {
        assert!(encode_png(2, 2, &[0u8; 15]).is_err());
    }

    #[test]
    fn the_clipboard_copy_is_bottom_up_behind_its_header() {
        let (top, bottom) = ([1u8; 4], [2u8; 4]);
        let dib = dib_bytes(1, 2, &[top, bottom].concat());
        let header_size = u32::from_le_bytes(dib[..4].try_into().unwrap()) as usize;
        assert_eq!(header_size, 40);
        // A positive height is what makes a DIB bottom-up.
        assert_eq!(i32::from_le_bytes(dib[8..12].try_into().unwrap()), 2);
        assert_eq!(&dib[header_size..], &[bottom, top].concat()[..]);
    }

    #[test]
    fn the_file_name_carries_the_tab_and_the_time() {
        let when = LocalTime { year: 2026, month: 9, day: 28, hour: 15, minute: 30, second: 12 };
        assert_eq!(screenshot_filename("System Info", when), "RochViewer-SystemInfo-20260928-153012.png");
        assert_eq!(screenshot_filename("", when), "RochViewer-Window-20260928-153012.png");
    }

    #[test]
    fn the_default_folder_is_named_for_the_app() {
        assert_eq!(default_folder().file_name().unwrap(), FOLDER_NAME);
    }

    #[test]
    fn a_frame_is_cropped_and_reordered_to_bgra() {
        let mut image = ColorImage::filled([3, 2], eframe::egui::Color32::from_rgb(1, 2, 3));
        image.pixels[4] = eframe::egui::Color32::from_rgb(10, 20, 30);
        let (w, h, bgra) = image_bgra(&image, Some([1, 1, 5, 5]));
        assert_eq!((w, h), (2, 1));
        assert_eq!(bgra, vec![30, 20, 10, 255, 3, 2, 1, 255]);
        assert_eq!(image_bgra(&image, None).0, 3);
    }
}
