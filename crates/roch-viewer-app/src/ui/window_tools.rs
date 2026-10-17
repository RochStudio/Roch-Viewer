//! Native window chores egui does not cover: where a window is in screen
//! pixels, putting it back there, and how see-through it is.
//!
//! Positions are kept in physical pixels, read from and written to Windows
//! directly, so they come back exactly where they were whatever the scale
//! setting or the monitor's DPI.

use std::sync::atomic::{AtomicU32, Ordering};

use windows::Win32::Foundation::{HWND, POINT, RECT};
use windows::Win32::Graphics::Gdi::{MONITOR_DEFAULTTONULL, MonitorFromPoint};
use windows::Win32::UI::WindowsAndMessaging::{
    GWL_EXSTYLE, GetLayeredWindowAttributes, GetWindowLongPtrW, GetWindowRect, IsIconic, LWA_ALPHA, SWP_NOACTIVATE, SWP_NOSIZE, SWP_NOZORDER,
    SetLayeredWindowAttributes, SetWindowLongPtrW, SetWindowPos, WS_EX_LAYERED,
};

/// The opacity every window should have, 0..=255. Each window applies it
/// on its own thread's next frame.
static OPACITY: AtomicU32 = AtomicU32::new(255);

pub fn set_target_opacity(fraction: f32) {
    OPACITY.store((fraction.clamp(0.0, 1.0) * 255.0).round() as u32, Ordering::Relaxed);
}

pub fn target_opacity() -> u8 {
    OPACITY.load(Ordering::Relaxed).min(255) as u8
}

pub fn hwnd(raw: isize) -> HWND {
    HWND(raw as *mut _)
}

/// The top-left corner in screen pixels, unless minimized.
pub fn position(window: HWND) -> Option<(i32, i32)> {
    let mut rect = RECT::default();
    // SAFETY: plain queries on a handle of ours; a stale one just fails.
    unsafe {
        if IsIconic(window).as_bool() {
            return None;
        }
        GetWindowRect(window, &mut rect).ok()?;
    }
    Some((rect.left, rect.top))
}

/// Whether a corner at `at` would sit on some monitor -- a saved position on
/// a monitor since unplugged must not open the window off screen.
pub fn on_screen(at: (i32, i32)) -> bool {
    // A point a little inside the corner: the title bar is what must show.
    let probe = POINT { x: at.0 + 40, y: at.1 + 10 };
    // SAFETY: a pure query.
    !unsafe { MonitorFromPoint(probe, MONITOR_DEFAULTTONULL) }.is_invalid()
}

/// A screen-pixel position as the points a new window is placed in.
///
/// egui places a new window at `points * zoom` logical pixels, and Windows
/// turns logical pixels into screen pixels by the monitor's own scale, so
/// the saved corner is divided by both. Creating the window there costs
/// nothing; moving it after it exists cost a tenth of a second.
pub fn to_points(at: (i32, i32), zoom: f32) -> (f32, f32) {
    use windows::Win32::UI::HiDpi::{GetDpiForMonitor, MDT_EFFECTIVE_DPI};
    let probe = POINT { x: at.0 + 40, y: at.1 + 10 };
    // SAFETY: pure queries; a failed DPI read leaves 96, which is 100%.
    let dpi = unsafe {
        let monitor = MonitorFromPoint(probe, MONITOR_DEFAULTTONULL);
        let (mut x, mut y) = (96u32, 96u32);
        if monitor.is_invalid() || GetDpiForMonitor(monitor, MDT_EFFECTIVE_DPI, &mut x, &mut y).is_err() {
            x = 96;
        }
        x.max(1)
    };
    let scale = dpi as f32 / 96.0 * zoom.max(0.1);
    (at.0 as f32 / scale, at.1 as f32 / scale)
}

/// Move a window's top-left corner to `at`, keeping its size and stacking.
pub fn move_to(window: HWND, at: (i32, i32)) -> bool {
    // SAFETY: repositions our own window.
    unsafe { SetWindowPos(window, None, at.0, at.1, 0, 0, SWP_NOSIZE | SWP_NOZORDER | SWP_NOACTIVATE) }.is_ok()
}

/// Keeps the layered style on a window while it is meant to be see-through.
///
/// The windowing library recomputes a window's extended style whenever it
/// resizes or restyles it, which drops the layered bit; the window then
/// draws opaque until something sets it again, and an idle window never
/// does. Answering WM_STYLECHANGING keeps the bit in whatever the library
/// asks for.
unsafe extern "system" fn keep_layered(
    window: HWND,
    message: u32,
    wparam: windows::Win32::Foundation::WPARAM,
    lparam: windows::Win32::Foundation::LPARAM,
    _id: usize,
    _data: usize,
) -> windows::Win32::Foundation::LRESULT {
    use windows::Win32::UI::WindowsAndMessaging::{STYLESTRUCT, WM_STYLECHANGING};
    if message == WM_STYLECHANGING && wparam.0 as i32 == GWL_EXSTYLE.0 && lparam.0 != 0 && target_opacity() < 255 {
        // SAFETY: WM_STYLECHANGING's lparam is the STYLESTRUCT being applied.
        let change = unsafe { &mut *(lparam.0 as *mut STYLESTRUCT) };
        change.styleNew |= WS_EX_LAYERED.0;
    }
    // SAFETY: passes every message on to the window's own procedure.
    unsafe { windows::Win32::UI::Shell::DefSubclassProc(window, message, wparam, lparam) }
}

/// Give a window this opacity (255 is opaque, which also drops the layered
/// style so an opaque window draws exactly as it did).
///
/// Cheap when nothing needs changing, so it can be called every frame.
pub fn apply_opacity(window: HWND, alpha: u8) {
    // SAFETY: style bits, layered attributes and a subclass on our own
    // window, on the thread that owns it.
    unsafe {
        if alpha < 255 {
            // Idempotent: the same procedure and id only update the data.
            let _ = windows::Win32::UI::Shell::SetWindowSubclass(window, Some(keep_layered), 0x524F_4348, 0);
        }
        let style = GetWindowLongPtrW(window, GWL_EXSTYLE);
        let layered = style & WS_EX_LAYERED.0 as isize != 0;
        if alpha == 255 {
            if layered {
                SetWindowLongPtrW(window, GWL_EXSTYLE, style & !(WS_EX_LAYERED.0 as isize));
            }
            return;
        }
        if layered {
            let (mut key, mut current, mut flags) = (windows::Win32::Foundation::COLORREF(0), 0u8, Default::default());
            if GetLayeredWindowAttributes(window, Some(&mut key), Some(&mut current), Some(&mut flags)).is_ok()
                && current == alpha
                && flags == LWA_ALPHA
            {
                return;
            }
        } else {
            SetWindowLongPtrW(window, GWL_EXSTYLE, style | WS_EX_LAYERED.0 as isize);
        }
        let _ = SetLayeredWindowAttributes(window, windows::Win32::Foundation::COLORREF(0), alpha, LWA_ALPHA);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn opacity_is_stored_as_a_byte() {
        set_target_opacity(0.5);
        assert_eq!(target_opacity(), 128);
        set_target_opacity(2.0);
        assert_eq!(target_opacity(), 255);
    }

    #[test]
    fn far_away_points_are_off_screen() {
        assert!(!on_screen((-100_000, -100_000)));
        assert!(on_screen((0, 0)));
    }
}
