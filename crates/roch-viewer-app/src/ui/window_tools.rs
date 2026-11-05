//! Native window chores egui does not cover: where a window is in screen
//! pixels, putting it back there, and how see-through it is.
//!
//! Positions are kept in physical pixels, read from and written to Windows
//! directly, so they come back exactly where they were whatever the scale
//! setting or the monitor's DPI.
//!
//! Linux has none of this. Wayland leaves a window's position and opacity to the
//! compositor and gives a client neither, so they are not offered there. What it
//! needs instead is a way to ask to float: see [`floating`].

use std::sync::atomic::{AtomicU32, Ordering};

#[cfg(windows)]
use windows::Win32::Foundation::{HWND, POINT, RECT};
#[cfg(windows)]
use windows::Win32::Graphics::Gdi::{MONITOR_DEFAULTTONULL, MonitorFromPoint};
#[cfg(windows)]
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

#[cfg_attr(not(windows), allow(dead_code))]
pub fn target_opacity() -> u8 {
    OPACITY.load(Ordering::Relaxed).min(255) as u8
}

/// Whether this platform lets a window know and choose where it is, and how
/// see-through it is. The settings that depend on it are hidden where it does not.
pub const NATIVE_PLACEMENT: bool = cfg!(windows);

/// Whether a corner at `at` would sit on some monitor. Off Windows no position is
/// ever saved, so there is nothing to put back.
#[cfg(not(windows))]
pub fn on_screen(_at: (i32, i32)) -> bool {
    false
}

/// A saved position as points. Never reached off Windows: see [`on_screen`].
#[cfg(not(windows))]
pub fn to_points(at: (i32, i32), _zoom: f32) -> (f32, f32) {
    (at.0 as f32, at.1 as f32)
}

/// How many frames a new window keeps its fixed size. The compositor decides when
/// the window first maps, which is after its first buffer, so one frame is not
/// certainly enough.
#[cfg(not(windows))]
pub const FRAMES_TO_MAP: u32 = 3;

/// A builder that opens at a fixed size, so a tiling compositor floats it.
///
/// Hyprland, and tiling window managers generally, tile every new window, and a
/// tiled window's size is the compositor's. This one sizes itself to the tab it
/// is showing and has its own title bar and drag handle, none of which works
/// tiled, and winit ignores a client's resize request outright. A window whose
/// minimum and maximum size are equal is the conventional way to say it cannot be
/// tiled, and the decision is made when the window is mapped, so the limits can be
/// let go of straight afterwards: see [`release_floating`].
#[cfg(not(windows))]
pub fn floating(builder: eframe::egui::ViewportBuilder) -> eframe::egui::ViewportBuilder {
    let Some(size) = builder.inner_size else { return builder };
    builder.with_min_inner_size(size).with_max_inner_size(size)
}

/// The requests that size this process's first window, its main one, in the order
/// to try them: Hyprland's Lua interface from 0.55, then the older dispatcher.
///
/// A dispatched resize grows a floating window around its centre, which would move
/// the tab strip out from under the pointer on every tab change. So the corner is
/// put back, which is what the window does everywhere else.
#[cfg(not(windows))]
pub fn hyprland_resize_requests(width: f32, height: f32, pid: u32) -> [String; 2] {
    let (width, height) = (width.round() as i32, height.round() as i32);
    [
        format!(
            "/eval local w = hl.get_window('pid:{pid}'); local at = w.at; \
             hl.dispatch(hl.dsp.window.resize({{ x = {width}, y = {height}, relative = false, window = w }})); \
             hl.dispatch(hl.dsp.window.move({{ x = at.x, y = at.y, window = w }}))"
        ),
        format!("/dispatch resizewindowpixel exact {width} {height},pid:{pid}"),
    ]
}

/// Ask Hyprland itself to size the main window, where a client's own request goes
/// nowhere.
///
/// Hyprland reports its floating windows as tiled too, and winit takes a tiled
/// window's size as the compositor's to choose and drops the request, so every tab
/// would keep the first tab's size: on Training, a third column with its values
/// off the edge. Its control socket is how anything else moves a window there.
/// Anywhere else this does nothing, and the ordinary request is what lands.
#[cfg(not(windows))]
pub fn resize_through_compositor(width: f32, height: f32) {
    use std::io::{Read, Write};
    let (Some(runtime), Some(signature)) =
        (std::env::var_os("XDG_RUNTIME_DIR"), std::env::var_os("HYPRLAND_INSTANCE_SIGNATURE"))
    else {
        return;
    };
    let socket = std::path::Path::new(&runtime).join("hypr").join(signature).join(".socket.sock");
    for request in hyprland_resize_requests(width, height, std::process::id()) {
        // One request to a connection: it closes after answering.
        let Ok(mut stream) = std::os::unix::net::UnixStream::connect(&socket) else { return };
        let patience = Some(std::time::Duration::from_millis(100));
        let _ = stream.set_read_timeout(patience);
        let _ = stream.set_write_timeout(patience);
        if stream.write_all(request.as_bytes()).is_err() {
            return;
        }
        let mut reply = [0u8; 16];
        let length = stream.read(&mut reply).unwrap_or(0);
        if reply[..length].starts_with(b"ok") {
            return;
        }
    }
}

/// The limits a window really has, to replace the ones [`floating`] opened it with.
#[cfg(not(windows))]
pub fn release_floating(
    mut builder: eframe::egui::ViewportBuilder,
    min: Option<eframe::egui::Vec2>,
) -> eframe::egui::ViewportBuilder {
    builder.min_inner_size = Some(min.unwrap_or(eframe::egui::Vec2::ZERO));
    // Infinity is how egui says "no limit" to winit.
    builder.max_inner_size = Some(eframe::egui::Vec2::INFINITY);
    builder
}

#[cfg(windows)]
pub fn hwnd(raw: isize) -> HWND {
    HWND(raw as *mut _)
}

/// The top-left corner in screen pixels, unless minimized.
#[cfg(windows)]
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
#[cfg(windows)]
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
#[cfg(windows)]
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
#[cfg(windows)]
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
#[cfg(windows)]
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
#[cfg(windows)]
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

    #[cfg(not(windows))]
    #[test]
    fn the_hyprland_requests_name_the_size_and_this_process() {
        let [lua, classic] = hyprland_resize_requests(1060.0, 839.6, 42);
        assert!(lua.starts_with("/eval "), "{lua}");
        assert!(lua.contains("hl.get_window('pid:42')"), "{lua}");
        assert!(lua.contains("x = 1060, y = 840, relative = false"), "{lua}");
        assert!(lua.contains("x = at.x, y = at.y"), "the corner goes back: {lua}");
        assert_eq!(classic, "/dispatch resizewindowpixel exact 1060 840,pid:42");
    }

    #[test]
    fn opacity_is_stored_as_a_byte() {
        set_target_opacity(0.5);
        assert_eq!(target_opacity(), 128);
        set_target_opacity(2.0);
        assert_eq!(target_opacity(), 255);
    }

    #[cfg(windows)]
    #[test]
    fn far_away_points_are_off_screen() {
        assert!(!on_screen((-100_000, -100_000)));
        assert!(on_screen((0, 0)));
    }
}
