//! One searchable window holding every row the tabs show.
//!
//! The tabs are organised for reading a timing set at a glance, which is the
//! wrong shape when you know the name of the one field you want. This window
//! is the other half: everything in one list, filtered as you type.
//!
//! The Python built the rows once as widgets and hid rather than destroyed
//! them when the filter changed, because a keystroke that rebuilt three
//! hundred label pairs was visibly slow. egui draws every frame from the
//! list, so the same idea is kept as data: the rows are worked out once (their
//! group, their search text, whether they pair) and the filter only changes
//! which of them are drawn.
//!
//! Each row lays its name and value out against the full width of its own
//! line rather than a grid shared by the whole list. A shared grid sized each
//! column to its widest cell anywhere in the list, so the motherboard string
//! -- 378px of "ASUSTeK COMPUTER INC. ROG MAXIMUS Z790 APEX (Rev 1.xx)" --
//! set the width of the value column for all 235 rows and left the name
//! column 5px to live in. Per-row layout keeps a long value's cost to its own
//! row, and gives the band something that spans the full width to paint on.
//!
//! The values are read on a worker rather than in the frame: a register, the
//! SMU or WMI can each take milliseconds, and the window should never wait.

use std::collections::HashSet;
use std::path::PathBuf;
use std::sync::mpsc::{self, RecvTimeoutError, Sender};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use eframe::egui::{self, Align, Color32, CornerRadius, FontId, Rect, Sense, Ui, UiBuilder, pos2, vec2};

use super::popouts::{self, ChannelLabels, PopoutStyle};
use super::theme::Palette;

pub const REFRESH_MS: u64 = 1000;

/// The dump lands on the desktop under one name, overwritten each time. A
/// timestamped name would leave a pile of files behind after a tuning
/// session, and the thing people do with this is paste the latest one.
pub const DUMP_NAME: &str = "RochViewer.txt";

/// Two spaces in, the name padded to here, then " : ", which puts the
/// separator at column 38 -- measured off the reference tool's own dump
/// rather than guessed, so the two files line up when read side by side. A
/// longer name pushes its own colon right rather than being truncated, which
/// is what that dump does and what keeps a long register name readable.
pub const DUMP_NAME_WIDTH: usize = 36;
#[cfg_attr(not(test), allow(dead_code))]
pub const DUMP_SEPARATOR_COLUMN: usize = 38;

pub const ROW_HEIGHT: f32 = 19.0;

/// The pitch of a wrapped value's lines: Consolas at 12px, as Tk spaced it.
const WRAPPED_LINE_HEIGHT: f32 = 14.0;

/// Each channel column, sized to the widest paired reading the tabs produce:
/// 126px of "120 ns / 98 ns" on the tRFC pair, with "RZQ/6 (40)" close
/// behind. It was 58, which reserved less than half of that, so two RTT
/// values ran into each other with four pixels between them.
pub const CHANNEL_WIDTH: f32 = 130.0;

/// Sized to the widest row the tabs actually produce rather than to the
/// widest name plus the widest value, which never share a row: the longest
/// names are on Misc against Enabled/Disabled, and the longest values are on
/// System Info against names like OS and CPU. 520 fit every row but the OS
/// string, "Microsoft Windows 11 Professional (x64)", which wrapped to a
/// second line; the 40px added here all go to the value, where that string
/// needed them.
pub const WINDOW_WIDTH: f32 = 560.0;
pub const WINDOW_HEIGHT: f32 = 800.0;

/// What a row keeps for its name and the paddings either side of the value.
/// Every name fits in it: it is what the two channel columns left of the
/// 520px the window was before.
pub const NAME_RESERVE: f32 = 260.0;

/// The width is fixed, so the space a value gets is known and a long one can
/// be wrapped instead of left to crowd its name. A single-channel row spans
/// both channel columns and whatever else the name does not keep.
pub const VALUE_WRAP: f32 = WINDOW_WIDTH - NAME_RESERVE;

/// A paired row's two values each get one channel column, less the 4px
/// either side of the cell.
pub const PAIRED_VALUE_WRAP: f32 = CHANNEL_WIDTH - 8.0;

// The header strip: the search box and the Dump button beside it.
const HEADER_PAD: f32 = 6.0;
/// The strip is as tall as the "Search:" label in it; the entry and the
/// button are 24px, centred on it.
const HEADER_HEIGHT: f32 = 28.0;
const CONTROL_HEIGHT: f32 = 24.0;
const SEARCH_LABEL_WIDTH: f32 = 54.0;
const DUMP_WIDTH: f32 = 62.0;
const COUNT_HEIGHT: f32 = 28.0;
const BODY_PAD: f32 = 4.0;

/// What one row read: one displayed value, or both channels of a row that
/// reads two. `None` is a reading with nothing in it (Python's `None`).
#[derive(Clone, Debug, PartialEq)]
pub enum Reading {
    Single(Option<String>),
    Pair(Option<String>, Option<String>),
}

#[cfg_attr(not(test), allow(dead_code))]
impl Reading {
    pub fn single(text: impl Into<String>) -> Self {
        Reading::Single(Some(text.into()))
    }

    pub fn pair(a: impl Into<String>, b: impl Into<String>) -> Self {
        Reading::Pair(Some(a.into()), Some(b.into()))
    }
}

/// Read one row. An `Err` (or a panic) is a row that raised.
pub type EntryRead = Arc<dyn Fn() -> Result<Reading, String> + Send + Sync>;

/// One row of the list: `(tab, category, name, read[, label])` in the Python.
///
/// `paired` says whether the row reads two channels. The Python found that
/// out by calling the reader once while building; here every feeder knows it
/// from the row, so the window never reads on the frame to decide a layout.
#[derive(Clone)]
pub struct Entry {
    pub tab: String,
    pub category: String,
    /// The row's own name: what the dump writes.
    pub name: String,
    /// What the tab calls the row, shown in place of the name; a search
    /// matches either.
    pub label: String,
    pub paired: bool,
    pub read: EntryRead,
}

impl Entry {
    pub fn new(
        tab: impl Into<String>,
        category: impl Into<String>,
        name: impl Into<String>,
        paired: bool,
        read: impl Fn() -> Result<Reading, String> + Send + Sync + 'static,
    ) -> Self {
        let name = name.into();
        Entry { tab: tab.into(), category: category.into(), label: name.clone(), name, paired, read: Arc::new(read) }
    }

    pub fn with_label(mut self, label: impl Into<String>) -> Self {
        let label = label.into();
        if !label.is_empty() {
            self.label = label;
        }
        self
    }

    /// Call the reader, a panic counting as a raise.
    pub fn reading(&self) -> Result<Reading, String> {
        std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| (self.read)()))
            .unwrap_or_else(|_| Err("the reader panicked".to_string()))
    }
}

/// The user's Desktop, asked of Windows rather than assumed.
///
/// ~/Desktop is wrong on any machine whose desktop is redirected -- OneDrive
/// does it by default -- and a dump written there is one the user cannot
/// find. SHGetFolderPathW answers where it actually is; the expanded home is
/// the fallback for when that call is unavailable.
pub fn desktop_directory() -> PathBuf {
    use windows::Win32::UI::Shell::SHGetFolderPathW;

    let mut buffer = [0u16; 260];
    // CSIDL_DESKTOPDIRECTORY, current user, no creation.
    // SAFETY: a MAX_PATH buffer, as the call requires.
    match unsafe { SHGetFolderPathW(None, 0x0010, None, 0, &mut buffer) } {
        Ok(()) => {
            let end = buffer.iter().position(|&c| c == 0).unwrap_or(buffer.len());
            if end > 0 {
                return PathBuf::from(String::from_utf16_lossy(&buffer[..end]));
            }
        }
        Err(error) => eprintln!("Could not ask Windows for the desktop: {error}"),
    }
    std::env::var_os("USERPROFILE").map(PathBuf::from).unwrap_or_default().join("Desktop")
}

fn na_if_blank(part: &Option<String>) -> String {
    match part.as_deref() {
        None | Some("") => "N/A".to_string(),
        Some(text) => text.to_string(),
    }
}

/// Render `entries` as the aligned "name : value" text of the dump.
///
/// Pure, so the layout can be checked without a window or a machine to read.
/// A row that raises is written as N/A rather than dropped, because a dump
/// that silently omits what it could not read is a dump you cannot trust to
/// be complete.
pub fn format_dump(entries: &[Entry]) -> String {
    let mut lines = Vec::with_capacity(entries.len());
    // The dump keeps each row's own name, lined up with the reference tool's
    // dump; the label a tab shows it under is the window's business.
    for entry in entries {
        let value = match entry.reading() {
            Err(_) => "N/A".to_string(),
            // Both channels, in the order the tab shows them.
            Ok(Reading::Pair(a, b)) => format!("{} | {}", na_if_blank(&a), na_if_blank(&b)),
            Ok(Reading::Single(value)) => na_if_blank(&value),
        };
        lines.push(format!("  {:<width$} : {}", entry.name, value, width = DUMP_NAME_WIDTH));
    }
    lines.join("\n") + "\n"
}

/// What a row shows for one reading: the channel-A (or only) text and the
/// channel-B text.
///
/// A pair means the row reads two channels; anything else is one value,
/// which sits in the channel-A column so it lines up with the readings above
/// and below it. Both columns always, even where the channels agree.
/// Blanking the matching side saved width but emptied B1 on almost every
/// row, since most settings are the same on both modules -- and an empty
/// column reads as no reading rather than as agreement.
pub fn shown_text(reading: Result<Reading, String>) -> (String, String) {
    match reading {
        Err(_) => ("N/A".to_string(), String::new()),
        Ok(Reading::Pair(a, b)) => (a.unwrap_or_default(), b.unwrap_or_default()),
        Ok(Reading::Single(value)) => (value.unwrap_or_default(), String::new()),
    }
}

/// Every word must appear somewhere in the row, so "skew vref" narrows
/// rather than widening the way a single substring would. Folded here as well
/// as on the way in: the haystack is built lowercase, and a filter that
/// skipped the fold would silently match nothing.
pub fn matches(filter: &str, haystack: &str) -> bool {
    filter.to_lowercase().split_whitespace().all(|word| haystack.contains(word))
}

/// One row high, or two when the value has to wrap to fit.
///
/// Measured rather than guessed from the character count: the row names are
/// monospaced but the values are not all the same width, and a value that
/// wrapped in a row kept at one line would lose its second half with nothing
/// on screen to say so. `None` is a value that could not be measured.
pub fn value_height(measured: Option<f32>) -> f32 {
    match measured {
        Some(width) if width > VALUE_WRAP => ROW_HEIGHT * 2.0,
        _ => ROW_HEIGHT,
    }
}

/// The background for one line of the list, headings included.
pub fn band(palette: &Palette, line: usize) -> Color32 {
    if line % 2 == 1 { palette.band } else { palette.bg }
}

/// The count line under the search box.
pub fn count_text(visible: usize, total: usize) -> String {
    if visible == total { format!("{total} rows") } else { format!("{visible} of {total} rows") }
}

type Group = (String, String);

/// What a row shows: the channel-A (or only) text and the channel-B text.
type Shown = (String, String);

struct RowInfo {
    group: Group,
    haystack: String,
}

/// Every row from the requested tabs, searchable and refreshing live.
pub struct AdvancedWindow {
    pub open: bool,
    pub palette: Palette,
    entries: Arc<Vec<Entry>>,
    /// The two channel headings, filled in off the UI thread once the
    /// installed modules are known.
    channel_labels: ChannelLabels,
    rows: Vec<RowInfo>,
    /// Which sections have a row that reads two channels. A section of
    /// single-channel rows carrying A1/B1 labels names two columns that
    /// nothing beneath it fills.
    paired_groups: HashSet<Group>,
    search: String,
    filter: String,
    visible: Vec<usize>,
    count: String,
    /// What each row shows, by entry index; written by the worker.
    values: Arc<Mutex<Vec<Option<Shown>>>>,
    /// Which rows the worker reads: only what is on screen, so a filtered
    /// list costs a filtered read.
    wanted: Arc<Mutex<Vec<usize>>>,
    wake: Option<Sender<()>>,
    dump_result: Arc<Mutex<Option<String>>>,
    focused: bool,
    refresh: Duration,
}

impl AdvancedWindow {
    pub fn new(entries: Vec<Entry>, channel_labels: ChannelLabels, palette: Palette) -> Self {
        let rows: Vec<RowInfo> = entries
            .iter()
            .map(|e| RowInfo {
                group: (e.tab.clone(), e.category.clone()),
                haystack: format!("{} {} {} {}", e.tab, e.category, e.name, e.label).to_lowercase(),
            })
            .collect();
        let paired_groups = entries.iter().filter(|e| e.paired).map(|e| (e.tab.clone(), e.category.clone())).collect();
        let count = entries.len();
        let mut window = AdvancedWindow {
            open: true,
            palette,
            entries: Arc::new(entries),
            channel_labels,
            rows,
            paired_groups,
            search: String::new(),
            filter: String::new(),
            visible: Vec::new(),
            count: String::new(),
            values: Arc::new(Mutex::new(vec![None; count])),
            wanted: Arc::new(Mutex::new(Vec::new())),
            wake: None,
            dump_result: Arc::new(Mutex::new(None)),
            focused: false,
            refresh: Duration::from_millis(REFRESH_MS),
        };
        window.apply_filter();
        window
    }

    /// The rows the current filter shows, by entry index.
    #[cfg_attr(not(test), allow(dead_code))]
    pub fn visible(&self) -> &[usize] {
        &self.visible
    }

    #[cfg_attr(not(test), allow(dead_code))]
    pub fn count(&self) -> &str {
        &self.count
    }

    /// Set the search text, as typing it would.
    #[cfg_attr(not(test), allow(dead_code))]
    pub fn set_search(&mut self, text: &str) {
        self.search = text.to_string();
        self.filter_changed();
    }

    fn filter_changed(&mut self) {
        let text = self.search.trim().to_lowercase();
        if text == self.filter {
            return;
        }
        self.filter = text;
        self.apply_filter();
    }

    fn apply_filter(&mut self) {
        self.visible = (0..self.rows.len()).filter(|&i| matches(&self.filter, &self.rows[i].haystack)).collect();
        self.count = count_text(self.visible.len(), self.rows.len());
        if let Ok(mut wanted) = self.wanted.lock() {
            wanted.clone_from(&self.visible);
        }
        if let Some(wake) = &self.wake {
            let _ = wake.send(());
        }
    }

    fn start_worker(&mut self, ctx: &egui::Context) {
        if self.wake.is_some() {
            return;
        }
        let (sender, receiver) = mpsc::channel::<()>();
        self.wake = Some(sender);
        let entries = self.entries.clone();
        let wanted = self.wanted.clone();
        let values = self.values.clone();
        let ctx = ctx.clone();
        let viewport = ctx.viewport_id();
        let refresh = self.refresh;
        let spawned = std::thread::Builder::new().name("advanced-reader".into()).spawn(move || {
            loop {
                let rows = wanted.lock().map(|w| w.clone()).unwrap_or_default();
                let mut changed = false;
                for index in rows {
                    let shown = shown_text(entries[index].reading());
                    let mut values = values.lock().unwrap_or_else(|e| e.into_inner());
                    if values[index].as_ref() != Some(&shown) {
                        values[index] = Some(shown);
                        changed = true;
                    }
                }
                if changed {
                    ctx.request_repaint_of(viewport);
                }
                match receiver.recv_timeout(refresh) {
                    Ok(()) | Err(RecvTimeoutError::Timeout) => {}
                    Err(RecvTimeoutError::Disconnected) => return,
                }
                // Collapse a burst of keystrokes into one pass.
                while receiver.try_recv().is_ok() {}
            }
        });
        if let Err(error) = spawned {
            eprintln!("Advanced window cannot read: {error}");
        }
    }

    /// Write every row to RochViewer.txt on the desktop.
    ///
    /// Reported in the count line under the search box rather than a
    /// dialog: the window stays usable, and the path is the one thing worth
    /// saying. Written off the frame, since it reads every row.
    fn dump(&self, ctx: &egui::Context) {
        let entries = self.entries.clone();
        let result = self.dump_result.clone();
        let ctx = ctx.clone();
        let viewport = ctx.viewport_id();
        let _ = std::thread::Builder::new().name("advanced-dump".into()).spawn(move || {
            let path = desktop_directory().join(DUMP_NAME);
            // The file is written with Windows line endings, as the Python
            // opened it with newline="\r\n".
            let text = format_dump(&entries).replace('\n', "\r\n");
            let message = match std::fs::write(&path, text) {
                Ok(()) => format!("Wrote {} rows to {}", entries.len(), path.display()),
                Err(error) => format!("Could not write {}: {error}", path.display()),
            };
            *result.lock().unwrap_or_else(|e| e.into_inner()) = Some(message);
            ctx.request_repaint_of(viewport);
        });
    }

    /// Draw the window. Called from the viewport's own frame.
    pub fn ui(&mut self, ui: &mut Ui) {
        let ctx = ui.ctx().clone();
        if ctx.input(|i| i.viewport().close_requested()) {
            self.open = false;
            return;
        }
        self.start_worker(&ctx);
        if let Some(message) = self.dump_result.lock().ok().and_then(|mut r| r.take()) {
            self.count = message;
        }
        let palette = self.palette;
        let style = PopoutStyle::new(&palette);
        egui::CentralPanel::default().frame(egui::Frame::new().fill(palette.bg)).show(ui, |ui| {
            let full = ui.max_rect();
            self.header(ui, &style, full);
            let count_rect = Rect::from_min_size(
                pos2(full.left() + 8.0, full.top() + HEADER_PAD + HEADER_HEIGHT + 4.0),
                vec2(full.width() - 16.0, COUNT_HEIGHT),
            );
            ui.painter().text(count_rect.left_center(), egui::Align2::LEFT_CENTER, &self.count, style.font.clone(), palette.text);
            let body = Rect::from_min_max(
                pos2(full.left() + BODY_PAD, count_rect.bottom() + 2.0),
                pos2(full.right() - BODY_PAD, full.bottom() - 6.0),
            );
            ui.scope_builder(UiBuilder::new().max_rect(body), |ui| self.body(ui, &style));
        });
    }

    fn header(&mut self, ui: &mut Ui, style: &PopoutStyle, full: Rect) {
        let palette = self.palette;
        let top = full.top() + HEADER_PAD;
        let left = full.left() + HEADER_PAD;
        let right = full.right() - HEADER_PAD;
        let label_rect = Rect::from_min_size(pos2(left, top), vec2(SEARCH_LABEL_WIDTH, HEADER_HEIGHT));
        ui.painter().text(label_rect.left_center(), egui::Align2::LEFT_CENTER, "Search:", style.font.clone(), palette.text);

        let control_top = top + (HEADER_HEIGHT - CONTROL_HEIGHT) / 2.0;
        let dump_rect = Rect::from_min_max(pos2(right - DUMP_WIDTH, control_top), pos2(right, control_top + CONTROL_HEIGHT));
        let search_rect =
            Rect::from_min_max(pos2(label_rect.right(), control_top), pos2(dump_rect.left() - 6.0, control_top + CONTROL_HEIGHT));
        let edit = egui::TextEdit::singleline(&mut self.search)
            .frame(egui::Frame::NONE.inner_margin(egui::Margin::symmetric(7, 4)))
            .font(style.font.clone())
            .text_color(palette.text)
            .align(egui::Align2::LEFT_CENTER)
            .desired_width(search_rect.width());
        let response = ui.put(search_rect, edit);
        if !self.focused {
            self.focused = true;
            response.request_focus();
        }
        // Escape clears rather than closing: the window is meant to stay open
        // while you look several things up in a row.
        // A key Windows synthesises from text (VK_PACKET: an on-screen
        // keyboard, an input tool) reaches winit as an Escape that carries a
        // character; only a bare Escape clears.
        let escape = ui.input(|i| {
            i.key_pressed(egui::Key::Escape) && !i.events.iter().any(|e| matches!(e, egui::Event::Text(_)))
        });
        if (response.has_focus() || response.lost_focus()) && escape {
            self.search.clear();
            response.request_focus();
        }
        if response.changed() || self.search.trim().to_lowercase() != self.filter {
            self.filter_changed();
        }

        // Beside the search rather than under it: the window is a long list
        // and a control at the bottom would be scrolled away from.
        let dump = ui.interact(dump_rect, ui.scope_id().with("dump"), Sense::click());
        let fill = if dump.hovered() { palette.tab_unselected_hover } else { palette.bg };
        ui.painter().rect_filled(dump_rect, CornerRadius::ZERO, fill);
        ui.painter().text(dump_rect.center(), egui::Align2::CENTER_CENTER, "Dump", style.font.clone(), palette.text);
        if dump.clicked() {
            self.dump(ui.ctx());
        }
    }

    fn body(&mut self, ui: &mut Ui, style: &PopoutStyle) {
        let palette = self.palette;
        style.apply_scrollbar(ui);
        let values = self.values.lock().map(|v| v.clone()).unwrap_or_default();
        egui::ScrollArea::vertical()
            .auto_shrink([false, false])
            // The bar keeps its place when everything fits, so a filter
            // never moves the value columns sideways.
            .scroll_bar_visibility(egui::scroll_area::ScrollBarVisibility::AlwaysVisible)
            .show(ui, |ui| {
            let width = ui.available_width();
            let mut line = 0usize;
            let mut group: Option<&Group> = None;
            for &index in &self.visible {
                let row = &self.rows[index];
                if group != Some(&row.group) {
                    group = Some(&row.group);
                    self.heading(ui, style, width, line, &row.group);
                    line += 1;
                }
                let entry = &self.entries[index];
                let (left, right) = values[index].clone().unwrap_or_default();
                draw_row(ui, style, &palette, width, line, &entry.label, entry.paired, &left, &right);
                line += 1;
            }
        });
    }

    fn heading(&self, ui: &mut Ui, style: &PopoutStyle, width: f32, line: usize, group: &Group) {
        let palette = self.palette;
        let (tab, category) = group;
        let title = if category.is_empty() { tab } else { category };
        // No colour of its own: a heading is a row of the table and takes the
        // band its position calls for, which is what the Timings and Training
        // tabs do. Given one, the alternation would visibly restart at every
        // section.
        let (rect, _) = ui.allocate_exact_size(vec2(width, ROW_HEIGHT), Sense::hover());
        if !ui.is_rect_visible(rect) {
            return;
        }
        let painter = ui.painter();
        painter.rect_filled(rect, CornerRadius::ZERO, band(&palette, line));
        let text = format!("{tab} — {title}").to_uppercase();
        painter.text(pos2(rect.left() + 6.0, rect.center().y), egui::Align2::LEFT_CENTER, text, style.bold.clone(), palette.value);
        // The channel names ride on the section heading rather than taking a
        // row of their own, which would break the band and cost a line per
        // section. Only where the section has a row to put under them.
        if self.paired_groups.contains(group) {
            let (a, b) = &*self.channel_labels.lock().unwrap_or_else(|e| e.into_inner());
            let a_right = rect.right() - CHANNEL_WIDTH - 4.0;
            let b_right = rect.right() - 8.0;
            painter.text(pos2(a_right, rect.center().y), egui::Align2::RIGHT_CENTER, a, style.bold.clone(), palette.text);
            painter.text(pos2(b_right, rect.center().y), egui::Align2::RIGHT_CENTER, b, style.bold.clone(), palette.text);
        }
    }
}

fn wrapped(ui: &Ui, text: &str, font: &FontId, color: Color32, wrap: f32) -> Arc<egui::Galley> {
    let mut job = egui::text::LayoutJob::simple(text.to_string(), font.clone(), color, wrap);
    job.halign = Align::RIGHT;
    // Wrapped lines sit as close as Tk set them.
    for section in &mut job.sections {
        section.format.line_height = Some(WRAPPED_LINE_HEIGHT);
    }
    ui.painter().layout_job(job)
}

#[allow(clippy::too_many_arguments)]
fn draw_row(ui: &mut Ui, style: &PopoutStyle, palette: &Palette, width: f32, line: usize, label: &str, paired: bool, left: &str, right: &str) {
    // A single-channel row spans both channel columns rather than leaving
    // the second empty: the long values are all single-channel -- the
    // motherboard string, the ECS and preamble text -- and they need the
    // width the second column would otherwise hold open. A paired row's two
    // values each wrap inside their own column: left to grow, the two copies
    // of "Timer Stops at 2048th clocks" took the name's width between them
    // and cut "DQS Interval Timer RT" short.
    let font = &style.font;
    let measured = ui.painter().layout_no_wrap(left.to_string(), font.clone(), palette.value).size().x;
    let (a, b) = if paired {
        (wrapped(ui, left, font, palette.value, PAIRED_VALUE_WRAP), Some(wrapped(ui, right, font, palette.value, PAIRED_VALUE_WRAP)))
    } else {
        (wrapped(ui, left, font, palette.value, VALUE_WRAP), None)
    };
    // A single value is given one row or two by measuring it; a paired row
    // keeps one row and grows to whatever its wrapped values need, as a Tk
    // label grows past the height it was given.
    let tallest = a.size().y.max(b.as_ref().map_or(0.0, |g| g.size().y));
    let height = if paired { ROW_HEIGHT.max(tallest.ceil()) } else { value_height(Some(measured)).max(tallest.ceil()) };
    let (rect, _) = ui.allocate_exact_size(vec2(width, height), Sense::hover());
    if !ui.is_rect_visible(rect) {
        return;
    }
    let painter = ui.painter();
    painter.rect_filled(rect, CornerRadius::ZERO, band(palette, line));
    // The name keeps the top line, which keeps every row at the same pitch.
    painter.text(pos2(rect.left() + 6.0, rect.top() + ROW_HEIGHT / 2.0), egui::Align2::LEFT_CENTER, label, font.clone(), palette.text);
    let place = |galley: Arc<egui::Galley>, right_edge: f32| {
        let y = if paired {
            rect.top() + (ROW_HEIGHT - WRAPPED_LINE_HEIGHT) / 2.0
        } else {
            rect.center().y - galley.size().y / 2.0
        };
        painter.galley(pos2(right_edge, y), galley, palette.value);
    };
    match b {
        Some(b) => {
            place(a, rect.right() - CHANNEL_WIDTH - 4.0);
            place(b, rect.right() - 8.0);
        }
        None => place(a, rect.right() - 8.0),
    }
}

/// The builder for the Advanced viewport, placed at `position`.
pub fn viewport_builder(position: Option<(f32, f32)>) -> egui::ViewportBuilder {
    // Width fixed, height not: the list is long and worth resizing
    // vertically, while the columns are laid out against a known width.
    let builder = egui::ViewportBuilder::default()
        .with_title("Advanced")
        .with_inner_size([WINDOW_WIDTH, WINDOW_HEIGHT])
        .with_min_inner_size([WINDOW_WIDTH, 200.0])
        .with_max_inner_size([WINDOW_WIDTH, f32::INFINITY])
        .with_maximize_button(false)
        .with_resizable(true);
    popouts::finish_builder(builder, position)
}

impl Drop for AdvancedWindow {
    fn drop(&mut self) {
        // Dropping the sender stops the worker at its next wake.
        self.wake = None;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn entry(name: &str, value: &str) -> Entry {
        let value = value.to_string();
        Entry::new("Timings", "Primary", name, false, move || Ok(Reading::single(value.clone())))
    }

    fn haystack_matches(filter: &str, haystack: &str) -> bool {
        matches(filter, haystack)
    }

    // -- the dump (test_advanced_dump.py) --------------------------------

    #[test]
    fn the_separator_sits_where_the_reference_dump_puts_it() {
        let text = format_dump(&[entry("tCL", "38"), entry("MOBO", "Micro-Star International Co., Ltd.")]);
        for line in text.lines() {
            assert_eq!(line.find(" : "), Some(DUMP_SEPARATOR_COLUMN), "{line}");
        }
    }

    #[test]
    fn a_long_name_pushes_its_own_separator() {
        // Truncating would lose which register a row is, which is the one
        // thing a dump has to keep.
        let name = "RXDQSCOMP_RXCODE_dllcomp_cmn_picoderxdqsnref";
        let text = format_dump(&[entry(name, "0")]);
        let line = text.lines().next().unwrap();
        assert!(line.contains(name));
        assert!(line.find(" : ").unwrap() > DUMP_SEPARATOR_COLUMN);
    }

    #[test]
    fn both_channels_are_written() {
        let row = Entry::new("Timings", "Primary", "tRCD", true, || Ok(Reading::pair("50", "50")));
        let text = format_dump(&[row]);
        assert!(text.lines().next().unwrap().ends_with("50 | 50"), "{text}");
    }

    #[test]
    fn a_row_that_cannot_be_read_is_kept_as_na() {
        // Dropping it would make the dump quietly incomplete, and a reader
        // comparing two dumps would see a row appear and disappear rather
        // than a value they could not read.
        let text = format_dump(&[
            Entry::new("Misc", "Features", "Broken", false, || Err("no driver".into())),
            Entry::new("Misc", "Features", "Panics", false, || panic!("no driver")),
            Entry::new("Timings", "Primary", "Blank", false, || Ok(Reading::Single(None))),
            entry("Empty", ""),
            Entry::new("Timings", "Primary", "Half", true, || Ok(Reading::Pair(Some("1".into()), None))),
        ]);
        for name in ["Broken", "Panics", "Blank", "Empty"] {
            assert!(text.contains(&format!("  {name:<36} : N/A\n")), "{name}: {text}");
        }
        assert!(text.contains(": 1 | N/A\n"));
    }

    #[test]
    fn every_row_is_written_once() {
        let rows: Vec<Entry> = (0..40).map(|i| entry(&format!("t{i}"), &i.to_string())).collect();
        assert_eq!(format_dump(&rows).lines().count(), 40);
    }

    #[test]
    fn the_text_ends_with_a_newline() {
        assert!(format_dump(&[entry("tCL", "38")]).ends_with('\n'));
    }

    #[test]
    fn the_file_is_named_for_the_app() {
        assert_eq!(DUMP_NAME, "RochViewer.txt");
    }

    // -- labels (test_advanced_window.py LabelTest) -----------------------

    #[test]
    fn the_dump_keeps_the_rows_own_name() {
        let row = Entry::new("System Info", "Clocks", "QCLK Ratio", false, || Ok(Reading::single("100.00 MHz")))
            .with_label("QCLK Reference");
        let text = format_dump(&[row]);
        assert!(text.contains("QCLK Ratio"));
        assert!(!text.contains("QCLK Reference"));
    }

    // -- search (test_advanced_window.py SearchTest) ----------------------

    #[test]
    fn an_empty_search_matches_everything() {
        assert!(haystack_matches("", "timings primary tcl"));
    }

    #[test]
    fn a_word_matches_anywhere_in_the_row() {
        assert!(haystack_matches("tcl", "timings primary tcl"));
        assert!(haystack_matches("primary", "timings primary tcl"));
        assert!(haystack_matches("timings", "timings primary tcl"));
    }

    #[test]
    fn every_word_must_match() {
        // Two words narrow rather than widen, which is what makes searching
        // "skew vref" useful when both tabs carry a VREF block.
        assert!(haystack_matches("skew vref", "skew vref dq vrefup"));
        assert!(!haystack_matches("skew vref", "misc features row hammer"));
        assert!(!haystack_matches("skew vref", "timings primary tcl"));
    }

    #[test]
    fn the_tab_name_is_searchable() {
        assert!(haystack_matches("imc", "imc power down idle length"));
    }

    #[test]
    fn matching_ignores_case() {
        assert!(haystack_matches("vref", "skew vref ca vref"));
        assert!(haystack_matches("CA", &"skew vref ca vref".to_lowercase()));
    }

    #[test]
    fn the_window_filters_and_counts() {
        let rows = vec![
            entry("tCL", "38"),
            Entry::new("Training", "Skew VREF", "DQ VrefUp", false, || Ok(Reading::single("1"))).with_label("DQ Vref Up"),
            entry("tRCD", "50"),
        ];
        let mut window = AdvancedWindow::new(rows, popouts::channel_labels("A1", "B1"), Palette::new(false));
        assert_eq!(window.count(), "3 rows");
        window.set_search("  SKEW vref ");
        assert_eq!(window.visible(), &[1]);
        assert_eq!(window.count(), "1 of 3 rows");
        // The label a tab shows is searchable as well as the row's name.
        window.set_search("vref up");
        assert_eq!(window.visible(), &[1]);
        window.set_search("");
        assert_eq!(window.visible(), &[0, 1, 2]);
    }

    // -- value height (test_advanced_window.py ValueHeightTest) -----------

    #[test]
    fn a_value_inside_the_column_stays_one_line() {
        // The board model at its real rendered width. This is the case that
        // regressed: it fits, and used to be given two lines anyway.
        assert_eq!(value_height(Some(224.0)), ROW_HEIGHT);
        assert_eq!(value_height(Some(VALUE_WRAP)), ROW_HEIGHT);
    }

    #[test]
    fn a_value_wider_than_the_column_gets_a_second_line() {
        // The OS string, which genuinely wraps.
        assert_eq!(value_height(Some(427.0)), ROW_HEIGHT * 2.0);
        assert_eq!(value_height(Some(VALUE_WRAP + 1.0)), ROW_HEIGHT * 2.0);
    }

    #[test]
    fn no_font_means_the_default_height() {
        assert_eq!(value_height(None), ROW_HEIGHT);
    }

    // -- what a row shows --------------------------------------------------

    #[test]
    fn a_reading_fills_its_columns() {
        assert_eq!(shown_text(Ok(Reading::pair("40", "38"))), ("40".into(), "38".into()));
        assert_eq!(shown_text(Ok(Reading::Pair(None, Some("x".into())))), (String::new(), "x".into()));
        assert_eq!(shown_text(Ok(Reading::single("8000 MT/s"))), ("8000 MT/s".into(), String::new()));
        assert_eq!(shown_text(Ok(Reading::Single(None))), (String::new(), String::new()));
        assert_eq!(shown_text(Err("boom".into())), ("N/A".into(), String::new()));
    }

    #[test]
    fn the_band_alternates_from_the_top_of_the_list() {
        let mut palette = Palette::new(false);
        palette.band = Color32::RED;
        assert_eq!(band(&palette, 0), palette.bg);
        assert_eq!(band(&palette, 1), Color32::RED);
        assert_eq!(band(&palette, 2), palette.bg);
    }

    #[test]
    fn dropping_the_window_stops_its_reader() {
        let reads = Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let counted = reads.clone();
        let row = Entry::new("Timings", "Primary", "tCL", false, move || {
            counted.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
            Ok(Reading::single("38"))
        });
        let mut window = AdvancedWindow::new(vec![row], popouts::channel_labels("A1", "B1"), Palette::new(false));
        window.refresh = Duration::from_millis(5);
        window.start_worker(&egui::Context::default());
        std::thread::sleep(Duration::from_millis(60));
        assert!(reads.load(std::sync::atomic::Ordering::SeqCst) > 1);
        drop(window);
        std::thread::sleep(Duration::from_millis(30));
        let after = reads.load(std::sync::atomic::Ordering::SeqCst);
        std::thread::sleep(Duration::from_millis(60));
        assert_eq!(reads.load(std::sync::atomic::Ordering::SeqCst), after);
    }
}
