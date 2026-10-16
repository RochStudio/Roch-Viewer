//! The window: the main tabs, the searchable Advanced list and the
//! Telemetry pop-out.

pub mod admin;
pub mod app;
pub mod asset_path;
pub mod draw;
pub mod fixture;
pub mod layout;
pub mod popouts;
pub mod reader;
pub mod selector;
pub mod settings;
pub mod spd_tab;
pub mod summary;
pub mod tabs;
pub mod theme;
#[cfg(test)]
mod ui_tests;

use rochviewer::version::{APP_NAME, VERSION};

/// Open the window, elevating first if this process is not elevated.
pub fn run() {
    if !admin::is_admin() {
        eprintln!("Admin privileges required. Relaunching with UAC prompt...");
        admin::run_as_admin();
    }
    admin::claim_taskbar_identity();

    let settings = settings::Settings::load();
    let platform = rochviewer::platform_profiles::active_platform();
    let (width, height) = app::window_size_for_tab(platform, "Summary", None);
    let mut viewport = eframe::egui::ViewportBuilder::default()
        .with_title(format!("{APP_NAME} {VERSION}"))
        .with_decorations(false)
        .with_inner_size([width, height])
        .with_min_inner_size([app::MIN_WINDOW_WIDTH, app::MIN_WINDOW_HEIGHT])
        .with_resizable(true);
    if let Some(icon) = asset_path::window_icon() {
        viewport = viewport.with_icon(icon);
    }
    let options = eframe::NativeOptions {
        viewport,
        centered: true,
        persist_window: false,
        ..Default::default()
    };
    let result = eframe::run_native(
        APP_NAME,
        options,
        Box::new(move |cc| Ok(Box::new(app::TimingApp::new(cc, settings)))),
    );
    if let Err(error) = result {
        eprintln!("Could not open the window: {error}");
    }
}
