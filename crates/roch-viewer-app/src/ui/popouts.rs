//! The windows the main window opens, and the screenshot.
//!
//! Advanced (every row in one searchable list), Telemetry (everything that
//! moves, polled with min/max/average) and the screenshot button all hang
//! off the main window through this one type, so the main window only says
//! *when*: a button was pressed, or a frame is being drawn.
//!
//! Each pop-out is its own OS window (an egui deferred viewport), placed
//! beside the main window, wearing the app icon and the app's own colours.
//! The feeders that turn the platform's rows into what each window lists --
//! `advanced_entries`, `sensor_groups` and the rest, which lived on the
//! Python main window -- live here too.

use std::collections::{BTreeMap, HashMap, HashSet};
use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use eframe::egui::{self, Color32, FontId, Rect, ViewportBuilder, ViewportCommand, ViewportId};

use rochviewer::display_values::{self, EM_DASH, WINDOWED_TABS};
use rochviewer::memory::dimm_inventory::{DimmModule, slots_by_channel};
use rochviewer::memory::spd_profiles::{SpdRecord, SpdValue};
use rochviewer::platform_profiles::Platform;
use rochviewer::row::{Channel, Row};
use rochviewer::timings::Backend;

use super::advanced_window::{self, AdvancedWindow, Entry, Reading};
use super::asset_path;
use super::dimm_telemetry_window::{self, DimmTelemetryWindow, SensorGroup, SensorReading, StatKey, TelemetryModule};
use super::reader::Values;
use super::prefs::Prefs;
use super::screenshot;
use super::settings::Settings;
use super::settings_window::{self, SettingsWindow};
use super::theme::{self, Palette};
use super::window_tools;

/// What the main window shares with the windows it opens.
pub struct Shared<'a> {
    pub backend: &'a Arc<Backend>,
    /// The main window's row values; the pop-outs read their own.
    #[allow(dead_code)]
    pub values: &'a Values,
    pub palette: &'a Palette,
    pub settings: &'a mut Settings,
    pub tab_names: &'a [String],
    pub current_tab: &'a str,
    /// The SPD read the SPD tab owns, which Advanced lists too.
    pub spd: &'a SpdSource,
    /// The slot each channel holds, once the main window has read them.
    pub channel_labels: &'a BTreeMap<String, String>,
    /// The preferences the Settings window edits, and a counter it bumps on
    /// each change.
    pub prefs: &'a Arc<Mutex<Prefs>>,
    pub prefs_revision: &'a Arc<AtomicU64>,
}

impl Shared<'_> {
    fn prefs(&self) -> Prefs {
        self.prefs.lock().unwrap_or_else(|e| e.into_inner()).clone()
    }

    /// Where a window was last, when the preferences say to remember it and
    /// it would still be on a monitor.
    fn saved_position(&self, key: &str) -> Option<(i32, i32)> {
        let prefs = self.prefs();
        prefs.remember_positions.then(|| prefs.positions.get(key).copied()).flatten().filter(|&at| window_tools::on_screen(at))
    }

    fn on_top(&self) -> bool {
        self.prefs().always_on_top
    }
}

/// What the Summary editor offers: every row once (by tab and name), and
/// the built-in Summary to start from.
fn summary_context(backend: &Arc<Backend>) -> settings_window::SummaryContext {
    use super::summary_layout::SummaryLayout;
    let rows = &backend.rows;
    let mut seen = HashSet::new();
    let catalog = rows
        .iter()
        .filter(|row| !row.extra_bool("diagnostic") && seen.insert((row.tab.clone(), row.name.clone())))
        .map(|row| settings_window::CatalogRow {
            name: row.name.clone(),
            tab: row.tab.clone(),
            category: row.category.clone(),
            label: row.label().to_string(),
        })
        .collect();
    let built_in = SummaryLayout::from_plan(rows, &super::summary::summary_plan(rows, backend.platform));
    settings_window::SummaryContext { platform: backend.platform.id().to_string(), catalog, built_in }
}

/// A pop-out builder with the preferences' window level.
fn with_level(builder: ViewportBuilder, on_top: bool) -> ViewportBuilder {
    if on_top { builder.with_window_level(egui::WindowLevel::AlwaysOnTop) } else { builder }
}

/// How long the screenshot status stays beside the button.
pub const SCREENSHOT_STATUS_MS: u64 = 4000;

/// The pop-outs' text size, in pixels.
///
/// The Python pop-outs were customtkinter labels given ("Consolas", 12),
/// which customtkinter draws at 12 pixels rather than 12 points; GDI then
/// hinted Consolas to a whole 7px advance. 12.75px is the size whose
/// unhinted advance is that same 7px, so every column the Python measured
/// against its text lands where it did.
pub const POPOUT_FONT_SIZE: f32 = 12.75;

/// The fonts and scroll bar shared by both pop-outs.
pub struct PopoutStyle {
    pub font: FontId,
    pub bold: FontId,
    handle: Color32,
    handle_hover: Color32,
}

impl PopoutStyle {
    pub fn new(palette: &Palette) -> Self {
        // customtkinter's scroll bar button: gray55 / gray41, lifting to
        // gray40 / gray53 under the pointer.
        let (handle, handle_hover) = if palette.dark {
            (Color32::from_rgb(0x69, 0x69, 0x69), Color32::from_rgb(0x87, 0x87, 0x87))
        } else {
            (Color32::from_rgb(0x8C, 0x8C, 0x8C), Color32::from_rgb(0x66, 0x66, 0x66))
        };
        PopoutStyle { font: theme::font(POPOUT_FONT_SIZE), bold: theme::bold(POPOUT_FONT_SIZE), handle, handle_hover }
    }

    /// A slim solid scroll bar beside the content rather than over it.
    pub fn apply_scrollbar(&self, ui: &mut egui::Ui) {
        let style = ui.style_mut();
        style.spacing.scroll = egui::style::ScrollStyle {
            floating: false,
            bar_width: 8.0,
            handle_min_length: 24.0,
            bar_inner_margin: 6.0,
            bar_outer_margin: 4.0,
            // A CTkScrollableFrame cuts its last row off at the edge; egui
            // otherwise washes the rows at either end into the background,
            // which drew the row under the footer as a faded ghost.
            fade: egui::style::ScrollFadeStyle { strength: 0.0, ..Default::default() },
            ..egui::style::ScrollStyle::solid()
        };
        let widgets = &mut style.visuals.widgets;
        widgets.inactive.bg_fill = self.handle;
        widgets.hovered.bg_fill = self.handle_hover;
        widgets.active.bg_fill = self.handle_hover;
        for state in [&mut widgets.inactive, &mut widgets.hovered, &mut widgets.active] {
            state.corner_radius = egui::CornerRadius::same(4);
        }
    }
}

/// Visuals for a pop-out, set on its own root so the text box's cursor and
/// selection follow the app's mode whatever theme egui thinks the window has.
fn popout_visuals(palette: &Palette) -> egui::Visuals {
    let mut visuals = if palette.dark { egui::Visuals::dark() } else { egui::Visuals::light() };
    visuals.panel_fill = palette.bg;
    visuals.window_fill = palette.bg;
    visuals.extreme_bg_color = palette.bg;
    visuals.override_text_color = Some(palette.text);
    visuals.selection.bg_fill = palette.tab_selected;
    visuals.selection.stroke.color = palette.tab_selected_text;
    visuals.text_cursor.stroke.color = palette.text;
    visuals
}

/// The parts every pop-out builder shares: the app icon, and where it opens.
pub fn finish_builder(mut builder: ViewportBuilder, position: Option<(f32, f32)>) -> ViewportBuilder {
    if let Some(icon) = asset_path::window_icon() {
        builder = builder.with_icon(icon);
    }
    if let Some((x, y)) = position {
        builder = builder.with_position([x, y]);
    }
    builder
}

// -- placement -----------------------------------------------------------

/// Which side of the main window a pop-out opens on.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Side {
    Left,
    Right,
}

/// Place a pop-out beside the main viewer, falling back on-screen.
///
/// `main` is the main window's outer rectangle and `screen` the screen it is
/// on, both in points.
pub fn adjacent_window_position(side: Side, popup_width: f32, popup_height: f32, main: Rect, screen: egui::Vec2, gap: f32) -> (f32, f32) {
    let left = main.left() - popup_width - gap;
    let right = main.left() + main.width() + gap;
    let x = match side {
        Side::Left => {
            if left >= 0.0 {
                left
            } else {
                right
            }
        }
        Side::Right => {
            if right + popup_width <= screen.x {
                right
            } else {
                left
            }
        }
    };
    let x = (x.trunc()).min((screen.x - popup_width).max(0.0)).max(0.0);
    let y = (main.top().trunc()).min((screen.y - popup_height).max(0.0)).max(0.0);
    (x, y)
}

fn position_beside(ctx: &egui::Context, side: Side, width: f32, height: f32) -> Option<(f32, f32)> {
    let (outer, monitor) = ctx.input(|i| (i.viewport().outer_rect, i.viewport().monitor_size));
    let outer = outer?;
    let screen = monitor.unwrap_or(egui::vec2(1920.0, 1080.0));
    // The pop-outs wear the Windows frame, which the Python placed by its
    // client size too; the frame is the gap's business.
    Some(adjacent_window_position(side, width, height, outer, screen, 8.0))
}

// -- channel headings ------------------------------------------------------

/// Name the two channel columns after the slot each one holds.
///
/// "A2/B2" says which stick a column is; "ChA/ChB" only says which memory
/// controller. The slot comes from the board's own DeviceLocator, so it is
/// the same name printed on the board and shown by every other tool.
///
/// Only a channel holding exactly one module is renamed. With two sticks on
/// a channel the column still covers both, and neither slot name would be
/// right, so that channel keeps its generic label.
pub fn channel_slot_labels(modules: &[DimmModule]) -> HashMap<String, String> {
    slots_by_channel(modules)
        .into_iter()
        .filter(|(_, slots)| slots.len() == 1)
        .map(|(channel, slots)| (channel.to_lowercase(), slots[0].clone()))
        .collect()
}

/// The two channel headings, shared with the reads that place a module
/// under each.
pub type ChannelLabels = Arc<Mutex<(String, String)>>;

pub fn channel_labels(a: &str, b: &str) -> ChannelLabels {
    Arc::new(Mutex::new((a.to_string(), b.to_string())))
}

/// The two channel column headings: the slot name when one is known,
/// otherwise the defaults.
pub fn channel_headers(labels: &HashMap<String, String>, default_a: &str, default_b: &str) -> (String, String) {
    (
        labels.get("a").cloned().unwrap_or_else(|| default_a.to_string()),
        labels.get("b").cloned().unwrap_or_else(|| default_b.to_string()),
    )
}

// -- the Advanced feeder ---------------------------------------------------

/// The reading tabs, in the order the tab strip shows them. Summary is left
/// out because every row on it is repeated from one of these, and the
/// telemetry window is its own thing with its own statistics.
pub const ADVANCED_TABS: [&str; 6] = ["System Info", "Timings", "Training", "IMC", "RTL", "Voltages"];

/// How a row is read for display: `Some(channel)` is one side of a dual row,
/// `None` the row as the compact tables show it.
pub type RowRead = Arc<dyn Fn(&Row, Option<Channel>) -> String + Send + Sync>;

/// The backend's own reader, as the tables use it.
pub fn backend_row_read(backend: &Backend) -> RowRead {
    let unavailable = backend.unavailable;
    Arc::new(move |row, channel| match channel {
        Some(channel) => display_values::read_side(row, channel, unavailable),
        None => display_values::read_value(row, None, unavailable),
    })
}

/// Every row the reading tabs display, as Advanced lists them.
///
/// A row that reads two channels reads as an (A, B) pair, so the window can
/// column them under A1/B1 headings the way the tabs do. Squeezed into one
/// string it read "A 2  |  B 1", which is the only shape tWPRE -- the one
/// timing whose channels differ -- ever appeared in.
///
/// Built tab by tab rather than by walking the rows once, so the window
/// groups the way the tab strip reads even though the table does not store
/// the rows in that order.
pub fn advanced_entries(backend: &Arc<Backend>, read: RowRead) -> Vec<Entry> {
    let mut entries = Vec::new();
    for tab in ADVANCED_TABS {
        for (index, row) in backend.rows.iter().enumerate() {
            if row.tab != tab {
                continue;
            }
            if row.extra_bool("diagnostic") && !row.extra_bool("advanced_only") {
                continue;
            }
            // The tables use blank rows as spacers; they are layout, not
            // readings, and a searchable list has nothing to do with them.
            if row.name.trim().is_empty() {
                continue;
            }
            let backend = backend.clone();
            let read = read.clone();
            let paired = row.is_dual();
            let entry = Entry::new(tab, row.category.clone(), row.name.clone(), paired, move || {
                let row = &backend.rows[index];
                Ok(if paired {
                    Reading::Pair(Some(read(row, Some(Channel::A))), Some(read(row, Some(Channel::B))))
                } else {
                    Reading::Single(Some(read(row, None)))
                })
            });
            entries.push(entry.with_label(row.label()));
        }
    }
    entries
}

/// (label, key) rows of the SPD page.
pub type SpdFieldList = &'static [(&'static str, &'static str)];

/// The SPD page's module fields, in reading order: the first half is the
/// MODULE panel, the second IDENTITY.
pub const SPD_FIELDS: &[(&str, &str)] = &[
    ("Memory Type", "memory_type"),
    ("Module Type", "module_type"),
    ("Channels", "system_channels"),
    ("Capacity", "system_capacity"),
    ("Module Size", "capacity"),
    ("Max Bandwidth", "max_bandwidth"),
    ("Module Manuf.", "module_manufacturer"),
    ("DRAM Manuf.", "dram_manufacturer"),
    ("DRAM Die", "dram_die"),
    ("SPD Extension", "extension"),
    ("Ranks", "rank"),
    ("Part Number", "part_number"),
    ("Serial Number", "serial_number"),
    ("Manufactured", "manufacture_date"),
    ("SPD Address", "address"),
];

/// On LGA1700 DDR5 the module's makeup reads down MODULE -- size, the dies
/// it is built from, its ranks -- and IDENTITY opens with the DRAM maker
/// beside the die, then the module's own PMIC and SPD hub.
pub const SPD_DDR5_FIELDS: &[(&str, &str)] = &[
    ("Memory Type", "memory_type"),
    ("Module Type", "module_type"),
    ("Channels", "system_channels"),
    ("Capacity", "system_capacity"),
    ("Module Size", "capacity"),
    ("DRAM Organization", "organization"),
    ("Ranks", "rank"),
    // The JEDEC reference PCB the module is built on, which is what tuners
    // mean by an A-die kit being "on an A0 board".
    ("Raw Card", "raw_card"),
    ("Max Bandwidth", "max_bandwidth"),
    ("Module Manuf.", "module_manufacturer"),
    ("DRAM Manuf.", "dram_manufacturer"),
    ("DRAM Die", "dram_die"),
    ("PMIC", "pmic"),
    ("SPD Hub", "spd_hub"),
    ("SPD Extension", "extension"),
    ("Part Number", "part_number"),
    ("Serial Number", "serial_number"),
    ("Manufactured", "manufacture_date"),
    ("SPD Address", "address"),
];

/// The SPD timings table's rows.
pub const SPD_PROFILE_FIELDS: &[(&str, &str)] = &[
    ("Frequency", "frequency"),
    ("CAS Latency", "cl"),
    ("tRCD", "trcd"),
    ("tRP", "trp"),
    ("tRAS", "tras"),
    ("tRC", "trc"),
    ("Voltage", "voltage"),
];

/// DDR5 profiles also carry tWR, the three refresh cycle times, tRRD_L,
/// tCCD_L, tCCD_L_WR, tFAW, tRTP, tWTR_L and tWTR_S, and a voltage for each
/// of VDD, VDDQ and VPP. EXPO stores only four of the floored timings, and
/// XMP's are not read, so those columns show a dash for the rest.
pub const SPD_DDR5_PROFILE_FIELDS: &[(&str, &str)] = &[
    ("Frequency", "frequency"),
    ("CAS Latency", "cl"),
    ("tRCD", "trcd"),
    ("tRP", "trp"),
    ("tRAS", "tras"),
    ("tRC", "trc"),
    ("tWR", "twr"),
    ("tRFC1", "trfc1"),
    ("tRFC2", "trfc2"),
    ("tRFCsb", "trfcsb"),
    ("tRRD_L", "trrd_l"),
    ("tCCD_L", "tccd_l"),
    ("tCCD_L_WR", "tccd_l_wr"),
    ("tFAW", "tfaw"),
    ("tRTP", "trtp"),
    ("tWTR_L", "twtr_l"),
    ("tWTR_S", "twtr_s"),
    ("VDD", "vdd"),
    ("VDDQ", "vddq"),
    ("VPP", "vpp"),
];

/// What this machine's SPD page shows, and so what Advanced lists for it.
/// AM5 is DDR5-only, so it takes the DDR5 page too: it had been getting the
/// DDR4-shaped one, which leaves out the organization, the PMIC and SPD hub,
/// and every profile timing past tRC.
pub fn spd_tab_fields(platform: Platform) -> (SpdFieldList, SpdFieldList) {
    match platform {
        Platform::Lga1700Ddr5 | Platform::Am5 => (SPD_DDR5_FIELDS, SPD_DDR5_PROFILE_FIELDS),
        _ => (SPD_FIELDS, SPD_PROFILE_FIELDS),
    }
}

/// The SPD modules Advanced lists, read once off the UI thread.
#[derive(Default)]
pub struct SpdState {
    pub modules: Vec<SpdRecord>,
    /// The system-wide values moved from System Info to the SPD page
    /// (`system_channels`, `system_capacity`).
    pub system_values: HashMap<String, String>,
    loading: bool,
    loaded: bool,
}

#[derive(Clone, Default)]
pub struct SpdSource {
    inner: Arc<Mutex<SpdState>>,
}

impl SpdSource {
    /// A source already holding `modules`, as a finished read leaves it.
    #[cfg_attr(not(test), allow(dead_code))]
    pub fn with(modules: Vec<SpdRecord>, system_values: HashMap<String, String>) -> Self {
        SpdSource {
            inner: Arc::new(Mutex::new(SpdState { modules, system_values, loading: false, loaded: true })),
        }
    }

    fn state(&self) -> std::sync::MutexGuard<'_, SpdState> {
        self.inner.lock().unwrap_or_else(|e| e.into_inner())
    }

    /// Whether a read is running, and whether one has finished.
    pub fn status(&self) -> (bool, bool) {
        let state = self.state();
        (state.loading, state.loaded)
    }

    /// The modules the read found, and the system-wide values it resolved.
    pub fn snapshot(&self) -> (Vec<SpdRecord>, HashMap<String, String>) {
        let state = self.state();
        (state.modules.clone(), state.system_values.clone())
    }

    /// Read SPD once off the UI thread, if it has not been. The SPD tab and
    /// Advanced share this one read, as the Python's page and window shared
    /// `_spd_modules`: two SMBus walks at once could disagree, and a page
    /// already read left Advanced waiting on a read of its own.
    pub fn load_async(&self, backend: Arc<Backend>, repaint: impl Fn() + Send + 'static) {
        {
            let mut state = self.state();
            if state.loaded || state.loading {
                return;
            }
            state.loading = true;
        }
        let source = self.clone();
        let spawned = std::thread::Builder::new().name("SPD reader".into()).spawn(move || {
            let read = std::panic::catch_unwind(|| rochviewer::memory::spd_profiles::read_spd_modules(None, false));
            // A read that fails drops the system values with the modules,
            // as the Python's worker did: the page shows dashes throughout.
            let (modules, system_values) = match read {
                Ok(Ok(modules)) => (modules, read_spd_system_values(&backend)),
                Ok(Err(error)) => {
                    eprintln!("SPD tab unavailable: {error}");
                    (Vec::new(), HashMap::new())
                }
                Err(_) => {
                    eprintln!("SPD tab unavailable: the reader panicked");
                    (Vec::new(), HashMap::new())
                }
            };
            {
                let mut state = source.state();
                state.modules = modules;
                state.system_values = system_values;
                state.loading = false;
                state.loaded = true;
            }
            repaint();
        });
        if spawned.is_err() {
            self.state().loading = false;
        }
    }
}

/// Resolve the system-wide values moved from System Info to SPD.
fn read_spd_system_values(backend: &Backend) -> HashMap<String, String> {
    let keys = [("Channels", "system_channels"), ("Memory Capacity", "system_capacity")];
    let mut values = HashMap::new();
    for row in &backend.rows {
        if row.tab != "SPD" {
            continue;
        }
        if let Some((_, key)) = keys.iter().find(|(name, _)| *name == row.name) {
            let value = row.value.as_ref().map(|v| display_values::resolve_display_value(v, EM_DASH)).unwrap_or_default();
            values.insert(key.to_string(), value);
        }
    }
    values
}

/// One SPD field as the page shows it; Advanced reads it the same way.
///
/// The system-wide values moved here from System Info win over the module's
/// own, the SPD address reads in hex, and nothing reads as a dash.
pub fn spd_field_text(system_values: &HashMap<String, String>, module: &SpdRecord, key: &str) -> String {
    let text = match system_values.get(key) {
        Some(value) => Some(value.clone()),
        None => match module.get(key) {
            Some(SpdValue::Int(value)) if key == "address" => Some(format!("0x{value:02X}")),
            Some(value) => value.display(),
            None => None,
        },
    };
    text.filter(|t| !t.is_empty()).unwrap_or_else(|| EM_DASH.to_string())
}

/// Each module under the channel heading its slot names.
///
/// A module whose slot the headings do not name still gets a column -- the
/// first one free, in the order the modules were read -- rather than being
/// left out beside one that matched.
fn place_modules(found: &[SpdRecord], channels: &(String, String)) -> [Option<SpdRecord>; 2] {
    let slot = |module: &SpdRecord| module.text("slot").unwrap_or_else(|| "None".to_string());
    let mut by_slot: BTreeMap<String, usize> = BTreeMap::new();
    for (index, module) in found.iter().enumerate() {
        by_slot.insert(slot(module), index);
    }
    let placed: Vec<Option<usize>> = [&channels.0, &channels.1].iter().map(|label| by_slot.get(*label).copied()).collect();
    let mut rest: Vec<usize> = (0..found.len()).filter(|i| !placed.contains(&Some(*i))).collect();
    let mut take = |placed: Option<usize>| {
        placed.or_else(|| if rest.is_empty() { None } else { Some(rest.remove(0)) }).map(|i| found[i].clone())
    };
    let a = take(placed[0]);
    let b = take(placed[1]);
    [a, b]
}

/// The SPD tab's module fields and profile table, one column a module.
///
/// Read from the modules the SPD source reads, so they fill in once that
/// read is done -- opening Advanced starts it. Each module goes under the
/// channel heading its slot names; the profiles are numbered sections headed
/// by the profile's own name, since which profiles a kit carries is not known
/// until it is read.
pub fn advanced_spd_entries(
    spd: &SpdSource,
    fields: SpdFieldList,
    profile_fields: SpdFieldList,
    channels: ChannelLabels,
) -> Vec<Entry> {
    let field = |key: &'static str| {
        let spd = spd.clone();
        let channels = channels.clone();
        move || {
            let channels = channels.lock().unwrap_or_else(|e| e.into_inner()).clone();
            let state = spd.state();
            let [a, b] = place_modules(&state.modules, &channels);
            let text = |module: &Option<SpdRecord>| match module {
                None => EM_DASH.to_string(),
                Some(module) => spd_field_text(&state.system_values, module, key),
            };
            Ok(Reading::Pair(Some(text(&a)), Some(text(&b))))
        }
    };
    let profile = |index: usize, key: &'static str| {
        let spd = spd.clone();
        let channels = channels.clone();
        move || {
            let channels = channels.lock().unwrap_or_else(|e| e.into_inner()).clone();
            let state = spd.state();
            let [a, b] = place_modules(&state.modules, &channels);
            let text = |module: &Option<SpdRecord>| {
                let value = module.as_ref().and_then(|m| m.profiles.get(index)).and_then(|p| p.get(key)).and_then(SpdValue::display);
                value.filter(|v| !v.is_empty()).unwrap_or_else(|| EM_DASH.to_string())
            };
            Ok(Reading::Pair(Some(text(&a)), Some(text(&b))))
        }
    };

    let half = fields.len().div_ceil(2);
    let mut entries: Vec<Entry> = fields
        .iter()
        .enumerate()
        .map(|(position, &(label, key))| {
            let category = if position < half { "Module" } else { "Identity" };
            Entry::new("SPD", category, label, true, field(key)).with_label(label)
        })
        .collect();
    for index in 0..5 {
        let category = format!("Profile {}", index + 1);
        entries.push(Entry::new("SPD", category.clone(), format!("Profile {}", index + 1), true, profile(index, "name")).with_label("Profile"));
        for &(label, key) in profile_fields {
            let name = format!("{label} (Profile {})", index + 1);
            entries.push(Entry::new("SPD", category.clone(), name, true, profile(index, key)).with_label(label));
        }
    }
    entries
}

// -- the Telemetry feeder ----------------------------------------------------

/// Sensor Telemetry group order: what the silicon is doing first, then what
/// it is drawing, then the rails feeding it. The rails are one section,
/// ordered core outward within it. The per-DIMM panels follow, each with its
/// own PMIC.
pub const SENSOR_GROUP_ORDER: [&str; 6] = [
    "Clocks",
    "Thermal & Power",
    "Voltages",
    "Fans",
    "Graphics",
    // Last, because it is the one section that should stay empty of news.
    "Errors",
];

/// The key a row names its parent under (`Parent` in the Python dicts).
pub const PARENT_ROW_KEY: &str = "Parent";

fn sensor_title(row: &Row) -> String {
    if row.category.is_empty() { "Sensors".to_string() } else { row.category.clone() }
}

/// The live sensor rows grouped by section, for the Telemetry window.
///
/// These are the rows that used to be the Sensors tab. They stay in the
/// table, but a tab shows one instant, and what a rail does over time is the
/// interesting part.
///
/// Groups come out in SENSOR_GROUP_ORDER, which reads outward from what the
/// silicon is doing to what is feeding it. A group the platform declares but
/// that order does not name keeps its declaration order at the end, so
/// another platform's sections are never dropped.
///
/// Each reading names the row it folds under -- the per-processor effective
/// clocks under their aggregate -- or none for a row that stands on its own.
/// Nothing is read here: whether a row shows is the window's call, made from
/// its own readings.
pub fn sensor_groups(backend: &Arc<Backend>, read: RowRead) -> Vec<SensorGroup> {
    let mut groups: Vec<SensorGroup> = Vec::new();
    for (index, row) in backend.rows.iter().enumerate() {
        if !WINDOWED_TABS.contains(&row.tab.as_str()) {
            continue;
        }
        let title = sensor_title(row);
        if title == "Graphics" {
            continue;
        }
        let position = match groups.iter().position(|(name, _)| *name == title) {
            Some(position) => position,
            None => {
                groups.push((title, Vec::new()));
                groups.len() - 1
            }
        };
        let backend = backend.clone();
        let read = read.clone();
        groups[position].1.push(SensorReading {
            label: row.label().to_string(),
            read: Arc::new(move || read(&backend.rows[index], None)),
            parent: row.extra_str(PARENT_ROW_KEY).map(str::to_string),
        });
    }
    let rank = |title: &str| SENSOR_GROUP_ORDER.iter().position(|t| *t == title).unwrap_or(SENSOR_GROUP_ORDER.len());
    groups.sort_by_key(|(title, _)| rank(title));
    groups
}

/// The sensor rows the window holds back until they read.
///
/// Rows marked `hide_when_blank` -- a fan header that may have no fan, a
/// board channel on a board it is not mapped for. The window decides from its
/// own worker's readings, so nothing is read on this thread to decide it,
/// and a row that answers late still appears.
pub fn sensor_reveal_keys(rows: &[Row]) -> HashSet<StatKey> {
    rows.iter()
        .filter(|row| WINDOWED_TABS.contains(&row.tab.as_str()) && row.extra_bool("hide_when_blank"))
        .map(|row| StatKey::sensor(&sensor_title(row), row.label()))
        .collect()
}

/// Module identity for the telemetry window.
///
/// SMBIOS names the module and the slot; the SPD adds the module vendor,
/// which firmware often leaves as "Unknown" -- it reads G.Skill off the DIMM
/// here while WMI has nothing. The IC maker and die the panel shows come from
/// each module's SPD where it answers, not from the part-number table.
pub fn telemetry_modules() -> Vec<TelemetryModule> {
    use rochviewer::memory::ddr5_spd::read_identity;
    use rochviewer::memory::dimm_inventory::{apply_spd_ic, rank_numeric, read_modules};

    let identities = read_identity(None, false, None);
    let mut spd_by_part: HashMap<String, String> = HashMap::new();
    for entry in &identities {
        let part = entry.part_number.trim();
        if !part.is_empty() {
            spd_by_part.insert(part.to_string(), entry.module_manufacturer.clone());
        }
    }
    apply_spd_ic(&read_modules(false), &identities)
        .into_iter()
        .map(|module| TelemetryModule {
            channel: module.channel.clone(),
            slot: module.slot.clone(),
            capacity_gb: module.capacity_gb,
            rank_numeric: Some(rank_numeric(module.rank_count)),
            ic: Some(module.ic.clone()),
            part_number: Some(module.part_number.clone()),
            spd_vendor: spd_by_part.get(module.part_number.trim()).filter(|v| !v.is_empty()).cloned(),
            module_manufacturer: Some(module.module_manufacturer.clone()),
        })
        .collect()
}

// -- the windows ---------------------------------------------------------------

struct Popout<T> {
    id: ViewportId,
    builder: ViewportBuilder,
    window: Arc<Mutex<T>>,
    /// The native window, once it has been found; per window, so a
    /// reopened one is looked up again.
    hwnd: Arc<Mutex<Option<isize>>>,
    /// When the window was asked for, until its first frame is drawn.
    opened: Arc<Mutex<Option<Instant>>>,
    name: String,
    /// Where to put the window once it exists, in screen pixels.
    restore_to: Arc<Mutex<Option<(i32, i32)>>>,
    /// Where it is now, in screen pixels.
    position: Arc<Mutex<Option<(i32, i32)>>>,
}

/// Something a pop-out draws in its own viewport.
trait PopoutWindow: Send + 'static {
    fn draw(&mut self, ui: &mut egui::Ui);
    fn is_open(&self) -> bool;
    fn palette_mut(&mut self) -> &mut Palette;
}

impl PopoutWindow for AdvancedWindow {
    fn draw(&mut self, ui: &mut egui::Ui) {
        self.ui(ui);
    }
    fn is_open(&self) -> bool {
        self.open
    }
    fn palette_mut(&mut self) -> &mut Palette {
        &mut self.palette
    }
}

impl PopoutWindow for SettingsWindow {
    fn draw(&mut self, ui: &mut egui::Ui) {
        self.ui(ui);
    }
    fn is_open(&self) -> bool {
        self.open
    }
    fn palette_mut(&mut self) -> &mut Palette {
        &mut self.palette
    }
}

impl PopoutWindow for DimmTelemetryWindow {
    fn draw(&mut self, ui: &mut egui::Ui) {
        self.ui(ui);
    }
    fn is_open(&self) -> bool {
        self.open
    }
    fn palette_mut(&mut self) -> &mut Palette {
        &mut self.palette
    }
}

/// This process's visible top-level window with exactly this title.
pub fn own_window_titled(title: &str) -> Option<windows::Win32::Foundation::HWND> {
    own_window(title, true)
}

/// This process's top-level window with exactly this title; `visible_only`
/// skips one not shown yet, which the main window is on its first frame.
pub fn own_window(title: &str, visible_only: bool) -> Option<windows::Win32::Foundation::HWND> {
    use windows::Win32::Foundation::{HWND, LPARAM};
    use windows::Win32::UI::WindowsAndMessaging::{EnumWindows, GetWindowThreadProcessId, InternalGetWindowText, IsWindowVisible};
    use windows::core::BOOL;

    struct Search {
        pid: u32,
        title: Vec<u16>,
        visible_only: bool,
        found: Option<HWND>,
    }
    unsafe extern "system" fn visit(hwnd: HWND, lparam: LPARAM) -> BOOL {
        // SAFETY: lparam is the &mut Search passed below, alive for the call.
        let search = unsafe { &mut *(lparam.0 as *mut Search) };
        let mut pid = 0u32;
        unsafe { GetWindowThreadProcessId(hwnd, Some(&mut pid)) };
        if pid != search.pid || (search.visible_only && !unsafe { IsWindowVisible(hwnd) }.as_bool()) {
            return BOOL(1);
        }
        let mut buffer = [0u16; 256];
        // InternalGetWindowText reads the title without sending WM_GETTEXT:
        // GetWindowTextW messages each window, and one owned by a thread of
        // ours that is not pumping messages (WMI's, the reader's) kept the
        // first frame waiting a tenth of a second.
        let length = unsafe { InternalGetWindowText(hwnd, &mut buffer) }.max(0) as usize;
        if buffer[..length] == search.title[..] {
            search.found = Some(hwnd);
            return BOOL(0);
        }
        BOOL(1)
    }
    let mut search = Search { pid: std::process::id(), title: title.encode_utf16().collect(), visible_only, found: None };
    // SAFETY: the callback only touches `search` for the length of the call.
    let _ = unsafe { EnumWindows(Some(visit), LPARAM(&mut search as *mut Search as isize)) };
    search.found
}

/// The window procedure each width-capped window had before ours.
static PREVIOUS_PROCS: Mutex<Vec<(isize, isize)>> = Mutex::new(Vec::new());

unsafe extern "system" fn capped_width_proc(
    hwnd: windows::Win32::Foundation::HWND,
    message: u32,
    wparam: windows::Win32::Foundation::WPARAM,
    lparam: windows::Win32::Foundation::LPARAM,
) -> windows::Win32::Foundation::LRESULT {
    use windows::Win32::UI::WindowsAndMessaging::{CallWindowProcW, DefWindowProcW, MINMAXINFO, WM_GETMINMAXINFO, WM_NCDESTROY, WNDPROC};
    let key = hwnd.0 as isize;
    let previous = PREVIOUS_PROCS.lock().unwrap_or_else(|e| e.into_inner()).iter().find(|(h, _)| *h == key).map(|(_, p)| *p);
    let Some(previous) = previous else {
        // SAFETY: the default procedure, for a window we no longer track.
        return unsafe { DefWindowProcW(hwnd, message, wparam, lparam) };
    };
    // SAFETY: `previous` is the WNDPROC SetWindowLongPtrW handed back.
    let previous: WNDPROC = unsafe { std::mem::transmute::<isize, WNDPROC>(previous) };
    let result = unsafe { CallWindowProcW(previous, hwnd, message, wparam, lparam) };
    if message == WM_GETMINMAXINFO && lparam.0 != 0 {
        // SAFETY: WM_GETMINMAXINFO's lparam is the MINMAXINFO to fill in.
        let info = unsafe { &mut *(lparam.0 as *mut MINMAXINFO) };
        if info.ptMaxTrackSize.x > 0 && info.ptMaxTrackSize.x < info.ptMaxSize.x {
            info.ptMaxSize.x = info.ptMaxTrackSize.x;
        }
    } else if message == WM_NCDESTROY {
        PREVIOUS_PROCS.lock().unwrap_or_else(|e| e.into_inner()).retain(|(h, _)| *h != key);
    }
    result
}

/// Make maximizing keep the window's maximum width, as Tk's
/// `resizable(False, True)` did: the window takes the screen's full height
/// at the left edge rather than the whole screen. winit hands Windows the
/// width cap only as the largest size a drag can reach.
pub fn cap_maximized_width(hwnd: windows::Win32::Foundation::HWND) {
    use windows::Win32::UI::WindowsAndMessaging::{GWLP_WNDPROC, SetWindowLongPtrW};
    let key = hwnd.0 as isize;
    if PREVIOUS_PROCS.lock().unwrap_or_else(|e| e.into_inner()).iter().any(|(h, _)| *h == key) {
        return;
    }
    // SAFETY: our own window, on the thread that owns it, so no message
    // reaches the new procedure before its predecessor is recorded; every
    // message is passed on to that predecessor.
    let previous = unsafe { SetWindowLongPtrW(hwnd, GWLP_WNDPROC, capped_width_proc as *const () as isize) };
    if previous != 0 {
        PREVIOUS_PROCS.lock().unwrap_or_else(|e| e.into_inner()).push((key, previous));
    }
}

/// Draw a borderless window as a native menu is drawn: square corners and
/// no frame line of DWM's own. Windows 11 rounds and outlines every
/// top-level window, which the Tk option menu's native menu never was.
pub fn square_menu_window(hwnd: windows::Win32::Foundation::HWND) -> bool {
    use windows::Win32::Graphics::Dwm::{DWMWA_BORDER_COLOR, DWMWA_COLOR_NONE, DWMWA_WINDOW_CORNER_PREFERENCE, DWMWCP_DONOTROUND, DwmSetWindowAttribute};
    let corner = DWMWCP_DONOTROUND.0;
    let border = DWMWA_COLOR_NONE;
    // SAFETY: 4-byte values, as both attributes take. Either fails harmlessly
    // before Windows 11.
    unsafe {
        let squared = DwmSetWindowAttribute(hwnd, DWMWA_WINDOW_CORNER_PREFERENCE, &corner as *const i32 as *const _, 4).is_ok();
        let _ = DwmSetWindowAttribute(hwnd, DWMWA_BORDER_COLOR, &border as *const u32 as *const _, 4);
        squared
    }
}

/// Make `owner` the owner of `hwnd`: an owned window stays above its owner,
/// and when it closes Windows activates the owner again.
pub fn set_owner(hwnd: windows::Win32::Foundation::HWND, owner: windows::Win32::Foundation::HWND) {
    use windows::Win32::UI::WindowsAndMessaging::{GWLP_HWNDPARENT, SetWindowLongPtrW};
    // SAFETY: both are live top-level windows of ours; GWLP_HWNDPARENT on a
    // top-level window sets its owner.
    unsafe { SetWindowLongPtrW(hwnd, GWLP_HWNDPARENT, owner.0 as isize) };
}

/// Whether a window's native title bar is drawn dark.
pub fn title_bar_dark(hwnd: windows::Win32::Foundation::HWND) -> Option<bool> {
    use windows::Win32::Graphics::Dwm::{DWMWA_USE_IMMERSIVE_DARK_MODE, DwmGetWindowAttribute};
    let mut value: i32 = 0;
    // SAFETY: a 4-byte BOOL out-parameter, as the attribute takes.
    unsafe { DwmGetWindowAttribute(hwnd, DWMWA_USE_IMMERSIVE_DARK_MODE, &mut value as *mut i32 as *mut _, 4) }.ok()?;
    Some(value != 0)
}

/// Give a native title bar the app's mode, as customtkinter did with
/// DWMWA_USE_IMMERSIVE_DARK_MODE. True when it had to be changed.
pub fn sync_title_bar(hwnd: windows::Win32::Foundation::HWND, dark: bool) -> bool {
    use windows::Win32::Graphics::Dwm::{DWMWA_USE_IMMERSIVE_DARK_MODE, DwmSetWindowAttribute};
    if title_bar_dark(hwnd) == Some(dark) {
        return false;
    }
    let value: i32 = dark.into();
    // SAFETY: a 4-byte BOOL, as the attribute takes; a stale handle fails.
    unsafe { DwmSetWindowAttribute(hwnd, DWMWA_USE_IMMERSIVE_DARK_MODE, &value as *const i32 as *const _, 4) }.is_ok()
}

impl<T: PopoutWindow> Popout<T> {
    fn new(name: &str, builder: ViewportBuilder, window: T) -> Self {
        Popout {
            id: ViewportId::from_hash_of(name),
            builder,
            window: Arc::new(Mutex::new(window)),
            hwnd: Arc::new(Mutex::new(None)),
            opened: Arc::new(Mutex::new(None)),
            name: name.to_string(),
            restore_to: Arc::new(Mutex::new(None)),
            position: Arc::new(Mutex::new(None)),
        }
    }

    /// Open where it was last rather than beside the main window: created
    /// there, and checked once it exists.
    fn restoring(mut self, at: Option<(i32, i32)>, zoom: f32) -> Self {
        if let Some(at) = at {
            self.builder = self.builder.with_position(window_tools::to_points(at, zoom));
        }
        *self.restore_to.lock().unwrap_or_else(|e| e.into_inner()) = at;
        self
    }

    /// Where the window is now, in screen pixels.
    fn current_position(&self) -> Option<(i32, i32)> {
        *self.position.lock().unwrap_or_else(|e| e.into_inner())
    }

    fn set_on_top(&self, ctx: &egui::Context, on_top: bool) {
        let level = if on_top { egui::WindowLevel::AlwaysOnTop } else { egui::WindowLevel::Normal };
        ctx.send_viewport_cmd_to(self.id, ViewportCommand::WindowLevel(level));
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, T> {
        self.window.lock().unwrap_or_else(|e| e.into_inner())
    }

    fn is_open(&self) -> bool {
        self.lock().is_open()
    }

    /// Bring the open window forward instead of opening a second.
    fn focus(&self, ctx: &egui::Context) {
        ctx.send_viewport_cmd_to(self.id, ViewportCommand::Minimized(false));
        ctx.send_viewport_cmd_to(self.id, ViewportCommand::Focus);
    }

    /// Show the viewport for this pass, following the main window's mode.
    fn show(&self, ctx: &egui::Context, palette: &Palette) {
        {
            let mut window = self.lock();
            let current = window.palette_mut();
            if *current != *palette {
                *current = *palette;
                ctx.request_repaint_of(self.id);
            }
        }
        let window = self.window.clone();
        let hwnd = self.hwnd.clone();
        let opened = self.opened.clone();
        let name = self.name.clone();
        let restore_to = self.restore_to.clone();
        let position = self.position.clone();
        let title = self.builder.title.clone().unwrap_or_default();
        let capped_width = self.builder.maximize_button == Some(true)
            && self.builder.max_inner_size.is_some_and(|size| size.x.is_finite());
        ctx.show_viewport_deferred(self.id, self.builder.clone(), move |ui, _class| {
            let frame_started = Instant::now();
            let mut window = window.lock().unwrap_or_else(|e| e.into_inner());
            let palette = *window.palette_mut();
            ui.style_mut().visuals = popout_visuals(&palette);
            // The native frame follows the app's mode, as customtkinter
            // darkened (or lightened) the pop-outs' title bars by setting
            // DWMWA_USE_IMMERSIVE_DARK_MODE. winit's SetTheme did not reach
            // the caption, and Windows puts the system's dark caption back
            // when the window is first shown, so it is checked every frame
            // and set again whenever it has drifted.
            let mut found = hwnd.lock().unwrap_or_else(|e| e.into_inner());
            if found.is_none() {
                *found = own_window_titled(&title).map(|h| h.0 as isize);
                if let Some(h) = *found
                    && capped_width
                {
                    cap_maximized_width(windows::Win32::Foundation::HWND(h as *mut _));
                }
            }
            if let Some(h) = *found
                && sync_title_bar(windows::Win32::Foundation::HWND(h as *mut _), palette.dark)
            {
                // Look again shortly: the first show can undo it once more.
                ui.ctx().request_repaint_after(Duration::from_millis(100));
            }
            if let Some(h) = *found {
                let native = window_tools::hwnd(h);
                let mut restore = restore_to.lock().unwrap_or_else(|e| e.into_inner());
                if let Some(at) = *restore
                    && let Some(now) = window_tools::position(native)
                {
                    *restore = None;
                    if (now.0 - at.0).abs() > 1 || (now.1 - at.1).abs() > 1 {
                        window_tools::move_to(native, at);
                    }
                }
                drop(restore);
                window_tools::apply_opacity(native, window_tools::target_opacity());
                if let Some(at) = window_tools::position(native) {
                    *position.lock().unwrap_or_else(|e| e.into_inner()) = Some(at);
                }
            }
            drop(found);
            window.draw(ui);
            if let Some(at) = opened.lock().unwrap_or_else(|e| e.into_inner()).take() {
                super::timing::took(&format!("{name} first frame (frame {:.1} ms)", frame_started.elapsed().as_secs_f64() * 1000.0), at);
            }
            if !window.is_open() {
                // The main window stops showing it on its next pass.
                ui.ctx().request_repaint_of(ViewportId::ROOT);
            }
        });
    }
}

/// A finished capture waiting to be saved.
struct Capture {
    image: Arc<egui::ColorImage>,
    tab: String,
    crop: Option<[usize; 4]>,
}

pub struct Popouts {
    pub last_screenshot: Option<PathBuf>,
    status: Option<(String, Instant)>,
    advanced: Option<Popout<AdvancedWindow>>,
    telemetry: Option<Popout<DimmTelemetryWindow>>,
    settings: Option<Popout<SettingsWindow>>,
    /// The always-on-top setting the open windows were last given.
    on_top: bool,
    /// The last place recorded for each pop-out.
    remembered: HashMap<&'static str, (i32, i32)>,
    capture: Arc<Mutex<Option<Capture>>>,
    auto_open_at: Option<Instant>,
}

impl Default for Popouts {
    fn default() -> Self {
        Popouts {
            last_screenshot: None,
            status: None,
            advanced: None,
            telemetry: None,
            settings: None,
            on_top: false,
            remembered: HashMap::new(),
            capture: Arc::new(Mutex::new(None)),
            auto_open_at: None,
        }
    }
}

impl Popouts {
    pub fn new() -> Self {
        Self::default()
    }

    /// Open the searchable all-rows window, or focus the open one.
    pub fn open_advanced(&mut self, ctx: &egui::Context, shared: &mut Shared) {
        let opened = Instant::now();
        if let Some(existing) = self.advanced.as_ref().filter(|p| p.is_open()) {
            existing.focus(ctx);
            return;
        }
        let backend = shared.backend.clone();
        let id = ViewportId::from_hash_of("advanced");
        // Named for the slots from the first frame when the main window
        // already knows them, as the Python passed its own headings in.
        let known: HashMap<String, String> = shared.channel_labels.iter().map(|(k, v)| (k.clone(), v.clone())).collect();
        let (a, b) = channel_headers(&known, "A1", "B1");
        let channels = channel_labels(&a, &b);
        if known.is_empty() {
            // Named for the slots once the inventory is read. Never on this
            // thread: WMI refuses a call from the window's thread
            // (RPC_E_WRONG_THREAD), and the inventory would cache the
            // failure as "no modules" for the rest of the run.
            let channels = channels.clone();
            let repaint_ctx = ctx.clone();
            let _ = std::thread::Builder::new().name("slot-labels".into()).spawn(move || {
                let labels = channel_slot_labels(&rochviewer::memory::dimm_inventory::read_modules(false));
                *channels.lock().unwrap_or_else(|e| e.into_inner()) = channel_headers(&labels, "A1", "B1");
                repaint_ctx.request_repaint_of(id);
            });
        }
        let mut entries = advanced_entries(&backend, backend_row_read(&backend));
        if shared.tab_names.iter().any(|t| t == "SPD") {
            // The SPD rows fill in from an SPD read; start it if it has not
            // been.
            let repaint_ctx = ctx.clone();
            shared.spd.load_async(backend.clone(), move || {
                repaint_ctx.request_repaint();
                repaint_ctx.request_repaint_of(id);
            });
            let (fields, profile_fields) = spd_tab_fields(backend.platform);
            entries.extend(advanced_spd_entries(shared.spd, fields, profile_fields, channels.clone()));
        }
        let window = AdvancedWindow::new(entries, channels, *shared.palette);
        let position = position_beside(ctx, Side::Right, advanced_window::WINDOW_WIDTH, advanced_window::WINDOW_HEIGHT);
        let builder = with_level(advanced_window::viewport_builder(position), shared.on_top());
        self.advanced = Some(Popout::new("advanced", builder, window).restoring(shared.saved_position("advanced"), ctx.zoom_factor()));
        super::timing::took("open Advanced (prepare)", opened);
        *self.advanced.as_ref().unwrap().opened.lock().unwrap() = Some(opened);
        ctx.request_repaint();
    }

    /// Open the telemetry window, or focus the open one.
    pub fn open_telemetry(&mut self, ctx: &egui::Context, shared: &mut Shared) {
        let opened = Instant::now();
        if let Some(existing) = self.telemetry.as_ref().filter(|p| p.is_open()) {
            existing.focus(ctx);
            return;
        }
        let backend = shared.backend.clone();
        let window = DimmTelemetryWindow::new(
            *shared.palette,
            Arc::new(|| rochviewer::memory::ddr5_telemetry::read_dimm_telemetry(None, None, None, None, None)),
            Arc::new(telemetry_modules),
            shared.settings.get_bool("telemetry_auto_open"),
            sensor_groups(&backend, backend_row_read(&backend)),
            sensor_reveal_keys(&backend.rows),
        );
        let position = position_beside(ctx, Side::Left, dimm_telemetry_window::WINDOW_WIDTH, dimm_telemetry_window::WINDOW_HEIGHT);
        let builder = with_level(dimm_telemetry_window::viewport_builder(position), shared.on_top());
        self.telemetry = Some(Popout::new("telemetry", builder, window).restoring(shared.saved_position("telemetry"), ctx.zoom_factor()));
        super::timing::took("open Telemetry (prepare)", opened);
        *self.telemetry.as_ref().unwrap().opened.lock().unwrap() = Some(opened);
        ctx.request_repaint();
    }

    /// Open the Settings window, or focus the open one.
    pub fn open_settings(&mut self, ctx: &egui::Context, shared: &mut Shared) {
        if let Some(existing) = self.settings.as_ref().filter(|p| p.is_open()) {
            existing.focus(ctx);
            return;
        }
        let window = SettingsWindow::new(
            *shared.palette,
            shared.prefs.clone(),
            shared.prefs_revision.clone(),
            shared.settings.get_bool("telemetry_auto_open"),
            Arc::new(summary_context(shared.backend)),
        );
        let position = position_beside(ctx, Side::Right, settings_window::WINDOW_WIDTH, settings_window::WINDOW_HEIGHT);
        let builder = with_level(settings_window::viewport_builder(position), shared.on_top());
        self.settings = Some(Popout::new("settings", builder, window).restoring(shared.saved_position("settings"), ctx.zoom_factor()));
        ctx.request_repaint();
    }

    /// Give every open window the always-on-top setting.
    fn follow_window_level(&mut self, ctx: &egui::Context, on_top: bool) {
        if self.on_top == on_top {
            return;
        }
        self.on_top = on_top;
        if let Some(p) = &self.advanced {
            p.set_on_top(ctx, on_top);
        }
        if let Some(p) = &self.telemetry {
            p.set_on_top(ctx, on_top);
        }
        if let Some(p) = &self.settings {
            p.set_on_top(ctx, on_top);
        }
    }

    /// Where each open pop-out is now, by its preferences key.
    fn positions(&self) -> Vec<(&'static str, (i32, i32))> {
        let mut out = Vec::new();
        if let Some(at) = self.advanced.as_ref().and_then(Popout::current_position) {
            out.push(("advanced", at));
        }
        if let Some(at) = self.telemetry.as_ref().and_then(Popout::current_position) {
            out.push(("telemetry", at));
        }
        if let Some(at) = self.settings.as_ref().and_then(Popout::current_position) {
            out.push(("settings", at));
        }
        out
    }

    /// Called once at startup: reopen Telemetry if the user asked for that.
    pub fn startup(&mut self, ctx: &egui::Context, shared: &mut Shared) {
        if shared.settings.get_bool("telemetry_auto_open") {
            // After the window exists, so the pop-out lands on top of it and
            // beside where it actually is.
            self.auto_open_at = Some(Instant::now() + Duration::from_millis(400));
            ctx.request_repaint_after(Duration::from_millis(400));
        }
    }

    /// Save a PNG of the main window and copy it to the clipboard.
    ///
    /// egui hands the rendered frame over once it has been painted, so the
    /// picture is the window as drawn, never what lies over it.
    pub fn take_screenshot(&mut self, ctx: &egui::Context, shared: &mut Shared) {
        let slot = self.capture.clone();
        let tab = shared.current_tab.to_string();
        let crop = ctx.input(|i| {
            let ppp = i.pixels_per_point;
            i.viewport().inner_rect.map(|r| [0, 0, (r.width() * ppp).round() as usize, (r.height() * ppp).round() as usize])
        });
        let repaint = ctx.clone();
        ctx.send_viewport_cmd_to(
            ViewportId::ROOT,
            ViewportCommand::Screenshot(egui::ScreenshotCallback::new(move |image| {
                *slot.lock().unwrap_or_else(|e| e.into_inner()) = Some(Capture { image, tab, crop });
                repaint.request_repaint_of(ViewportId::ROOT);
            })),
        );
    }

    fn save_capture(&mut self, capture: Capture) {
        let (width, height, pixels) = screenshot::image_bgra(&capture.image, capture.crop);
        let name = screenshot::screenshot_filename(&capture.tab, screenshot::LocalTime::now());
        let path = match screenshot::save_png(&screenshot::default_folder(), &name, width, height, &pixels) {
            Ok(path) => path,
            Err(error) => {
                eprintln!("Screenshot failed: {error}");
                self.show_screenshot_status("Screenshot failed");
                return;
            }
        };
        // The file is what matters; a clipboard another program is holding
        // open only costs the paste.
        if !screenshot::COPY_TO_CLIPBOARD.load(Ordering::Relaxed) {
            self.show_screenshot_status("Saved");
            self.last_screenshot = Some(path);
            return;
        }
        match screenshot::copy_to_clipboard(width, height, &pixels) {
            Ok(()) => self.show_screenshot_status("Saved · copied"),
            Err(error) => {
                eprintln!("Screenshot saved to {}; not copied: {error}", path.display());
                self.show_screenshot_status("Saved");
            }
        }
        self.last_screenshot = Some(path);
    }

    fn show_screenshot_status(&mut self, text: &str) {
        self.status = Some((text.to_string(), Instant::now()));
    }

    /// Draw the open windows and handle anything that arrived this frame
    /// (a finished screenshot, a closed window).
    pub fn show(&mut self, ctx: &egui::Context, shared: &mut Shared) {
        let capture = self.capture.lock().ok().and_then(|mut c| c.take());
        if let Some(capture) = capture {
            self.save_capture(capture);
        }
        if let Some(at) = self.auto_open_at {
            if Instant::now() >= at {
                self.auto_open_at = None;
                self.open_telemetry(ctx, shared);
            } else {
                ctx.request_repaint_after(at.saturating_duration_since(Instant::now()));
            }
        }

        if self.advanced.as_ref().is_some_and(|p| !p.is_open()) {
            self.advanced = None;
        }
        if let Some(popout) = &self.advanced {
            popout.show(ctx, shared.palette);
        }

        if let Some(popout) = &self.telemetry {
            let changed = popout.lock().auto_open_changed.take();
            if let Some(value) = changed {
                shared.settings.save("telemetry_auto_open", serde_json::Value::Bool(value));
            }
        }
        if self.telemetry.as_ref().is_some_and(|p| !p.is_open()) {
            self.telemetry = None;
        }
        if let Some(popout) = &self.telemetry {
            popout.show(ctx, shared.palette);
        }

        if let Some(popout) = &self.settings {
            let requests = std::mem::take(&mut popout.lock().requests);
            if let Some(value) = requests.auto_open {
                shared.settings.save("telemetry_auto_open", serde_json::Value::Bool(value));
            }
            if requests.forget_positions {
                // Nothing saved means nothing to put back: a window open now
                // records its place again only once moved.
                self.remembered.clear();
            }
        }
        if self.settings.as_ref().is_some_and(|p| !p.is_open()) {
            self.settings = None;
        }
        if let Some(popout) = &self.settings {
            popout.show(ctx, shared.palette);
        }

        self.follow_window_level(ctx, shared.on_top());
        // Each window's place, into the preferences when it has moved; the
        // main window saves them.
        let positions = self.positions();
        if !positions.is_empty() {
            let mut prefs = shared.prefs.lock().unwrap_or_else(|e| e.into_inner());
            if prefs.remember_positions {
                for (key, at) in positions {
                    if self.remembered.get(key) != Some(&at) {
                        self.remembered.insert(key, at);
                        prefs.positions.insert(key.to_string(), at);
                    }
                }
            }
        }
    }

    /// The few-seconds status beside the screenshot button, if any.
    pub fn screenshot_status(&self) -> Option<&str> {
        self.status
            .as_ref()
            .filter(|(_, at)| at.elapsed() < Duration::from_millis(SCREENSHOT_STATUS_MS))
            .map(|(text, _)| text.as_str())
    }

    /// Show the saved picture in Explorer, selected.
    pub fn open_screenshot_folder(&self) {
        if let Some(path) = self.last_screenshot.as_ref().filter(|p| p.exists()) {
            super::admin::reveal_in_explorer(path);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use rochviewer::row::{Extra, Side as RowSide, Value};
    use std::sync::atomic::{AtomicUsize, Ordering};

    fn backend(rows: Vec<Row>) -> Arc<Backend> {
        Arc::new(Backend { platform: Platform::Unsupported, rows, unavailable: EM_DASH })
    }

    #[test]
    fn advanced_opens_with_the_slot_names_the_main_window_knows() {
        // The Python passed its own channel headings in when it opened the
        // window; the port started at A1/B1 and renamed them later.
        let ctx = egui::Context::default();
        let backend = backend(vec![Row::new("tCL", "Primary", "Timings", "Left").with_value("38")]);
        let palette = Palette::new(true);
        let mut settings = Settings::default();
        let spd = SpdSource::with(Vec::new(), HashMap::new());
        let labels = BTreeMap::from([("a".to_string(), "A2".to_string()), ("b".to_string(), "B2".to_string())]);
        let values = Values::default();
        let tab_names: Vec<String> = vec!["Summary".into(), "Timings".into()];
        let prefs = Arc::new(Mutex::new(Prefs::default()));
        let revision = Arc::new(AtomicU64::new(0));
        let mut shared = Shared {
            backend: &backend,
            values: &values,
            palette: &palette,
            settings: &mut settings,
            tab_names: &tab_names,
            current_tab: "Summary",
            spd: &spd,
            channel_labels: &labels,
            prefs: &prefs,
            prefs_revision: &revision,
        };
        let mut popouts = Popouts::new();
        popouts.open_advanced(&ctx, &mut shared);
        let window = popouts.advanced.as_ref().expect("opened").lock();
        assert_eq!(*window.channel_labels.lock().unwrap(), ("A2".to_string(), "B2".to_string()));
    }

    #[test]
    fn a_pop_out_list_is_cut_at_its_edge_not_faded() {
        // CTkScrollableFrame clips its last row; egui's default fade drew it
        // as a washed-out ghost above the footer.
        let style = PopoutStyle::new(&Palette::new(false));
        super::super::ui_tests::with_ui(|ui| {
            style.apply_scrollbar(ui);
            assert_eq!(ui.spacing().scroll.fade.strength, 0.0);
            assert!(!ui.spacing().scroll.floating);
        });
    }

    #[test]
    fn a_pop_out_title_bar_takes_the_apps_mode_and_keeps_it() {
        // A Light pop-out wore the system's black caption: winit's SetTheme
        // never reached it. The bar is set the way customtkinter set it, on
        // our own window found by its title, and set again if it drifts.
        use windows::Win32::UI::WindowsAndMessaging::{CreateWindowExW, DestroyWindow, WS_EX_TOOLWINDOW, WS_OVERLAPPEDWINDOW, WS_VISIBLE};
        use windows::core::w;
        let title = "RochViewer title bar test";
        // SAFETY: a plain STATIC window, off screen, destroyed below.
        let hwnd = unsafe {
            CreateWindowExW(WS_EX_TOOLWINDOW, w!("STATIC"), w!("RochViewer title bar test"), WS_OVERLAPPEDWINDOW | WS_VISIBLE, -3000, -3000, 200, 100, None, None, None, None)
        }
        .expect("a test window");
        assert_eq!(own_window_titled(title), Some(hwnd));
        assert_eq!(own_window_titled("RochViewer title bar"), None, "the whole title, not a prefix");
        for dark in [true, false, true] {
            sync_title_bar(hwnd, dark);
            assert_eq!(title_bar_dark(hwnd), Some(dark));
            assert!(!sync_title_bar(hwnd, dark), "already in that mode: nothing to change");
        }
        // SAFETY: the window made above, on this thread.
        let _ = unsafe { DestroyWindow(hwnd) };
    }

    #[test]
    fn maximizing_the_advanced_window_keeps_its_width() {
        // Tk's resizable(False, True) left the maximize button working, and
        // it took the window to the screen's height at its fixed width. The
        // port had switched the button off.
        let builder = advanced_window::viewport_builder(None);
        assert_eq!(builder.maximize_button, Some(true));
        assert_eq!(builder.max_inner_size.map(|s| s.x), Some(advanced_window::WINDOW_WIDTH));

        use windows::Win32::Foundation::{LPARAM, POINT, WPARAM};
        use windows::Win32::UI::WindowsAndMessaging::{CreateWindowExW, DestroyWindow, MINMAXINFO, SendMessageW, WM_GETMINMAXINFO, WS_EX_TOOLWINDOW, WS_OVERLAPPEDWINDOW};
        use windows::core::w;
        // SAFETY: a plain STATIC window, never shown, destroyed below.
        let hwnd = unsafe { CreateWindowExW(WS_EX_TOOLWINDOW, w!("STATIC"), w!("cap test"), WS_OVERLAPPEDWINDOW, 0, 0, 200, 100, None, None, None, None) }
            .expect("a test window");
        cap_maximized_width(hwnd);
        cap_maximized_width(hwnd); // a second call does not wrap it twice
        let ask = |track: i32| {
            let mut info = MINMAXINFO { ptMaxSize: POINT { x: 2576, y: 1456 }, ptMaxTrackSize: POINT { x: track, y: 99999 }, ..Default::default() };
            // SAFETY: the MINMAXINFO outlives the synchronous call.
            unsafe { SendMessageW(hwnd, WM_GETMINMAXINFO, Some(WPARAM(0)), Some(LPARAM(&mut info as *mut MINMAXINFO as isize))) };
            info.ptMaxSize
        };
        assert_eq!(ask(576), POINT { x: 576, y: 1456 }, "maximized at the capped width, full height");
        assert_eq!(ask(9999), POINT { x: 2576, y: 1456 }, "no cap narrower than the screen: left alone");
        // SAFETY: the window made above, on this thread.
        let _ = unsafe { DestroyWindow(hwnd) };
        assert!(PREVIOUS_PROCS.lock().unwrap().iter().all(|(h, _)| *h != hwnd.0 as isize), "forgotten once destroyed");
    }

    /// The stand-in reader the Python tests gave the builder.
    fn named() -> RowRead {
        Arc::new(|row, _| format!("<{}>", row.name))
    }

    fn row(name: &str, tab: &str, category: &str) -> Row {
        Row::new(name, category, tab, "Left")
    }

    fn names(entries: &[Entry]) -> Vec<String> {
        entries.iter().map(|e| e.name.clone()).collect()
    }

    // -- advanced_entries (test_advanced_window.py EntryListTest) -----------

    #[test]
    fn it_covers_the_reading_tabs() {
        // Voltages included, so the search and the dump cover the rails too.
        // SPD is added on its own, from the modules the SPD page reads.
        assert_eq!(ADVANCED_TABS, ["System Info", "Timings", "Training", "IMC", "RTL", "Voltages"]);
    }

    #[test]
    fn rows_are_grouped_by_tab_in_tab_order() {
        let entries = advanced_entries(
            &backend(vec![
                row("tCL", "Timings", "Primary"),
                row("CPU", "System Info", "General"),
                row("RTL MC0 CHA R0", "RTL", "RTL CHA MC0"),
                row("CMD SComp", "Training", "CMD"),
                row("DLL BWSEL", "IMC", "Misc Additional"),
            ]),
            named(),
        );
        let tabs: Vec<&str> = entries.iter().map(|e| e.tab.as_str()).collect();
        assert_eq!(tabs, ["System Info", "Timings", "Training", "IMC", "RTL"]);
    }

    #[test]
    fn spacer_rows_are_left_out() {
        // The tables use blank rows to separate blocks. They are layout, and
        // a searchable list has nothing to show for them.
        let entries = advanced_entries(
            &backend(vec![row("tCL", "Timings", "Primary"), row("", "Timings", "Primary"), row("   ", "Timings", "Primary")]),
            named(),
        );
        assert_eq!(names(&entries), ["tCL"]);
    }

    #[test]
    fn rows_hidden_from_the_tabs_stay_hidden_here() {
        let entries = advanced_entries(
            &backend(vec![
                row("tCL", "Timings", "Primary"),
                row("Secret", "Timings", "Primary").with_extra("diagnostic", Extra::Bool(true)),
            ]),
            named(),
        );
        assert_eq!(names(&entries), ["tCL"]);
    }

    #[test]
    fn rows_from_other_tabs_are_left_out() {
        let entries = advanced_entries(
            &backend(vec![row("tCL", "Timings", "Primary"), row("RTL", "Summary", "General"), row("VDD", "Sensors", "Rails")]),
            named(),
        );
        assert_eq!(names(&entries), ["tCL"]);
    }

    #[test]
    fn opted_in_diagnostics_are_available_for_dump() {
        let entries = advanced_entries(
            &backend(vec![
                row("MR1 raw", "Training", "RON diagnostics")
                    .with_extra("diagnostic", Extra::Bool(true))
                    .with_extra("advanced_only", Extra::Bool(true)),
                row("Hidden", "Training", "").with_extra("diagnostic", Extra::Bool(true)),
            ]),
            named(),
        );
        assert_eq!(names(&entries), ["MR1 raw"]);
        assert_eq!(entries[0].reading(), Ok(Reading::single("<MR1 raw>")));
    }

    #[test]
    fn each_row_reads_its_own_timing() {
        // The readers are built in a loop; without per-row binding every row
        // would report the last timing's value.
        let entries = advanced_entries(&backend(vec![row("tCL", "Timings", "Primary"), row("tRCD", "Timings", "Primary")]), named());
        let read: Vec<_> = entries.iter().map(|e| e.reading().unwrap()).collect();
        assert_eq!(read, [Reading::single("<tCL>"), Reading::single("<tRCD>")]);
    }

    #[test]
    fn a_dual_row_reads_both_channels() {
        let dual = row("tWPRE", "Timings", "Primary")
            .with_sides(RowSide::value(Value::text("2")), RowSide::value(Value::text("1")));
        let entries = advanced_entries(&backend(vec![dual]), backend_row_read(&backend(Vec::new())));
        assert!(entries[0].paired);
        assert_eq!(entries[0].reading(), Ok(Reading::pair("2", "1")));
    }

    #[test]
    fn the_real_table_produces_rows() {
        let backend = Arc::new(rochviewer::timings::load_timing_backend(Platform::Lga1700Ddr5));
        let entries = advanced_entries(&backend, named());
        assert!(!entries.is_empty());
        assert!(entries.iter().all(|e| !e.name.trim().is_empty()));
    }

    // -- labels (LabelTest) ----------------------------------------------------

    #[test]
    fn the_tab_label_rides_with_the_entry() {
        let entries = advanced_entries(
            &backend(vec![
                row("QCLK Ratio", "System Info", "Clocks").with_display_name("QCLK Reference"),
                row("tCL", "Timings", "Primary"),
            ]),
            named(),
        );
        let labels: Vec<&str> = entries.iter().map(|e| e.label.as_str()).collect();
        assert_eq!(labels, ["QCLK Reference", "tCL"]);
    }

    // -- SPD entries (SpdEntryTest) -------------------------------------------------

    fn module(slot: &str, part: &str) -> SpdRecord {
        let mut record = SpdRecord::default();
        record.set("slot", slot);
        record.set("part_number", part);
        record
    }

    fn spd_entries(modules: Vec<SpdRecord>, channels: (&str, &str)) -> Vec<Entry> {
        let source = SpdSource::with(modules, HashMap::from([("system_capacity".to_string(), "32 GB".to_string())]));
        advanced_spd_entries(&source, SPD_FIELDS, SPD_PROFILE_FIELDS, channel_labels(channels.0, channels.1))
    }

    fn by_label(entries: &[Entry]) -> HashMap<(String, String), Entry> {
        let mut map = HashMap::new();
        for entry in entries {
            assert_eq!(entry.tab, "SPD");
            map.entry((entry.category.clone(), entry.label.clone())).or_insert_with(|| entry.clone());
        }
        map
    }

    fn read(map: &HashMap<(String, String), Entry>, category: &str, label: &str) -> Reading {
        map[&(category.to_string(), label.to_string())].reading().unwrap()
    }

    #[test]
    fn a_module_the_headings_do_not_name_keeps_a_column() {
        // One slot matches and the other is named differently: the second
        // module takes the free column rather than being left out.
        let map = by_label(&spd_entries(vec![module("A1", "KIT-A"), module("B1", "KIT-B")], ("A1", "B2")));
        assert_eq!(read(&map, "Identity", "Part Number"), Reading::pair("KIT-A", "KIT-B"));
    }

    #[test]
    fn each_module_takes_its_slots_column() {
        let profile = |cl: i64| {
            let mut p = rochviewer::memory::spd_profiles::SpdProfile::new();
            p.insert("name", SpdValue::text("XMP-8000"));
            p.insert("cl", SpdValue::Int(cl));
            p
        };
        let mut b = module("B1", "KIT-B");
        b.set("address", 0x52i64);
        b.profiles = vec![profile(38)];
        let mut a = module("A1", "KIT-A");
        a.set("address", 0x50i64);
        a.profiles = vec![profile(40)];
        let map = by_label(&spd_entries(vec![b, a], ("A1", "B1")));
        assert_eq!(read(&map, "Identity", "Part Number"), Reading::pair("KIT-A", "KIT-B"));
        assert_eq!(read(&map, "Identity", "SPD Address"), Reading::pair("0x50", "0x52"));
        assert_eq!(read(&map, "Module", "Capacity"), Reading::pair("32 GB", "32 GB"));
        assert_eq!(read(&map, "Profile 1", "Profile"), Reading::pair("XMP-8000", "XMP-8000"));
        assert_eq!(read(&map, "Profile 1", "CAS Latency"), Reading::pair("40", "38"));
        assert_eq!(read(&map, "Profile 2", "Profile"), Reading::pair("—", "—"));
    }

    #[test]
    fn before_the_read_every_value_is_a_dash() {
        for entry in spd_entries(Vec::new(), ("A1", "B1")) {
            assert_eq!(entry.reading(), Ok(Reading::pair("—", "—")));
        }
    }

    #[test]
    fn every_dump_name_is_its_own() {
        let entries = spd_entries(Vec::new(), ("A1", "B1"));
        let unique: HashSet<&str> = entries.iter().map(|e| e.name.as_str()).collect();
        assert_eq!(unique.len(), entries.len());
    }

    #[test]
    fn ddr5_platforms_list_the_ddr5_page() {
        assert_eq!(spd_tab_fields(Platform::Am5).0.len(), SPD_DDR5_FIELDS.len());
        assert_eq!(spd_tab_fields(Platform::Lga1700Ddr5).1.len(), SPD_DDR5_PROFILE_FIELDS.len());
        assert_eq!(spd_tab_fields(Platform::Lga1700Ddr4).0.len(), SPD_FIELDS.len());
    }

    // -- sensor groups ---------------------------------------------------------

    fn sensor(name: &str, category: &str) -> Row {
        Row::new(name, category, "Sensors", "Left")
    }

    #[test]
    fn graphics_not_polled_by_telemetry() {
        // test_tachyon_readings.py
        let groups = sensor_groups(&backend(vec![sensor("GPU Clock", "Graphics"), sensor("VDDIO", "Voltages")]), named());
        let titles: Vec<&str> = groups.iter().map(|(t, _)| t.as_str()).collect();
        assert_eq!(titles, ["Voltages"]);
    }

    #[test]
    fn groups_come_out_in_the_window_order() {
        let groups = sensor_groups(
            &backend(vec![
                sensor("WHEA Errors", "Errors"),
                sensor("Board Only", "Board"),
                sensor("+12V", "Voltages"),
                sensor("Bus Clock", "Clocks"),
                sensor("CPU 0", "Clocks").with_extra(PARENT_ROW_KEY, Extra::Str("Core Effective Clock".into())),
            ]),
            named(),
        );
        let titles: Vec<&str> = groups.iter().map(|(t, _)| t.as_str()).collect();
        // A group the order does not name keeps its place, at the end.
        assert_eq!(titles, ["Clocks", "Voltages", "Errors", "Board"]);
        assert_eq!(groups[0].1[1].parent.as_deref(), Some("Core Effective Clock"));
        assert_eq!((groups[0].1[0].read)(), "<Bus Clock>");
    }

    fn fan_rows() -> Vec<Row> {
        vec![
            sensor("CPU Fan", "Fans").with_extra("hide_when_blank", Extra::Bool(true)),
            sensor("System Fan 1", "Fans").with_display_name("Sys Fan 1").with_extra("hide_when_blank", Extra::Bool(true)),
            // Not marked: a blank row stays, as it always has.
            sensor("VTT", "Voltages"),
        ]
    }

    #[test]
    fn only_marked_rows_wait_for_a_reading() {
        // test_x870_board_monitor.py HiddenWhenBlankTest
        assert_eq!(
            sensor_reveal_keys(&fan_rows()),
            HashSet::from([StatKey::sensor("Fans", "CPU Fan"), StatKey::sensor("Fans", "Sys Fan 1")])
        );
    }

    #[test]
    fn every_row_is_still_offered_to_the_window() {
        // Whether a marked row shows is the window's call, made from its own
        // readings; the groups hand it every row and read nothing to decide.
        let reads = Arc::new(AtomicUsize::new(0));
        let counted = reads.clone();
        let read: RowRead = Arc::new(move |row, _| {
            counted.fetch_add(1, Ordering::SeqCst);
            row.name.clone()
        });
        let groups: HashMap<String, Vec<SensorReading>> = sensor_groups(&backend(fan_rows()), read).into_iter().collect();
        let labels: Vec<&str> = groups["Fans"].iter().map(|r| r.label.as_str()).collect();
        assert_eq!(labels, ["CPU Fan", "Sys Fan 1"]);
        assert_eq!(reads.load(Ordering::SeqCst), 0);
    }

    // -- slot labels -------------------------------------------------------------

    #[test]
    fn split_channel_and_dimm_labels() {
        // test_tachyon_readings.py
        use rochviewer::memory::dimm_inventory::parse_slot;
        assert_eq!(parse_slot("DIMM 1", "P0 CHANNEL A").as_deref(), Some("A1"));
        assert_eq!(parse_slot("DIMM 1", "P0 CHANNEL B").as_deref(), Some("B1"));
        assert_eq!(parse_slot("DIMM 1", "Unknown"), None);
    }

    #[test]
    fn a_channel_with_one_module_is_named_for_its_slot() {
        let module = |slot: &str| DimmModule { slot: Some(slot.into()), ..DimmModule::default() };
        let labels = channel_slot_labels(&[module("A2"), module("B1"), module("B2")]);
        assert_eq!(labels, HashMap::from([("a".to_string(), "A2".to_string())]));
        assert_eq!(channel_headers(&labels, "A1", "B1"), ("A2".to_string(), "B1".to_string()));
    }

    // -- placement ---------------------------------------------------------------

    fn main_rect(x: f32, y: f32, w: f32) -> Rect {
        Rect::from_min_size(egui::pos2(x, y), egui::vec2(w, 750.0))
    }

    #[test]
    fn pop_outs_open_beside_the_main_window() {
        let screen = egui::vec2(2560.0, 1440.0);
        assert_eq!(adjacent_window_position(Side::Right, 560.0, 800.0, main_rect(700.0, 40.0, 700.0), screen, 8.0), (1408.0, 40.0));
        assert_eq!(adjacent_window_position(Side::Left, 600.0, 800.0, main_rect(700.0, 40.0, 700.0), screen, 8.0), (92.0, 40.0));
    }

    #[test]
    fn a_side_without_room_falls_back_to_the_other() {
        let screen = egui::vec2(1920.0, 1080.0);
        // No room on the left: Telemetry goes right.
        assert_eq!(adjacent_window_position(Side::Left, 600.0, 800.0, main_rect(100.0, 40.0, 700.0), screen, 8.0), (808.0, 40.0));
        // No room on the right: Advanced goes left.
        assert_eq!(adjacent_window_position(Side::Right, 560.0, 800.0, main_rect(1200.0, 40.0, 700.0), screen, 8.0), (632.0, 40.0));
    }

    #[test]
    fn a_pop_out_is_kept_on_screen() {
        let screen = egui::vec2(1280.0, 720.0);
        // Neither side has room: clamped to the screen, and lifted so its
        // foot is on it too.
        let (x, y) = adjacent_window_position(Side::Right, 560.0, 800.0, main_rect(300.0, 300.0, 700.0), screen, 8.0);
        assert_eq!((x, y), (0.0, 0.0));
        let (x, _) = adjacent_window_position(Side::Left, 600.0, 400.0, main_rect(-50.0, 10.0, 1300.0), screen, 8.0);
        assert_eq!(x, 680.0);
    }

    // -- the screenshot status -------------------------------------------------------

    #[test]
    fn the_status_shows_for_four_seconds() {
        let mut popouts = Popouts::new();
        assert_eq!(popouts.screenshot_status(), None);
        popouts.show_screenshot_status("Saved · copied");
        assert_eq!(popouts.screenshot_status(), Some("Saved · copied"));
        popouts.status = Some(("Saved".into(), Instant::now() - Duration::from_millis(SCREENSHOT_STATUS_MS + 1)));
        assert_eq!(popouts.screenshot_status(), None);
    }

    #[test]
    fn a_capture_is_saved_and_reported() {
        // The Python's live test drew the real window; here the frame egui
        // would hand back is built by hand and saved the same way.
        let mut popouts = Popouts::new();
        let image = egui::ColorImage::filled([4, 3], Color32::from_rgb(10, 20, 30));
        let folder = std::env::temp_dir().join(format!("rochviewer-shot-{}", std::process::id()));
        let (w, h, pixels) = screenshot::image_bgra(&image, None);
        let path = screenshot::save_png(&folder, "x.png", w, h, &pixels).unwrap();
        let decoded = super::super::asset_path::decode_png(&std::fs::read(&path).unwrap()).unwrap();
        assert_eq!((decoded.width, decoded.height), (4, 3));
        assert_eq!(decoded.pixels[..3], [10, 20, 30]);
        let _ = std::fs::remove_dir_all(&folder);
        popouts.show_screenshot_status("Screenshot failed");
        assert_eq!(popouts.screenshot_status(), Some("Screenshot failed"));
    }
}
