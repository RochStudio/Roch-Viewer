//! The windows the main window opens, and the screenshot.
//!
//! Advanced (every row in one searchable list), Telemetry (everything that
//! moves, polled with min/max/average) and the screenshot button all hang
//! off the main window through this one type, so the main window only says
//! *when*: a button was pressed, or a frame is being drawn.
//!
//! Placeholder: the Advanced and Telemetry windows and the screenshot are
//! not ported yet. The interface below is what `app.rs` calls.

use std::path::PathBuf;
use std::sync::Arc;

use eframe::egui;

use rochviewer::timings::Backend;

use super::reader::Values;
use super::settings::Settings;
use super::theme::Palette;

/// What the main window shares with the windows it opens.
pub struct Shared<'a> {
    pub backend: &'a Arc<Backend>,
    pub values: &'a Values,
    pub palette: &'a Palette,
    pub settings: &'a mut Settings,
    pub tab_names: &'a [String],
    pub current_tab: &'a str,
}

#[derive(Default)]
pub struct Popouts {
    pub last_screenshot: Option<PathBuf>,
    status: Option<(String, std::time::Instant)>,
}

impl Popouts {
    pub fn new() -> Self {
        Self::default()
    }

    /// Open the searchable all-rows window, or focus the open one.
    pub fn open_advanced(&mut self, _ctx: &egui::Context, _shared: &mut Shared) {}

    /// Open the telemetry window, or focus the open one.
    pub fn open_telemetry(&mut self, _ctx: &egui::Context, _shared: &mut Shared) {}

    /// Called once at startup: reopen Telemetry if the user asked for that.
    pub fn startup(&mut self, ctx: &egui::Context, shared: &mut Shared) {
        if shared.settings.get_bool("telemetry_auto_open") {
            self.open_telemetry(ctx, shared);
        }
    }

    /// Save a PNG of the main window and copy it to the clipboard.
    pub fn take_screenshot(&mut self, _ctx: &egui::Context, _shared: &mut Shared) {}

    /// Draw the open windows and handle anything that arrived this frame
    /// (a finished screenshot, a closed window).
    pub fn show(&mut self, _ctx: &egui::Context, _shared: &mut Shared) {}

    /// The few-seconds status beside the screenshot button, if any.
    pub fn screenshot_status(&self) -> Option<&str> {
        self.status
            .as_ref()
            .filter(|(_, at)| at.elapsed() < std::time::Duration::from_millis(4000))
            .map(|(text, _)| text.as_str())
    }

    /// Show the saved picture in Explorer, selected.
    pub fn open_screenshot_folder(&self) {
        if let Some(path) = self.last_screenshot.as_ref().filter(|p| p.exists()) {
            super::admin::reveal_in_explorer(path);
        }
    }
}
